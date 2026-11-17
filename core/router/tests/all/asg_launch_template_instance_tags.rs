// SPDX-License-Identifier: CC0-1.0

//! What an Auto Scaling group's instance carries BECAUSE it was launched from a launch
//! template — through the whole mounted service set, because the rule only holds if Amazon
//! EC2 Auto Scaling and Amazon EC2 agree over one engine: the group makes the launch, and
//! the instance (and its tags) is Amazon EC2's own record, read back through
//! `DescribeInstances` the way a client reads it.
//!
//! The RULE under test: an instance a group launches from a launch template carries, beside
//! the group's tags whose stored `PropagateAtLaunch` is true ("determines whether the tag is
//! added to new instances as they are launched in the group", `Tag$PropagateAtLaunch`) and
//! the reserved tag naming the group, (a) the instance tags the RESOLVED version's
//! `TagSpecifications` declare — "the tags to apply to the resources that are created during
//! instance launch" (`RequestLaunchTemplateData$TagSpecifications`), each entry tagging the
//! resources of its own `ResourceType`, so only an `instance` entry reaches the instance —
//! and (b) the two reserved tags naming the launch template and the version number the launch
//! resolved, `aws:ec2launchtemplate:id` and `aws:ec2launchtemplate:version`. The version
//! recorded is the NUMBER the group's selector resolved to at the instant of the launch, not
//! the `$Latest`/`$Default` word the group may float on ("the version number, $Latest, or
//! $Default … If the value is $Latest, Amazon EC2 Auto Scaling selects the latest version of
//! the launch template when launching instances",
//! `autoscaling#LaunchTemplateSpecification$Version`).
//!
//! The rows span what the rule generalises over: HOW the group names the version (a pinned
//! number, `$Latest` over a template that has gained a second version, and no version at all
//! — which is `$Default`, read after the default version was moved), WHAT the resolved
//! version declares (instance tags, tags for another resource type that must not reach the
//! instance, no tag specifications at all), and HOW the group's own tags interact with them
//! (a propagating tag, a non-propagating one, and a propagating tag whose key the template
//! also declares).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The AMI, instance type and Availability Zone every launch here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const ZONE: &str = "us-east-1a";

/// The reserved tag keys the rule is about, spelled out here rather than imported so a wrong
/// constant in the implementation cannot make this test pass.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";
const TEMPLATE_ID_TAG: &str = "aws:ec2launchtemplate:id";
const TEMPLATE_VERSION_TAG: &str = "aws:ec2launchtemplate:version";

/// How long a leg waits for the instance its group reports to resolve in Amazon EC2, and how
/// often it looks. The deadline is generous and only here so a failure is a failure rather
/// than a hang: the test waits for the CONDITION, never for a fixed time.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `LaunchTemplateData.<member>` parameter of a launch-template create.
fn data(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One subnet of one VPC, created through Amazon EC2 itself so the id the group places into
/// is one EC2 minted.
async fn seed_subnet(router: &MultiServiceRouter) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.0.0.0/16")]).await,
        "vpcId",
    );
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc),
                p("CidrBlock", "10.0.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    )
}

/// One launch-template version's `TagSpecifications`, as the parameters a create carries:
/// `entries` is `(resource type, key, value)`, one entry per specification.
fn tag_specifications(entries: &[(&str, &str, &str)]) -> Vec<(String, String)> {
    let mut params = Vec::new();
    for (position, (resource_type, key, value)) in entries.iter().enumerate() {
        let index = position + 1;
        params.push(data(
            &format!("TagSpecification.{index}.ResourceType"),
            *resource_type,
        ));
        params.push(data(&format!("TagSpecification.{index}.Tag.1.Key"), *key));
        params.push(data(&format!("TagSpecification.{index}.Tag.1.Value"), *value));
    }
    params
}

/// Create a launch template whose first version carries `version_data`, and answer its id.
async fn create_template(
    router: &MultiServiceRouter,
    name: &str,
    version_data: &[(String, String)],
) -> String {
    let mut params = vec![
        p("LaunchTemplateName", name),
        data("ImageId", IMAGE_ID),
        data("InstanceType", INSTANCE_TYPE),
    ];
    params.extend_from_slice(version_data);
    element(
        &ec2(router, "CreateLaunchTemplate", &params).await,
        "launchTemplateId",
    )
}

/// Add a version carrying `version_data` to `launch_template_id`, and answer its number as
/// Amazon EC2 reports it.
async fn create_version(
    router: &MultiServiceRouter,
    launch_template_id: &str,
    version_data: &[(String, String)],
) -> String {
    let mut params = vec![
        p("LaunchTemplateId", launch_template_id),
        data("ImageId", IMAGE_ID),
        data("InstanceType", INSTANCE_TYPE),
    ];
    params.extend_from_slice(version_data);
    element(
        &ec2(router, "CreateLaunchTemplateVersion", &params).await,
        "versionNumber",
    )
}

/// One tag the group under test carries: its key, its value, and whether it propagates.
struct GroupTag {
    key: &'static str,
    value: &'static str,
    propagates: bool,
}

/// Create a group of one instance launching from `launch_template_id`, naming the version
/// `version` when it names one, and answer the instance id it launched.
async fn given_the_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &str,
    launch_template_id: &str,
    version: Option<&str>,
    tags: &[GroupTag],
) -> String {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "1"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("LaunchTemplate.LaunchTemplateId", launch_template_id),
        p("VPCZoneIdentifier", subnet),
    ];
    // A group that names NO version is the `$Default` case: the member is left off the
    // request entirely rather than sent with the word in it.
    if let Some(version) = version {
        params.push(p("LaunchTemplate.Version", version));
    }
    for (position, tag) in tags.iter().enumerate() {
        let index = position + 1;
        params.push(p(&format!("Tags.member.{index}.Key"), tag.key));
        params.push(p(&format!("Tags.member.{index}.Value"), tag.value));
        params.push(p(
            &format!("Tags.member.{index}.PropagateAtLaunch"),
            if tag.propagates { "true" } else { "false" },
        ));
    }
    asg(router, "CreateAutoScalingGroup", &params).await;

    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    let instances = elements(&body, "InstanceId");
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: a group of one desired instance reports one instance, got \
         {instances:?} in {body}"
    );
    instances
        .into_iter()
        .next()
        .expect("the group reports its one instance")
}

/// The tags Amazon EC2 holds for `instance_id`, polled for until the instance resolves.
///
/// Read through `DescribeInstances`, which is where a client reads an instance's tags, and
/// paired inside each `<item>` of the answer so a tag never takes another's value.
async fn instance_tags(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = send(
            router,
            "ec2",
            form(
                "DescribeInstances",
                EC2_API_VERSION,
                &[p("InstanceId.1", instance_id)],
            ),
        )
        .await;
        if status == 200 && body.contains(instance_id) {
            return items_of(&body)
                .into_iter()
                .filter_map(|item| {
                    let key = elements(item, "key").into_iter().next()?;
                    let value = elements(item, "value").into_iter().next()?;
                    Some((key, value))
                })
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2 never resolved {instance_id}, which the group reports \
             as its instance: last answer {status}: {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The map an expectation is written as.
fn expected(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// One case: the group, the launch template and version it launches from, the group's own
/// tags, and the tags the instance must carry beside the two the loop adds (the group's name
/// and the template's id).
struct Row<'a> {
    case: &'static str,
    group: &'static str,
    launch_template_id: &'a String,
    /// The version the group names, or `None` for a group that names none.
    version: Option<&'static str>,
    tags: Vec<GroupTag>,
    carried: Vec<(&'static str, &'static str)>,
}

#[tokio::test]
async fn an_instance_a_group_launched_names_its_launch_template_version_and_its_tags() {
    let router = router();
    let subnet = seed_subnet(&router).await;

    // One template with TWO versions: version 1 declares an `instance` tag and a `volume`
    // tag (which tags a resource that is not the instance), version 2 declares a different
    // `instance` tag. The group's selector decides which of them an instance carries.
    let versioned = create_template(
        &router,
        "asg-lt-tags-versioned",
        &tag_specifications(&[("instance", "Stage", "v1"), ("volume", "Disk", "root")]),
    )
    .await;
    let second = create_version(
        &router,
        &versioned,
        &tag_specifications(&[("instance", "Stage", "v2")]),
    )
    .await;
    assert_eq!(
        second, "2",
        "the second version of a template is version 2, got {second:?}"
    );

    // A template whose default version is MOVED to its second one, so a group that names no
    // version at all (`$Default`) resolves to a number that is not the first.
    let defaulted = create_template(
        &router,
        "asg-lt-tags-defaulted",
        &tag_specifications(&[("instance", "Stage", "default-v1")]),
    )
    .await;
    create_version(
        &router,
        &defaulted,
        &tag_specifications(&[("instance", "Stage", "default-v2")]),
    )
    .await;
    ec2(
        &router,
        "ModifyLaunchTemplate",
        &[
            p("LaunchTemplateId", defaulted.clone()),
            p("SetDefaultVersion", "2"),
        ],
    )
    .await;

    // A template that declares no tag specifications at all: its instances carry only what
    // the group and the service put on them.
    let untagged = create_template(&router, "asg-lt-tags-untagged", &[]).await;

    let rows: Vec<Row<'_>> = vec![
        Row {
            case: "a group pinned to version 1",
            group: "asg-lt-tags-pinned",
            launch_template_id: &versioned,
            version: Some("1"),
            tags: Vec::new(),
            carried: vec![("Stage", "v1"), (TEMPLATE_VERSION_TAG, "1")],
        },
        Row {
            case: "a group on $Latest once a second version exists",
            group: "asg-lt-tags-latest",
            launch_template_id: &versioned,
            version: Some("$Latest"),
            tags: Vec::new(),
            carried: vec![("Stage", "v2"), (TEMPLATE_VERSION_TAG, "2")],
        },
        Row {
            case: "a group naming no version, after the default version was moved",
            group: "asg-lt-tags-default",
            launch_template_id: &defaulted,
            version: None,
            tags: Vec::new(),
            carried: vec![("Stage", "default-v2"), (TEMPLATE_VERSION_TAG, "2")],
        },
        Row {
            case: "a template declaring no tag specifications, and group tags either way",
            group: "asg-lt-tags-none",
            launch_template_id: &untagged,
            version: Some("1"),
            tags: vec![
                GroupTag {
                    key: "Owner",
                    value: "platform",
                    propagates: true,
                },
                GroupTag {
                    key: "Ledger",
                    value: "internal",
                    propagates: false,
                },
            ],
            carried: vec![("Owner", "platform"), (TEMPLATE_VERSION_TAG, "1")],
        },
        Row {
            case: "a propagating group tag whose key the template also declares",
            group: "asg-lt-tags-collision",
            launch_template_id: &versioned,
            version: Some("1"),
            tags: vec![GroupTag {
                key: "Stage",
                value: "from-the-group",
                propagates: true,
            }],
            carried: vec![("Stage", "from-the-group"), (TEMPLATE_VERSION_TAG, "1")],
        },
    ];

    for row in rows {
        let case = row.case;
        let instance = given_the_group(
            &router,
            case,
            row.group,
            &subnet,
            row.launch_template_id,
            row.version,
            &row.tags,
        )
        .await;
        let read = instance_tags(&router, case, &instance).await;

        let mut wanted = expected(&row.carried);
        // The two facts no row spells: the group the instance belongs to, and the template it
        // was launched from — the id EC2 minted for that template, never a value this test
        // composed.
        wanted.insert(GROUP_NAME_TAG.to_string(), row.group.to_string());
        wanted.insert(TEMPLATE_ID_TAG.to_string(), row.launch_template_id.clone());
        assert_eq!(
            read, wanted,
            "case {case:?}: expected the instance {instance} of {} to carry {wanted:?}, got \
             {read:?}",
            row.group
        );
    }
}
