// SPDX-License-Identifier: CC0-1.0

//! The instance type a LAUNCH-TEMPLATE WRITE names that the published catalog does not hold.
//!
//! The rule under test: **a write of a launch template's launch parameters is SERVED when it
//! names an instance type the catalog does not hold — the template or version is created with
//! the type kept exactly as the caller sent it — and the type is reported as a finding of the
//! response's `warning.errorSet` instead.** "The error codes and error messages that are
//! returned for the parameters or parameter combinations that are not valid when a new launch
//! template or new version of a launch template is created" (`ValidationWarning`, the shape
//! both writes report through their `Warning` member) is the channel such a parameter is
//! answered on: a launch template is a set of launch PARAMETERS rather than a launch, so the
//! launch that tries to use the type is what refuses it. The finding carries EC2's own code
//! for a type that does not exist and the sentence it words every answer about supplied types
//! the catalog does not hold with.
//!
//! The rows differ along what the rule generalises over — whether the catalog publishes the
//! value at all, and HOW a value misses it (a family that does not exist, a size the catalog
//! does not carry for a family that does, a family stated with no size, a size stated with no
//! family, a published type in another ASCII case) — and each row first asserts what the
//! service's OWN catalog says about the value, so the table is read against that catalog
//! rather than asserting a copy of it.
//!
//! Both writes are driven for every row, because both answer from one reading of a template's
//! parameters, and the stored parameters are read back through
//! `DescribeLaunchTemplateVersions`: a finding is advisory, so what the caller sent is what
//! the template keeps. The other half of the rule is here too: the LAUNCH that tries to use
//! such a type is what refuses it, so EVERY placement made from the template — a launch, an
//! EC2 Fleet pool and a Spot Fleet pool — is refused naming the value and places nothing.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_PARAMETER_COMBINATION,
    SECURITY_GROUPS_BELONG_TO_THE_NETWORK_INTERFACE,
};

use super::ec2_query::{call, element, error_code, error_message, unescaped};
use super::wire::{child, child_text, document, set_items};

/// The Region every call of this suite is signed for. A launch template carries no Region
/// member, so the Region only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM for the launch parameters every row carries: a template stores
/// its parameters without resolving them, so which image it names is not the subject here.
const SOME_AMI: &str = "ami-0123456789abcdef0";

/// The fleet role a Spot Fleet request names: an ARN of the right form, which is all this
/// suite's subject needs of it.
const FLEET_ROLE: &str = "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role";

/// An AMI of the published catalog a fresh account can already see, read out of the service's
/// own state rather than written here: the fleet placements resolve the image against the
/// catalog, so the rows that drive them cannot be given an invented id.
fn catalog_image() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// A security group id of the right form, for the row that also earns the combination
/// finding.
const SOME_GROUP: &str = "sg-0123456789abcdef0";

/// One finding, as a row asserts it: the code and the message.
type Finding = (String, String);

/// A `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `LaunchTemplateData.<member>` parameter.
fn data(member: &str, value: &str) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// The findings a write's body reports, in the order it reported them — empty for a body that
/// carries no `warning` element at all.
fn findings(case: &str, body: &str) -> Vec<Finding> {
    let parsed = document(case, body);
    let Some(warning) = parsed
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "warning")
    else {
        return Vec::new();
    };
    set_items(warning, "errorSet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "code").unwrap_or_default(),
                unescaped(&child_text(item, "message").unwrap_or_default()),
            )
        })
        .collect()
}

/// The finding an instance type the catalog does not hold earns, worded by the service's own
/// code and sentence for such a type rather than by a copy of either.
fn nonexistent(instance_type: &str) -> Finding {
    (
        ec2::state::INVALID_INSTANCE_TYPE.to_string(),
        ec2::state::nonexistent_instance_types_message(&[instance_type.to_string()]),
    )
}

/// One call that has to be served, as the body it answered.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a launch template's parameters are stored and reported on, so {action} \
         answers 200, got {status} with body {body}"
    );
    body
}

/// The `(versionNumber, defaultVersion, instanceType)` of every version of
/// `launch_template_id`, read back through the template's own reader.
async fn stored_versions(
    router: &MultiServiceRouter,
    case: &str,
    launch_template_id: &str,
) -> Vec<(String, String, Option<String>)> {
    let body = served(
        router,
        case,
        "DescribeLaunchTemplateVersions",
        &[p("LaunchTemplateId", launch_template_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "versionNumber").is_some())
        .map(|node| {
            (
                child_text(node, "versionNumber").unwrap_or_default(),
                child_text(node, "defaultVersion").unwrap_or_default(),
                child(node, "launchTemplateData")
                    .and_then(|stored| child_text(stored, "instanceType")),
            )
        })
        .collect()
}

/// RULE: both launch-template writes serve a request naming an instance type the catalog does
/// not hold, keep the type and report it as a finding — while a published type earns no
/// finding at all.
#[tokio::test]
async fn a_launch_template_write_reports_an_instance_type_the_catalog_does_not_hold() {
    // (case, the InstanceType the write names, whether the catalog publishes it)
    let rows: &[(&str, &str, bool)] = &[
        ("a current-generation type", "t3.micro", true),
        ("a previous-generation type", "m1.small", true),
        ("a bare-metal size", "c5.metal", true),
        ("a family that does not exist", "sim2real.invalid-type", false),
        ("a size the family does not offer", "t3.enormous", false),
        ("a family stated with no size", "m5", false),
        ("a size stated with no family", "large", false),
        ("a published type in upper case", "T3.MICRO", false),
    ];

    for (case, instance_type, published) in rows {
        // The row is read against the service's own catalog first, so a table that drifted
        // from the model fails here rather than asserting the wrong half of the rule.
        let modelled = ec2::state::instance_type_is_modelled(instance_type);
        assert_eq!(
            modelled, *published,
            "case {case:?}: expected the catalog to publish {instance_type:?} = {published}, \
             got {modelled}"
        );
        let expected: Vec<Finding> = if *published {
            Vec::new()
        } else {
            vec![nonexistent(instance_type)]
        };

        let (router, _engine) = super::ec2_query::emulator();
        let parameters = vec![data("ImageId", SOME_AMI), data("InstanceType", instance_type)];

        // The template, created with those parameters.
        let mut create = vec![p("LaunchTemplateName", "probe")];
        create.extend(parameters.clone());
        let body = served(&router, case, "CreateLaunchTemplate", &create).await;
        let reported = findings(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the findings {expected:?} from CreateLaunchTemplate, got \
             {reported:?} in {body}"
        );
        let launch_template_id = element(&body, "launchTemplateId").unwrap_or_else(|| {
            panic!("case {case:?}: the template is created whatever its parameters earn: {body}")
        });
        let counters = (
            element(&body, "defaultVersionNumber"),
            element(&body, "latestVersionNumber"),
        );
        assert_eq!(
            counters,
            (Some("1".to_string()), Some("1".to_string())),
            "case {case:?}: the create reports the one version it minted, got {counters:?} in \
             {body}"
        );

        // A new version of the same template, from the same reading of the same parameters.
        let mut version = vec![p("LaunchTemplateId", &launch_template_id)];
        version.extend(parameters.clone());
        let body = served(&router, case, "CreateLaunchTemplateVersion", &version).await;
        let reported = findings(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the findings {expected:?} from \
             CreateLaunchTemplateVersion, got {reported:?} in {body}"
        );
        let numbered = (
            element(&body, "versionNumber"),
            element(&body, "defaultVersion"),
        );
        assert_eq!(
            numbered,
            (Some("2".to_string()), Some("false".to_string())),
            "case {case:?}: the second version is numbered one past the first and is not the \
             default, got {numbered:?} in {body}"
        );

        // The parameters both writes STORED, read back through the template's own reader: a
        // finding is advisory, so the type the caller sent is the type each version keeps.
        let stored = stored_versions(&router, case, &launch_template_id).await;
        let kept: Vec<Option<String>> = stored
            .iter()
            .map(|(_, _, instance_type)| instance_type.clone())
            .collect();
        let expected_kept = vec![
            Some((*instance_type).to_string()),
            Some((*instance_type).to_string()),
        ];
        assert_eq!(
            kept, expected_kept,
            "case {case:?}: expected both stored versions to keep {instance_type:?}, got \
             {kept:?} in {stored:?}"
        );
    }
}

/// An unpublished instance type is ONE finding beside the others a template's parameters
/// earn: a write whose parameters are at fault in two ways reports both, the value of the one
/// parameter before the combination of two.
#[tokio::test]
async fn an_unpublished_instance_type_is_reported_beside_the_other_findings() {
    let case = "an unpublished type beside groups outside the network interface";
    let instance_type = "sim2real.invalid-type";
    let (router, _engine) = super::ec2_query::emulator();
    let parameters = vec![
        p("LaunchTemplateName", "probe"),
        data("ImageId", SOME_AMI),
        data("InstanceType", instance_type),
        data("NetworkInterface.1.DeviceIndex", "0"),
        data("NetworkInterface.1.SecurityGroupId.1", SOME_GROUP),
        data("SecurityGroupId.1", SOME_GROUP),
    ];
    let body = served(&router, case, "CreateLaunchTemplate", &parameters).await;
    let expected = vec![
        nonexistent(instance_type),
        (
            INVALID_PARAMETER_COMBINATION.to_string(),
            SECURITY_GROUPS_BELONG_TO_THE_NETWORK_INTERFACE.to_string(),
        ),
    ];
    let reported = findings(case, &body);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected both findings {expected:?}, got {reported:?} in {body}"
    );
}

/// A template whose FIRST version names a published type and whose second names one the
/// catalog does not hold: each version is reported on its own parameters, so the finding
/// belongs to the version that earned it and the stored types are not mixed up.
#[tokio::test]
async fn each_version_is_reported_on_its_own_instance_type() {
    let case = "one published version, then an unpublished one";
    let unpublished = "t3.enormous";
    let (router, _engine) = super::ec2_query::emulator();

    let body = served(
        &router,
        case,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "probe"),
            data("ImageId", SOME_AMI),
            data("InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        findings(case, &body),
        Vec::<Finding>::new(),
        "case {case:?}: a published type earns no finding, got {body}"
    );
    let launch_template_id = element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reports the id it minted: {body}"));

    let body = served(
        &router,
        case,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &launch_template_id),
            data("ImageId", SOME_AMI),
            data("InstanceType", unpublished),
        ],
    )
    .await;
    let reported = findings(case, &body);
    assert_eq!(
        reported,
        vec![nonexistent(unpublished)],
        "case {case:?}: the new version's own type is the finding, got {reported:?} in {body}"
    );

    let stored = stored_versions(&router, case, &launch_template_id).await;
    let kept: Vec<(String, Option<String>)> = stored
        .iter()
        .map(|(number, _, instance_type)| (number.clone(), instance_type.clone()))
        .collect();
    let expected = vec![
        ("2".to_string(), Some(unpublished.to_string())),
        ("1".to_string(), Some("t3.micro".to_string())),
    ];
    assert_eq!(
        kept, expected,
        "case {case:?}: expected each version to keep its own type {expected:?}, got {kept:?}"
    );
}

/// The instance type as the ONLY launch parameter the request carries: a write whose data
/// names nothing else is still served, keeps the type and reports the finding — the parameter
/// the catalog does not hold is the whole of the launch parameters.
#[tokio::test]
async fn an_unpublished_instance_type_can_be_the_only_launch_parameter() {
    let case = "the unpublished type as the only launch parameter";
    let unpublished = "t3.enormous";
    let (router, _engine) = super::ec2_query::emulator();

    let body = served(
        &router,
        case,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "probe"),
            data("InstanceType", unpublished),
        ],
    )
    .await;
    let reported = findings(case, &body);
    assert_eq!(
        reported,
        vec![nonexistent(unpublished)],
        "case {case:?}: expected the finding for {unpublished:?}, got {reported:?} in {body}"
    );
    let launch_template_id = element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reports the id it minted: {body}"));

    // A version whose own data names only that type, inheriting the rest from version 1.
    let body = served(
        &router,
        case,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("SourceVersion", "1"),
            data("InstanceType", unpublished),
        ],
    )
    .await;
    let reported = findings(case, &body);
    assert_eq!(
        reported,
        vec![nonexistent(unpublished)],
        "case {case:?}: expected the finding for {unpublished:?} from the version, got \
         {reported:?} in {body}"
    );
    let stored = stored_versions(&router, case, &launch_template_id).await;
    let kept: Vec<Option<String>> = stored
        .iter()
        .map(|(_, _, instance_type)| instance_type.clone())
        .collect();
    assert_eq!(
        kept,
        vec![
            Some(unpublished.to_string()),
            Some(unpublished.to_string())
        ],
        "case {case:?}: expected both versions to keep {unpublished:?}, got {kept:?}"
    );
}

/// The OTHER half of the rule: the placement that tries to use a parameter no launch can use
/// is what refuses it. A template keeps an instance type the catalog does not hold, and EVERY
/// operation that places instances from that template — a launch, an EC2 Fleet pool and a Spot
/// Fleet pool — is refused naming the value, with nothing placed.
///
/// One row per placement path, because each resolves the effective type from the stored
/// parameters itself; all three answer the service's one code and one sentence for a type the
/// catalog does not hold.
#[tokio::test]
async fn every_placement_from_a_template_holding_an_unpublished_type_is_refused() {
    /// One row: the case, the action, and the parameters that place from `template_id`.
    type Row = (&'static str, &'static str, fn(&str) -> Vec<(String, String)>);

    let rows: &[Row] = &[
        ("a launch from the template", "RunInstances", |template_id| {
            vec![
                p("LaunchTemplate.LaunchTemplateId", template_id),
                p("MinCount", "1"),
                p("MaxCount", "1"),
            ]
        }),
        ("one pool of an EC2 Fleet", "CreateFleet", |template_id| {
            vec![
                p(
                    "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
                    template_id,
                ),
                p("Type", "instant"),
                p("TargetCapacitySpecification.TotalTargetCapacity", "1"),
                p("TargetCapacitySpecification.OnDemandTargetCapacity", "1"),
            ]
        }),
        (
            "one pool of a Spot Fleet request",
            "RequestSpotFleet",
            |template_id| {
                vec![
                    p("SpotFleetRequestConfig.IamFleetRole", FLEET_ROLE),
                    p("SpotFleetRequestConfig.TargetCapacity", "1"),
                    p(
                        "SpotFleetRequestConfig.LaunchTemplateConfigs.1.\
                         LaunchTemplateSpecification.LaunchTemplateId",
                        template_id,
                    ),
                ]
            },
        ),
    ];

    let unpublished = "sim2real.invalid-type";
    for (case, action, parameters) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        // The template keeps the type: the write is served, as the table above pins.
        let body = served(
            &router,
            case,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", "probe"),
                data("ImageId", &catalog_image()),
                data("InstanceType", unpublished),
            ],
        )
        .await;
        let launch_template_id = element(&body, "launchTemplateId").unwrap_or_else(|| {
            panic!("case {case:?}: the create reports the id it minted: {body}")
        });

        let (status, body) = call(&router, REGION, action, &parameters(&launch_template_id)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action} placing {unpublished:?}, got \
             {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code,
            ec2::state::INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the code {:?}, got {code:?} in {body}",
            ec2::state::INVALID_PARAMETER_VALUE
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            ec2::state::instance_type_refusal_message(unpublished),
            "case {case:?}: expected the sentence {:?}, got {message:?}",
            ec2::state::instance_type_refusal_message(unpublished)
        );
        // Nothing was placed: the account holds no instance of a type that does not exist,
        // and the refused call left no fleet behind either.
        for (reader, params) in [
            ("DescribeInstances", Vec::new()),
            ("DescribeFleets", Vec::new()),
            ("DescribeSpotFleetRequests", Vec::new()),
        ] {
            let (status, described) = call(&router, REGION, reader, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {reader} must answer 200, got {status} with body {described}"
            );
            assert!(
                !described.contains(unpublished),
                "case {case:?}: expected {reader} to report nothing of {unpublished:?}, got \
                 {described}"
            );
        }
    }
}

/// A version that INHERITS from a source version whose type the catalog does not hold is
/// reported on the type it stores: the finding follows the stored parameters, not the request.
#[tokio::test]
async fn an_inherited_instance_type_the_catalog_does_not_hold_is_reported_too() {
    let case = "a version inheriting an unpublished type";
    let unpublished = "m5";
    let (router, _engine) = super::ec2_query::emulator();

    let body = served(
        &router,
        case,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "probe"),
            data("ImageId", SOME_AMI),
            data("InstanceType", unpublished),
        ],
    )
    .await;
    let launch_template_id = element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reports the id it minted: {body}"));

    // The new version names no type of its own at all, inheriting the first version's.
    let body = served(
        &router,
        case,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &launch_template_id),
            p("SourceVersion", "1"),
            data("KeyName", "some-key"),
        ],
    )
    .await;
    let reported = findings(case, &body);
    assert_eq!(
        reported,
        vec![nonexistent(unpublished)],
        "case {case:?}: the stored parameters carry the inherited type, so the finding is \
         reported for it, got {reported:?} in {body}"
    );
    let parsed = document(case, &body);
    let stored = child(parsed.root_element(), "launchTemplateVersion")
        .and_then(|version| child(version, "launchTemplateData"))
        .and_then(|stored| child_text(stored, "instanceType"));
    assert_eq!(
        stored.as_deref(),
        Some(unpublished),
        "case {case:?}: expected the inherited type {unpublished:?} to be stored, got \
         {stored:?} in {body}"
    );
}
