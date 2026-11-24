// SPDX-License-Identifier: CC0-1.0

//! The PAIR of sentences `CreateAutoScalingGroup` refuses a dangling launch-template
//! reference with, and the create that follows against a template the account holds.
//!
//! RULE under test: Amazon EC2 Auto Scaling resolves a group's launch-template reference
//! through the service that OWNS launch templates, so a reference that does not resolve is
//! refused with HTTP 400 and `ValidationError`, carrying that service's sentence for the HALF
//! of the reference the request stated — `The specified launch template, with template name
//! <name>, does not exist.` for `LaunchTemplateName` and `… with template ID <id>, does not
//! exist.` for `LaunchTemplateId` — whose one variable part is the identifier the request
//! carried, verbatim; no group is created, and the same request naming a template the account
//! DOES hold is served.
//!
//! `CreateAutoScalingGroup` models no launch-template error
//! (`smithy_query(query="operation",operation="CreateAutoScalingGroup")` answers
//! `AlreadyExistsFault`, `LimitExceededFault`, `ResourceContentionFault` and
//! `ServiceLinkedRoleFailure`), and `LaunchTemplateSpecification$LaunchTemplateId` /
//! `$LaunchTemplateName` are each "Conditional: You must specify either a `LaunchTemplateId`
//! or a `LaunchTemplateName`", so the refusal rides the service-common `ValidationError` and
//! WHICH sentence it carries is decided by the half the request stated.
//!
//! What this suite adds over the per-reference-site table in
//! `dangling_launch_template_refusals`: the two halves are driven as a PAIR, against the SAME
//! deleted template of ONE account, with the by-id value taken from the mint rather than
//! written out — and each pair is followed by the identical create against a LIVE template, so
//! the refusal is shown to be about the reference and not about the request. The rows span
//! where the group places instances (`VPCZoneIdentifier` over one subnet, over two, and
//! `AvailabilityZones`) and the FORM of the identifier the sentence has to echo (the shortest
//! name the model admits, one carrying every punctuation character it admits, and one at its
//! published maximum length).
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols — awsQuery and ec2Query — so the VPC, the subnets and every launch template this
//! suite creates and deletes are written where a client writes them.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, delete_launch_template, ec2_query, emulator, list_members, p,
    query, tag_value, tag_values, xml_unescape, REGION,
};

/// The error code Auto Scaling answers a request naming a resource that does not resolve.
const VALIDATION_ERROR: &str = "ValidationError";

/// The network every subnet-placed row puts its subnets in.
const VPC_CIDR_BLOCK: &str = "10.91.0.0/16";

/// The published maximum length of a launch-template name: `LaunchTemplateName` is "3 to 128"
/// characters of `[a-zA-Z0-9().\-/_]`
/// (ec2 `smithy_query(query="shape",shape="LaunchTemplateName")`).
const LAUNCH_TEMPLATE_NAME_MAX: usize = 128;

/// Which half of the reference a case states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    /// `LaunchTemplateId`, whose value this suite takes from Amazon EC2's own mint.
    Id,
    /// `LaunchTemplateName`, whose value the row chose when it created the template.
    Name,
}

impl Half {
    /// The specification member this half is stated under.
    fn member(self) -> &'static str {
        match self {
            Self::Id => "LaunchTemplateId",
            Self::Name => "LaunchTemplateName",
        }
    }

    /// A word for the half, for the group names and case labels of a row.
    fn label(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
        }
    }
}

/// The two halves of one reference, in the order the recorded client states them: a group
/// launches from ONE template, so each half is a complete reference on its own.
const HALVES: &[Half] = &[Half::Name, Half::Id];

/// Where a row states the group places instances.
///
/// Its own enum rather than `support::Placement`, which is the two-way choice (one zone or one
/// subnet) the suites creating a single group per placement share: what this suite spans is the
/// COUNT of subnets a `VPCZoneIdentifier` may carry beside the zone form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// `VPCZoneIdentifier`, over the row's first `n` subnets.
    Subnets(usize),
    /// `AvailabilityZones`, naming the Region's first zone.
    Zones,
}

/// The FORM of the launch-template name a row creates its templates under — the variable part
/// the by-name sentence has to echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameForm {
    /// The shortest name the model admits.
    Shortest,
    /// A name carrying every punctuation character the model's pattern admits.
    Punctuated,
    /// A name at the model's published maximum length.
    Longest,
}

impl NameForm {
    /// The name of the template the row DELETES before it configures a group against it.
    fn deleted(self) -> String {
        match self {
            Self::Shortest => "a.b".to_string(),
            Self::Punctuated => "asg(eni)-dangling_template.1/x".to_string(),
            Self::Longest => "d".repeat(LAUNCH_TEMPLATE_NAME_MAX),
        }
    }

    /// The name of the template the row KEEPS, which the create that follows the pair names.
    fn held(self) -> String {
        match self {
            Self::Shortest => "x.y".to_string(),
            Self::Punctuated => "asg(eni)-held_template.1/x".to_string(),
            Self::Longest => "e".repeat(LAUNCH_TEMPLATE_NAME_MAX),
        }
    }
}

/// Where the rows place their groups.
const PLACEMENTS: &[(&str, Placement)] = &[
    ("one subnet", Placement::Subnets(1)),
    ("two subnets", Placement::Subnets(2)),
    ("availability zones", Placement::Zones),
];

/// The identifier forms the rows make the sentence echo.
const NAME_FORMS: &[(&str, NameForm)] = &[
    ("the shortest name the model admits", NameForm::Shortest),
    (
        "a name carrying every punctuation character the model admits",
        NameForm::Punctuated,
    ),
    (
        "a name at the model's published maximum length",
        NameForm::Longest,
    ),
];

/// The sentence AWS answers a reference that resolves to no launch template with, written out
/// here rather than read off the implementation: fixed text around the ONE identifier the
/// request stated.
fn expected_message(half: Half, value: &str) -> String {
    match half {
        Half::Name => {
            format!("The specified launch template, with template name {value}, does not exist.")
        }
        Half::Id => {
            format!("The specified launch template, with template ID {value}, does not exist.")
        }
    }
}

/// The Region's own Availability Zone catalog, read from Amazon EC2 rather than written here.
fn zones() -> Vec<String> {
    let zones: Vec<String> = ec2::availability_zones_in_region(REGION)
        .into_iter()
        .map(|zone| zone.zone_name)
        .collect();
    assert!(
        zones.len() >= 2,
        "the Region must publish at least two Availability Zones, got {zones:?}"
    );
    zones
}

/// Read one element out of an answered ec2Query body, asserting the call was served.
async fn ec2_value(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    tag: &str,
) -> String {
    let (status, body) = ec2_query(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} expected 200, got {status}: {body}"
    );
    let value = tag_value(&body, tag);
    assert!(
        !value.is_empty(),
        "case {case:?}: {action} must report <{tag}>, got {body}"
    );
    value
}

/// Create one launch template over Amazon EC2's own wire and answer the id that service minted
/// for it — never one this suite spells out.
async fn create_template(router: &MultiServiceRouter, name: &str) -> String {
    create_launch_template(router, name).await.0
}

/// The VPC and the `count` subnets of it the row places its group in, created over Amazon
/// EC2's own wire; the subnet ids are that service's, never values written out here.
async fn create_subnets(router: &MultiServiceRouter, case: &str, count: usize) -> Vec<String> {
    let vpc = ec2_value(
        router,
        case,
        "CreateVpc",
        &[p("CidrBlock", VPC_CIDR_BLOCK)],
        "vpcId",
    )
    .await;
    let zones = zones();
    let mut subnets = Vec::with_capacity(count);
    for at in 0..count {
        let subnet = ec2_value(
            router,
            case,
            "CreateSubnet",
            &[
                p("VpcId", vpc.clone()),
                p("CidrBlock", format!("10.91.{}.0/24", at + 1)),
                p("AvailabilityZone", zones[at % zones.len()].clone()),
            ],
            "subnetId",
        )
        .await;
        subnets.push(subnet);
    }
    subnets
}

/// The request parameters stating where the group places instances.
fn placement_params(
    placement: Placement,
    subnets: &[String],
    zones: &[String],
) -> Vec<(String, String)> {
    match placement {
        Placement::Subnets(count) => vec![p("VPCZoneIdentifier", subnets[..count].join(","))],
        Placement::Zones => vec![p("AvailabilityZones.member.1", zones[0].clone())],
    }
}

/// The group body `DescribeAutoScalingGroups` reports under `name`, or `None` when it reports
/// no group of that name.
async fn described_group(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?} must answer 200, got {status}: {body}"
    );
    if tag_values(&body, "AutoScalingGroupName").is_empty() {
        return None;
    }
    Some(body)
}

/// RULE: a `CreateAutoScalingGroup` whose launch-template reference resolves to no template of
/// the account is refused `ValidationError` under HTTP 400 with the sentence for the HALF of
/// the reference the request stated, echoing that identifier verbatim and creating nothing,
/// while the identical create naming a template the account holds is served.
#[tokio::test]
async fn a_dangling_create_reference_is_refused_by_the_half_it_states_and_a_held_one_is_served() {
    for (placement_at, (placement_case, placement)) in PLACEMENTS.iter().enumerate() {
        for (name_at, (name_case, name_form)) in NAME_FORMS.iter().enumerate() {
            let case = format!("{placement_case}, {name_case}");
            let (router, _engine) = emulator();
            let zones = zones();
            let subnets = match placement {
                Placement::Subnets(count) => create_subnets(&router, &case, *count).await,
                Placement::Zones => Vec::new(),
            };

            // A template the account HOLDS for the whole row, and one it held until a client
            // deleted it through the service that owns launch templates.
            let held_name = name_form.held();
            let held_id = create_template(&router, &held_name).await;
            let deleted_name = name_form.deleted();
            let deleted_id = create_template(&router, &deleted_name).await;
            assert_ne!(
                deleted_id, held_id,
                "case {case:?}: the two templates must be two resources, got {deleted_id:?}"
            );
            delete_launch_template(&router, &case, &deleted_id).await;

            let group = format!("dangling-pair-group-{placement_at}-{name_at}");

            // Both halves of the SAME dangling reference, each a complete reference on its
            // own, each earning the sentence for the half it states.
            for half in HALVES {
                let case = format!("{case}, by {}", half.label());
                let value = match half {
                    Half::Id => deleted_id.clone(),
                    Half::Name => deleted_name.clone(),
                };
                let mut params = vec![
                    p("AutoScalingGroupName", group.clone()),
                    p("MinSize", "0"),
                    p("MaxSize", "1"),
                    p("DesiredCapacity", "0"),
                    p(&format!("LaunchTemplate.{}", half.member()), value.clone()),
                ];
                params.extend(placement_params(*placement, &subnets, &zones));

                let (status, body) =
                    query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: a reference resolving to no template must be refused, got \
                     {status}: {body}"
                );
                assert_eq!(
                    tag_value(&body, "Code"),
                    VALIDATION_ERROR,
                    "case {case:?}: expected {VALIDATION_ERROR}, got {body}"
                );
                let answered = xml_unescape(&tag_value(&body, "Message"));
                let expected = expected_message(*half, &value);
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                );

                // The refused create left nothing behind for the name it asked for.
                let described = described_group(&router, &group).await;
                assert!(
                    described.is_none(),
                    "case {case:?}: the refused create must leave no group, got {described:?}"
                );
            }

            // The IDENTICAL create against the template the account still holds is served, and
            // the group it leaves reports the template it resolved and the placement it stated.
            for half in HALVES {
                let case = format!("{case}, held template by {}", half.label());
                let group = format!("{group}-held-{}", half.label());
                let value = match half {
                    Half::Id => held_id.clone(),
                    Half::Name => held_name.clone(),
                };
                let mut params = vec![
                    p("AutoScalingGroupName", group.clone()),
                    p("MinSize", "0"),
                    p("MaxSize", "1"),
                    p("DesiredCapacity", "0"),
                    p(&format!("LaunchTemplate.{}", half.member()), value),
                ];
                params.extend(placement_params(*placement, &subnets, &zones));

                let (status, body) =
                    query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a reference the account holds must be served, got {status}: \
                     {body}"
                );

                let described = described_group(&router, &group).await.unwrap_or_else(|| {
                    panic!("case {case:?}: the served create must leave a group")
                });
                assert_eq!(
                    tag_value(&described, "LaunchTemplateId"),
                    held_id,
                    "case {case:?}: the group must report the template it resolved, got \
                     {described}"
                );
                assert_eq!(
                    tag_value(&described, "LaunchTemplateName"),
                    held_name,
                    "case {case:?}: the group must report the template it resolved, got \
                     {described}"
                );
                match placement {
                    Placement::Subnets(count) => assert_eq!(
                        tag_value(&described, "VPCZoneIdentifier"),
                        subnets[..*count].join(","),
                        "case {case:?}: the group must report the subnets it was placed in, got \
                         {described}"
                    ),
                    Placement::Zones => {
                        let reported: Vec<&str> = list_members(&described, "AvailabilityZones");
                        assert_eq!(
                            reported,
                            vec![zones[0].as_str()],
                            "case {case:?}: the group must report the zone it was placed in, got \
                             {described}"
                        );
                    }
                }
            }
        }
    }
}
