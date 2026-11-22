// SPDX-License-Identifier: CC0-1.0

//! A group KEEPS the launch-template reference it was configured with after Amazon EC2 has
//! deleted the template, and the next launch is the call that cannot be made.
//!
//! RULE under test: a capacity change is a request about the capacity a group MAINTAINS, so it
//! is accepted whatever its launch source then resolves to; the launch it starts resolves that
//! source against the service that owns it at the instant of the launch, and a reference that
//! resolves to nothing places no instance — each unit it could not place records a terminal
//! `Cancelled` activity at `Progress` 100 whose `StatusMessage` is the owning service's own
//! sentence for the half of the reference that did not resolve, the group keeps the
//! `DesiredCapacity` that was asked for with no member for it, and the owning service holds no
//! instance for the group.
//!
//! The rows span what the rule generalises over: how the group names the template (by id, by
//! name), how it names the version (`$Latest`, `$Default`, no member at all, a literal number
//! Amazon EC2 minted), how many units the capacity change asks for at once (one, and the whole
//! of the group's maximum), and how the group states where it places instances
//! (`VPCZoneIdentifier` over one subnet, and over two subnets in two zones). The spellings and
//! versions of the reference that a ZONE-placed group carries are pinned beside these by
//! `deleted_launch_template_cancels_a_launch`; what is spanned here is the SUBNET-placed group
//! and the capacity change that asks for more than one unit, plus the account's own Amazon EC2
//! view: a launch that was cancelled leaves EC2 holding no instance tagged for the group.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols — awsQuery and ec2Query — so the template, its deletion, the capacity change, the
//! activities and the instances are each written and read where a client writes and reads them.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, ec2_query, emulator, list_members, p, query, seed_subnets, tag_value, tag_values,
    xml_unescape, SeededSubnet,
};

/// The AMI every launch template in this suite names, in the long id form Amazon EC2 issues
/// one in.
const IMAGE_ID: &str = "ami-00000000000000091";

/// The instance type every launch template in this suite names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The tag Amazon EC2 Auto Scaling puts the group's name on every instance it launches in, so
/// the account's own Amazon EC2 view can be asked which instances belong to one group.
///
/// SOURCE: the reserved tag AWS documents on the instances of an Auto Scaling group,
/// `aws:autoscaling:groupName`; written out here rather than taken from the implementation,
/// which a case re-using the service's own literal would assert nothing about.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// How long a case keeps looking for the outcome it waits on before it gives up — generous on
/// purpose, so a loaded host is never the reason a case fails.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The status codes a scaling activity has FINISHED in, from the model's own
/// `ScalingActivityStatusCode`: a client polling for the outcome of a capacity change stops at
/// one of these.
const TERMINAL_STATUS_CODES: &[&str] = &["Successful", "Failed", "Cancelled"];

/// What a launch that could not resolve the launch template its group points at reports as the
/// activity's `StatusMessage`, written out here rather than taken from either implementation.
///
/// SOURCE: Amazon EC2's `InvalidLaunchTemplateId.NotFound` sentence for a launch naming a
/// template the account no longer holds — the sentence a client's own launch against the
/// deleted template is answered. Fixed text apart from the template id, which is the one the
/// group's own reference carries.
fn absent_template_message(launch_template_id: &str) -> String {
    format!("The specified launch template, with template ID {launch_template_id}, does not exist.")
}

/// One launch template of the account: what Amazon EC2 minted for it.
#[derive(Debug, Clone)]
struct Template {
    id: String,
    name: String,
    /// The version number Amazon EC2 gave the version it created with the template.
    latest_version: String,
}

/// Create one launch template over Amazon EC2's own wire and answer what EC2 minted for it.
async fn create_template(router: &MultiServiceRouter, case: &str, name: &str) -> Template {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
    let id = tag_value(&body, "launchTemplateId");
    let latest_version = tag_value(&body, "latestVersionNumber");
    assert!(
        !id.is_empty() && !latest_version.is_empty(),
        "case {case:?}: CreateLaunchTemplate must report the template's id and latest version \
         number, got {body}"
    );
    Template {
        id,
        name: name.to_string(),
        latest_version,
    }
}

/// Delete the whole launch template through the service that owns it, and assert a client can
/// no longer read it back — the precondition every cancelled case rests on.
async fn delete_template(router: &MultiServiceRouter, case: &str, template: &Template) {
    let (status, body) = ec2_query(
        router,
        "DeleteLaunchTemplate",
        &[p("LaunchTemplateId", &template.id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteLaunchTemplate for {:?} expected 200, got {status}: {body}",
        template.id
    );
    let (status, body) = ec2_query(
        router,
        "DescribeLaunchTemplates",
        &[p("LaunchTemplateId.1", &template.id)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: DescribeLaunchTemplates must refuse the deleted template, got {status}: \
         {body}"
    );
}

/// How the group's own `LaunchTemplate` names the template it launches from: "you must specify
/// either a `LaunchTemplateId` or a `LaunchTemplateName`"
/// (`smithy_query(query="shape",shape="LaunchTemplateSpecification")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// By the id Amazon EC2 minted for the template.
    Id,
    /// By the name the template was created under.
    Name,
}

/// How the group's reference names the VERSION it launches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// `$Latest`, which floats on whatever version is newest at the instant of the launch.
    Latest,
    /// `$Default`, which floats on the template's default version.
    Default,
    /// No `Version` member at all: "the default value is `$Default`"
    /// (`smithy_query(query="shape",shape="LaunchTemplateSpecification")`).
    Absent,
    /// The literal number Amazon EC2 minted for the template's first version.
    Literal,
}

impl Version {
    /// The `Version` value this spelling sends for `template`, or `None` when it sends no
    /// member.
    fn value(self, template: &Template) -> Option<String> {
        match self {
            Self::Latest => Some("$Latest".to_string()),
            Self::Default => Some("$Default".to_string()),
            Self::Absent => None,
            Self::Literal => Some(template.latest_version.clone()),
        }
    }
}

/// Create the group `row` describes: it launches from `template` and places its instances in
/// `subnets`, as a `VPCZoneIdentifier` naming them and nothing else — a group that names only
/// subnets takes its Availability Zones from the zones those subnets are in.
async fn create_group(
    router: &MultiServiceRouter,
    row: &Row,
    subnets: &[SeededSubnet],
    template: &Template,
) {
    let case = row.case;
    let identifier = subnets
        .iter()
        .map(|subnet| subnet.id.clone())
        .collect::<Vec<String>>()
        .join(",");
    let mut params = vec![
        p("AutoScalingGroupName", row.group),
        p("MinSize", "0"),
        p("MaxSize", row.max_size.to_string()),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", identifier),
    ];
    match row.named {
        Named::Id => params.push(p("LaunchTemplate.LaunchTemplateId", &template.id)),
        Named::Name => params.push(p("LaunchTemplate.LaunchTemplateName", &template.name)),
    }
    if let Some(version) = row.version.value(template) {
        params.push(p("LaunchTemplate.Version", version));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {:?} expected 200, got {status}: {body}",
        row.group
    );
}

/// Ask the group for `capacity` units of capacity, and answer the status the service gave the
/// request: a capacity change is about the capacity the group maintains, so whether it is
/// ACCEPTED is itself part of the rule.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    group: &str,
    capacity: i32,
) -> (u16, String) {
    query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &capacity.to_string()),
        ],
    )
    .await
}

/// What one scaling activity reported: the members a client reads the outcome of a capacity
/// change off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    status_code: String,
    status_message: String,
    progress: String,
}

/// Every scaling activity the group has recorded, newest first, read through
/// `DescribeScalingActivities` the way a client polls for the outcome of the capacity change it
/// just asked for.
async fn activities(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Outcome> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .map(|member| Outcome {
            status_code: tag_value(member, "StatusCode"),
            status_message: xml_unescape(&tag_value(member, "StatusMessage")),
            progress: tag_value(member, "Progress"),
        })
        .collect()
}

/// The group's activities once there are at least `wanted` of them and every one has reached a
/// terminal status code.
///
/// Looked for again until that holds or [`DEADLINE`] passes: an activity the clock still owes
/// is reported in progress, so this waits for the OUTCOME rather than for any length of time.
async fn settled_activities(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<Outcome> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let settled = activities(router, case, group).await;
        let terminal = settled
            .iter()
            .all(|outcome| TERMINAL_STATUS_CODES.contains(&outcome.status_code.as_str()));
        if settled.len() >= wanted && terminal {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change must record {wanted} terminal activity(ies), got \
             {settled:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The instance ids the group reports holding, as its own read reports them.
async fn group_instances(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
}

/// The whole group, as one answer, for the members a case reads off it.
async fn group_body(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    body
}

/// The instance ids the account's own Amazon EC2 view holds FOR THIS GROUP, asked of the
/// service that owns the instances through the tag Auto Scaling marks them with, sorted so a
/// case compares a set rather than an order EC2 never promised.
async fn ec2_instances_of(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = ec2_query(
        router,
        "DescribeInstances",
        &[
            p("Filter.1.Name", format!("tag:{GROUP_NAME_TAG}")),
            p("Filter.1.Value.1", group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances expected 200, got {status}: {body}"
    );
    let mut ids = tag_values(&body, "instanceId");
    ids.sort();
    ids.dedup();
    ids
}

/// One row of either table below: the group it creates, the template it points at, and the
/// capacity change it asks for.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    named: Named,
    version: Version,
    /// How many subnets the group's `VPCZoneIdentifier` names.
    subnets: usize,
    max_size: i32,
    /// How many units of capacity the change asks for.
    asked: i32,
}

/// A capacity change asked of a group whose launch template Amazon EC2 has deleted is
/// ACCEPTED, places nothing, and records one terminal `Cancelled` activity per unit it could
/// not place, carrying Amazon EC2's own sentence for the reference the group still points at.
#[tokio::test]
async fn a_scale_out_from_a_deleted_launch_template_is_accepted_and_cancelled() {
    let rows = [
        Row {
            case: "one subnet, the template named by id at $Latest, one unit below the maximum",
            group: "stale-ref-one-unit",
            template: "lt-stale-ref-one-unit",
            named: Named::Id,
            version: Version::Latest,
            subnets: 1,
            max_size: 2,
            asked: 1,
        },
        Row {
            case: "one subnet, the template named by name at $Default, the whole maximum asked \
                   for at once",
            group: "stale-ref-whole-maximum",
            template: "lt-stale-ref-whole-maximum",
            named: Named::Name,
            version: Version::Default,
            subnets: 1,
            max_size: 2,
            asked: 2,
        },
        Row {
            case: "two subnets in two zones, the template named by id with no Version member, \
                   two units asked for",
            group: "stale-ref-two-subnets",
            template: "lt-stale-ref-two-subnets",
            named: Named::Id,
            version: Version::Absent,
            subnets: 2,
            max_size: 3,
            asked: 2,
        },
        Row {
            case: "one subnet, the template named by id at the literal version number EC2 \
                   minted",
            group: "stale-ref-literal-version",
            template: "lt-stale-ref-literal-version",
            named: Named::Id,
            version: Version::Literal,
            subnets: 1,
            max_size: 2,
            asked: 1,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, row.subnets);
        let template = create_template(&router, case, row.template).await;
        create_group(&router, row, &subnets, &template).await;
        delete_template(&router, case, &template).await;

        // The group KEEPS the reference it was configured with: the template is Amazon EC2's
        // resource and the client's to delete, and the group reports what it points at —
        // which is the only way a client can see what to repair.
        let stale = tag_value(&group_body(&router, case, row.group).await, "LaunchTemplateId");
        assert_eq!(
            stale, template.id,
            "case {case:?}: the group must keep the launch template reference it was configured \
             with, got {stale:?}"
        );

        // The capacity change is a request about the capacity the group MAINTAINS, so it is
        // accepted whatever the launch source now resolves to.
        let (status, body) = set_desired_capacity(&router, row.group, row.asked).await;
        assert_eq!(
            status, 200,
            "case {case:?}: SetDesiredCapacity {} must be accepted, got {status}: {body}",
            row.asked
        );

        let wanted = usize::try_from(row.asked).expect("the asked capacity is not negative");
        let settled = settled_activities(&router, case, row.group, wanted).await;
        let expected = absent_template_message(&template.id);
        assert_eq!(
            settled.len(),
            wanted,
            "case {case:?}: every unit that could not be placed records one activity, got \
             {settled:?}"
        );
        for outcome in &settled {
            assert_eq!(
                (
                    outcome.status_code.as_str(),
                    outcome.status_message.as_str(),
                    outcome.progress.as_str(),
                ),
                ("Cancelled", expected.as_str(), "100"),
                "case {case:?}: expected a cancelled activity carrying Amazon EC2's sentence for \
                 the reference, got {outcome:?}"
            );
        }

        // Nothing was launched: not in the group, and not in the account's own Amazon EC2 view
        // either — an instance the group reported would resolve nowhere.
        let instances = group_instances(&router, case, row.group).await;
        assert!(
            instances.is_empty(),
            "case {case:?}: the group must hold no instance when its launch source resolves to \
             nothing, got {instances:?}"
        );
        let held = ec2_instances_of(&router, case, row.group).await;
        assert!(
            held.is_empty(),
            "case {case:?}: Amazon EC2 must hold no instance for the group, got {held:?}"
        );

        // The capacity the caller asked for STANDS, unmet: it is the capacity the group
        // maintains, not an instruction one call carried out.
        let desired = tag_value(
            &group_body(&router, case, row.group).await,
            "DesiredCapacity",
        );
        assert_eq!(
            desired,
            row.asked.to_string(),
            "case {case:?}: the group must keep the capacity that was asked for, got {desired:?}"
        );
    }
}

/// The control the table above is read against: the same capacity change on the same kind of
/// group launches its instances when the reference still resolves, the activities are
/// `Successful` with no `StatusMessage`, and Amazon EC2 holds exactly the instances the group
/// reports.
#[tokio::test]
async fn a_scale_out_from_a_launch_template_the_account_holds_places_its_instances() {
    let rows = [
        Row {
            case: "one subnet, the template named by id at $Latest, one unit",
            group: "held-ref-one-unit",
            template: "lt-held-ref-one-unit",
            named: Named::Id,
            version: Version::Latest,
            subnets: 1,
            max_size: 2,
            asked: 1,
        },
        Row {
            case: "two subnets in two zones, the template named by name with no Version member, \
                   two units",
            group: "held-ref-two-subnets",
            template: "lt-held-ref-two-subnets",
            named: Named::Name,
            version: Version::Absent,
            subnets: 2,
            max_size: 3,
            asked: 2,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, row.subnets);
        let template = create_template(&router, case, row.template).await;
        create_group(&router, row, &subnets, &template).await;

        let (status, body) = set_desired_capacity(&router, row.group, row.asked).await;
        assert_eq!(
            status, 200,
            "case {case:?}: SetDesiredCapacity {} must be accepted, got {status}: {body}",
            row.asked
        );

        let wanted = usize::try_from(row.asked).expect("the asked capacity is not negative");
        let settled = settled_activities(&router, case, row.group, wanted).await;
        assert_eq!(
            settled.len(),
            wanted,
            "case {case:?}: the capacity change records one activity per instance it launched, \
             got {settled:?}"
        );
        for outcome in &settled {
            assert_eq!(
                (outcome.status_code.as_str(), outcome.progress.as_str()),
                ("Successful", "100"),
                "case {case:?}: a launch from a reference that resolves must succeed, got \
                 {outcome:?}"
            );
            assert!(
                outcome.status_message.is_empty(),
                "case {case:?}: an activity that did not fail carries no status message, got \
                 {outcome:?}"
            );
        }

        let mut instances = group_instances(&router, case, row.group).await;
        instances.sort();
        assert_eq!(
            instances.len(),
            wanted,
            "case {case:?}: the group must hold the instances it launched, got {instances:?}"
        );
        let held = ec2_instances_of(&router, case, row.group).await;
        assert_eq!(
            held, instances,
            "case {case:?}: Amazon EC2 must hold exactly the instances the group reports, got \
             {held:?}"
        );
    }
}
