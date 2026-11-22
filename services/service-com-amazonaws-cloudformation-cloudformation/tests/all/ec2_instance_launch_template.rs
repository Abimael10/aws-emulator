// SPDX-License-Identifier: CC0-1.0

//! An `AWS::EC2::Instance` that declares a `LaunchTemplate` is LAUNCHED FROM that template.
//!
//! RULE: every property of a declared resource travels to the member of the owning service's
//! create call that carries it, so an `AWS::EC2::Instance` whose declaration names a launch
//! template is launched with `RunInstances`'s own `LaunchTemplate` — the template named by id
//! or by name, at the version the declaration asks for — and Amazon EC2 merges the template's
//! launch parameters with the ones the declaration states itself ("any additional parameters
//! that you specify for the new instance overwrite the corresponding parameters included in
//! the launch template", `RunInstancesRequest$LaunchTemplate`). Nothing about the template is
//! read by CloudFormation: the AMI, the instance type, the instance profile and the security
//! groups of the launched instance are the version's, the version the launch resolved is the
//! one Amazon EC2 resolved, and a template or version that service holds none of fails the
//! RESOURCE with that service's own refusal.
//!
//! The rows differ along what the rule generalises over — which member of the specification
//! names the template (its id or its name), and which version the declaration asks for (a
//! number, either alias, or no version at all) — plus the overlay direction: a property the
//! declaration states ITSELF beside the template wins over the template's value for it.
//!
//! Two services have to agree for any of it, both mounted on the one engine this crate's
//! harness builds: CloudFormation makes the launch and Amazon EC2 serves the template, the
//! launch and the reads. The launch template of the table below is created in Amazon EC2
//! itself so that a row can ask for a version the stack did not create; the second case
//! deploys the `AWS::EC2::LaunchTemplate` and the instance in ONE stack, which is the shape a
//! CDK estate synthesises — the instance's specification is a `Ref` to the template resource
//! and an `Fn::GetAtt` of its latest version.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The shared waiter for a failed resource's own event reason lives on the harness itself:
// `stack_lifecycle` re-exports the readers below but not that one.
use crate::harness::failed_resource_reason;
use crate::stack_lifecycle::{
    create_stack, ec2, ec2_tag_value, iam, is_service_assigned_id, resource_event_reason,
    router_with_engine, settled_stack, xml_blocks, xml_text, xml_texts, REGION,
};

/// The logical id of the instance every template here declares.
const INSTANCE: &str = "TheInstance";

/// The logical id of the launch template the one-stack case declares beside it.
const TEMPLATE: &str = "TheTemplate";

/// The two tags Amazon EC2 itself puts on an instance a LAUNCH TEMPLATE launched — the
/// template's id and the version number the launch resolved
/// (`crate::state::LAUNCH_TEMPLATE_ID_TAG` of that service's own state module, as
/// `DescribeInstances` reports them). They are what says the launch really read the template
/// rather than the declaration happening to state the same values.
const LAUNCH_TEMPLATE_ID_TAG: &str = "aws:ec2launchtemplate:id";
const LAUNCH_TEMPLATE_VERSION_TAG: &str = "aws:ec2launchtemplate:version";

/// The instance profile the launch template names. A launch GIVES its instances that
/// profile, so Amazon EC2 holds the launch to one the account's IAM records really carry —
/// the profile is created through IAM's own wire below ([`hold_instance_profile`]), and what
/// the rows assert is that the template's profile reached the instance.
const PROFILE_NAME: &str = "the-estate-instance-profile";

/// The instance types the two versions of the table's launch template carry, and the one a row
/// states on the instance itself to overlay the template's.
const VERSION_ONE_TYPE: &str = "t3.micro";
const VERSION_TWO_TYPE: &str = "t3.small";
const DECLARED_TYPE: &str = "t3.large";

/// What one row of the table declares and what the launched instance must then report.
struct Row {
    /// What the case is, named in every assertion of it.
    case: &'static str,
    /// Whether the specification names the template by its ID (rather than by its name).
    by_id: bool,
    /// The `Version` the specification asks for, or `None` for a declaration that states none.
    version: Option<&'static str>,
    /// The `InstanceType` the instance states ITSELF, beside the template.
    declared_type: Option<&'static str>,
    /// The version number Amazon EC2 must resolve the specification to.
    resolved_version: &'static str,
    /// The instance type the launched instance must report.
    expected_type: &'static str,
}

/// Every way a declaration names the template and its version, and the instance each one must
/// produce.
///
/// `$Latest` and `$Default` are the two aliases the member publishes ("a value of `$Latest`
/// uses the latest version of the launch template. A value of `$Default` uses the default
/// version", `LaunchTemplateSpecification$Version`), and a declaration that states no version
/// at all gets the same default ("Default: The default version of the launch template") — the
/// first version here, since nothing moved the default off it.
const ROWS: &[Row] = &[
    Row {
        case: "by id, at the first version",
        by_id: true,
        version: Some("1"),
        declared_type: None,
        resolved_version: "1",
        expected_type: VERSION_ONE_TYPE,
    },
    Row {
        case: "by name, at the second version",
        by_id: false,
        version: Some("2"),
        declared_type: None,
        resolved_version: "2",
        expected_type: VERSION_TWO_TYPE,
    },
    Row {
        case: "by id, at the $Latest alias",
        by_id: true,
        version: Some("$Latest"),
        declared_type: None,
        resolved_version: "2",
        expected_type: VERSION_TWO_TYPE,
    },
    Row {
        case: "by name, at the $Default alias",
        by_id: false,
        version: Some("$Default"),
        declared_type: None,
        resolved_version: "1",
        expected_type: VERSION_ONE_TYPE,
    },
    Row {
        case: "by id, with no version stated",
        by_id: true,
        version: None,
        declared_type: None,
        resolved_version: "1",
        expected_type: VERSION_ONE_TYPE,
    },
    Row {
        case: "by id, with the instance type stated on the resource",
        by_id: true,
        version: Some("2"),
        declared_type: Some(DECLARED_TYPE),
        resolved_version: "2",
        expected_type: DECLARED_TYPE,
    },
];

/// The launch template the table's rows launch from: two versions, each carrying its own AMI
/// and instance type beside the security group and instance profile both hold.
struct Template {
    launch_template_id: String,
    name: String,
    /// The AMI of each version, by version number.
    first_image: String,
    second_image: String,
    group_id: String,
}

/// One EC2 call that must succeed, answered with its body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// Make IAM hold the instance profile the templates below name, through the service that OWNS
/// instance profiles: a launch names the profile its instances run as, and Amazon EC2 refuses
/// a launch naming one the account holds none of.
async fn hold_instance_profile(router: &MultiServiceRouter) {
    let (status, body) = iam(
        router,
        "CreateInstanceProfile",
        &[("InstanceProfileName", PROFILE_NAME)],
    )
    .await;
    assert_eq!(
        status, 200,
        "IAM must hold the instance profile the templates name, got {status} with body {body}"
    );
}

/// Two AMIs of the Region, read out of Amazon EC2 itself — a template cannot invent an image
/// id, and the two versions carry different ones so a row says WHICH version the launch read.
async fn two_images(router: &MultiServiceRouter) -> (String, String) {
    let body = ec2_ok(router, "the estate", "DescribeImages", &[]).await;
    let images = xml_texts(&body, "imageId");
    assert!(
        images.len() >= 2,
        "the Region must publish at least two images, got {images:?}"
    );
    (images[0].clone(), images[1].clone())
}

/// Create the two-version launch template in Amazon EC2, with the security group and instance
/// profile both versions carry.
async fn launch_template(router: &MultiServiceRouter) -> Template {
    let case = "the estate";
    hold_instance_profile(router).await;
    let (first_image, second_image) = two_images(router).await;
    let created = ec2_ok(
        router,
        case,
        "CreateSecurityGroup",
        &[
            ("GroupName", "the-estate-group"),
            ("GroupDescription", "the group the launch template names"),
        ],
    )
    .await;
    let group_id = xml_text(&created, "groupId").expect("the security group's own id");
    let name = "the-estate-launch-template".to_string();
    let created = ec2_ok(
        router,
        case,
        "CreateLaunchTemplate",
        &version_parameters(
            &[("LaunchTemplateName", name.as_str())],
            &first_image,
            VERSION_ONE_TYPE,
            &group_id,
        ),
    )
    .await;
    let launch_template_id =
        xml_text(&created, "launchTemplateId").expect("the launch template's own id");
    let added = ec2_ok(
        router,
        case,
        "CreateLaunchTemplateVersion",
        &version_parameters(
            &[("LaunchTemplateId", launch_template_id.as_str())],
            &second_image,
            VERSION_TWO_TYPE,
            &group_id,
        ),
    )
    .await;
    let version_number = xml_text(&added, "versionNumber").unwrap_or_default();
    assert_eq!(
        version_number, "2",
        "the second version of the template must be numbered 2, got {version_number:?}"
    );
    Template {
        launch_template_id,
        name,
        first_image,
        second_image,
        group_id,
    }
}

/// The `LaunchTemplateData.<member>` parameters one version carries, after the parameters that
/// name the template itself.
fn version_parameters<'a>(
    naming: &[(&'a str, &'a str)],
    image_id: &'a str,
    instance_type: &'a str,
    group_id: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut parameters = naming.to_vec();
    parameters.push(("LaunchTemplateData.ImageId", image_id));
    parameters.push(("LaunchTemplateData.InstanceType", instance_type));
    parameters.push(("LaunchTemplateData.SecurityGroupId.1", group_id));
    parameters.push(("LaunchTemplateData.IamInstanceProfile.Name", PROFILE_NAME));
    parameters
}

/// The template body one row deploys: the instance, with the `LaunchTemplate` specification
/// the row states and whatever it states beside it.
fn instance_template(specification: Value, declared_type: Option<&str>) -> String {
    let mut properties = json!({ "LaunchTemplate": specification });
    if let Some(instance_type) = declared_type {
        properties["InstanceType"] = json!(instance_type);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            INSTANCE: { "Type": "AWS::EC2::Instance", "Properties": properties }
        },
        "Outputs": { "InstanceId": { "Value": { "Ref": INSTANCE } } }
    })
    .to_string()
}

/// Deploy a template and answer the settled stack's `DescribeStacks` body.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} with body {body}"
    );
    settled_stack(router, case, stack).await
}

/// What Amazon EC2 reports about one instance, as the one instance's own block of the answer.
async fn described_instance(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    xml_blocks(&body, "instancesSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected Amazon EC2 to report {instance_id}"))
}

/// The instance id a settled stack published through its one output.
fn published_instance(case: &str, described: &str) -> String {
    let published = xml_texts(described, "OutputValue")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert!(
        is_service_assigned_id(&published, "i"),
        "case {case:?}: expected the stack to publish an Amazon EC2 instance id, got \
         {published:?} in {described}"
    );
    published
}

/// Every way a declaration can name a launch template and a version of it launches the
/// instance FROM that version: the instance Amazon EC2 holds carries the version's AMI,
/// instance type, security group and instance profile, and the two tags that service puts on
/// an instance launched from a template name the template and the version the launch resolved.
#[tokio::test]
async fn an_instance_declared_from_a_launch_template_is_launched_from_that_version() {
    let (router, _engine) = router_with_engine();
    let template = launch_template(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let mut specification = if row.by_id {
            json!({ "LaunchTemplateId": template.launch_template_id })
        } else {
            json!({ "LaunchTemplateName": template.name })
        };
        if let Some(version) = row.version {
            specification["Version"] = json!(version);
        }
        let stack = format!("launch-template-row-{index}");
        let described = deploy(
            &router,
            case,
            &stack,
            &instance_template(specification, row.declared_type),
        )
        .await;
        let status = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {status:?}; the \
             instance's last event said {reason:?}",
            reason = resource_event_reason(&router, &stack, INSTANCE, "CREATE_FAILED").await
        );
        let instance_id = published_instance(case, &described);
        let reported = described_instance(&router, case, &instance_id).await;

        let expected_image = if row.resolved_version == "1" {
            &template.first_image
        } else {
            &template.second_image
        };
        let reported_image = xml_text(&reported, "imageId").unwrap_or_default();
        assert_eq!(
            reported_image, *expected_image,
            "case {case:?}: expected the instance to boot the version's AMI \
             ({expected_image}), got {reported_image:?} in {reported}"
        );
        let reported_type = xml_text(&reported, "instanceType").unwrap_or_default();
        assert_eq!(
            reported_type, row.expected_type,
            "case {case:?}: expected the instance to report instance type {expected:?}, got \
             {reported_type:?} in {reported}",
            expected = row.expected_type
        );
        let reported_groups = xml_texts(&reported, "groupId");
        assert!(
            reported_groups.contains(&template.group_id),
            "case {case:?}: expected the instance in the template's security group ({group}), \
             got {reported_groups:?}",
            group = template.group_id
        );
        let reported_profile = xml_blocks(&reported, "iamInstanceProfile")
            .into_iter()
            .next()
            .unwrap_or_default();
        assert!(
            reported_profile.contains(PROFILE_NAME),
            "case {case:?}: expected the instance to carry the template's instance profile \
             ({PROFILE_NAME}), got {reported_profile:?} in {reported}"
        );
        let held_template = ec2_tag_value(&reported, LAUNCH_TEMPLATE_ID_TAG);
        assert_eq!(
            held_template.as_deref(),
            Some(template.launch_template_id.as_str()),
            "case {case:?}: expected the instance to carry {LAUNCH_TEMPLATE_ID_TAG} naming the \
             template Amazon EC2 launched it from, got {held_template:?}"
        );
        let held_version = ec2_tag_value(&reported, LAUNCH_TEMPLATE_VERSION_TAG);
        assert_eq!(
            held_version.as_deref(),
            Some(row.resolved_version),
            "case {case:?}: expected the launch to resolve version {expected:?}, got \
             {held_version:?}",
            expected = row.resolved_version
        );
    }
}

/// The CDK shape: one stack declaring the launch template AND the instance that launches from
/// it, the specification being a `Ref` to the template resource and an `Fn::GetAtt` of the
/// version it published. The stack completes and the instance Amazon EC2 holds was launched
/// from the template the same stack created.
#[tokio::test]
async fn a_stack_that_declares_both_launches_the_instance_from_its_own_template() {
    let case = "a launch template and an instance in one stack";
    let (router, _engine) = router_with_engine();
    hold_instance_profile(&router).await;
    let (image_id, _other) = two_images(&router).await;
    let body = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            TEMPLATE: {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateName": "the-stacks-own-template",
                    "LaunchTemplateData": {
                        "ImageId": image_id,
                        "InstanceType": VERSION_ONE_TYPE,
                        "IamInstanceProfile": { "Name": PROFILE_NAME },
                    }
                }
            },
            INSTANCE: {
                "Type": "AWS::EC2::Instance",
                "Properties": {
                    "LaunchTemplate": {
                        "LaunchTemplateId": { "Ref": TEMPLATE },
                        "Version": { "Fn::GetAtt": [TEMPLATE, "LatestVersionNumber"] },
                    }
                }
            }
        },
        "Outputs": {
            "InstanceId": { "Value": { "Ref": INSTANCE } },
            "TemplateId": { "Value": { "Ref": TEMPLATE } }
        }
    })
    .to_string();

    let stack = "launch-template-estate";
    let described = deploy(&router, case, stack, &body).await;
    let status = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {status:?}; the \
         instance's last event said {reason:?}",
        reason = resource_event_reason(&router, stack, INSTANCE, "CREATE_FAILED").await
    );
    let published = xml_texts(&described, "OutputValue");
    let instance_id = published
        .iter()
        .find(|value| is_service_assigned_id(value, "i"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish an instance id, got {published:?}")
        })
        .clone();
    let launch_template_id = published
        .iter()
        .find(|value| is_service_assigned_id(value, "lt"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish a template id, got {published:?}")
        })
        .clone();

    let reported = described_instance(&router, case, &instance_id).await;
    let reported_image = xml_text(&reported, "imageId").unwrap_or_default();
    assert_eq!(
        reported_image, image_id,
        "case {case:?}: expected the instance to boot the template's AMI ({image_id}), got \
         {reported_image:?} in {reported}"
    );
    let reported_type = xml_text(&reported, "instanceType").unwrap_or_default();
    assert_eq!(
        reported_type, VERSION_ONE_TYPE,
        "case {case:?}: expected the template's instance type ({VERSION_ONE_TYPE}), got \
         {reported_type:?} in {reported}"
    );
    let held_template = ec2_tag_value(&reported, LAUNCH_TEMPLATE_ID_TAG);
    assert_eq!(
        held_template.as_deref(),
        Some(launch_template_id.as_str()),
        "case {case:?}: expected the instance to be launched from the template the stack \
         created ({launch_template_id}), got {held_template:?}"
    );
}

/// A declaration naming a launch template or a version Amazon EC2 cannot resolve fails THAT
/// resource with that service's own refusal — the launch is made and its answer is the
/// resource's failure, so the reason carries the sentence Amazon EC2 worded and the stack
/// rolls back with no instance left behind.
///
/// The rows differ along WHICH reference cannot be resolved: an id no issue of that service
/// could have produced, an id whose template has since been deleted, and a version number the
/// template never minted.
#[tokio::test]
async fn a_reference_amazon_ec2_cannot_resolve_fails_the_resource_with_its_refusal() {
    /// What a row declares, and the sentence Amazon EC2 must have refused it with. `absent` is
    /// the id of a template the row DELETES before deploying, rather than a literal id.
    struct Refused {
        case: &'static str,
        deleted: bool,
        version: &'static str,
        /// The refusal, with `{id}` standing for the launch template id the row names.
        expected: &'static str,
    }

    let rows: &[Refused] = &[
        Refused {
            case: "an id no issue of that service could have produced",
            deleted: false,
            version: "1",
            expected: "Malformed launch template Id: {id}",
        },
        Refused {
            case: "a version the template never minted",
            deleted: false,
            version: "7",
            expected: "Could not find the specified version 7 for the launch template with \
                       ID {id}.",
        },
        Refused {
            case: "an id whose template has since been deleted",
            deleted: true,
            version: "1",
            expected: "The specified launch template, with template ID {id}, does not exist.",
        },
    ];

    let (router, _engine) = router_with_engine();
    let template = launch_template(&router).await;
    // An id of the long form that this service never issued, which is a different refusal from
    // an issued id whose template is gone.
    const NEVER_ISSUED: &str = "lt-0123456789abcdef0";

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let launch_template_id = if row.expected.starts_with("Malformed") {
            NEVER_ISSUED.to_string()
        } else {
            template.launch_template_id.clone()
        };
        if row.deleted {
            ec2_ok(
                &router,
                case,
                "DeleteLaunchTemplate",
                &[("LaunchTemplateId", launch_template_id.as_str())],
            )
            .await;
        }
        let stack = format!("launch-template-refused-{index}");
        let described = deploy(
            &router,
            case,
            &stack,
            &instance_template(
                json!({ "LaunchTemplateId": launch_template_id, "Version": row.version }),
                None,
            ),
        )
        .await;
        let status = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back, got {status:?} in {described}"
        );
        let reason =
            failed_resource_reason(case, &router, REGION, &stack, INSTANCE, "CREATE_FAILED").await;
        let expected = row.expected.replace("{id}", &launch_template_id);
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the failure to carry Amazon EC2's own refusal \
             ({expected:?}), got {reason:?}"
        );
    }
}

