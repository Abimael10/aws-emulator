// SPDX-License-Identifier: CC0-1.0

//! The launch template an Auto Scaling group points at is Amazon EC2's resource, and the
//! group resolves it AT THE INSTANT OF EACH LAUNCH.
//!
//! RULE under test: a capacity change launches from the launch-template version the group's
//! specification names at the instant of the launch, so a specification that no longer
//! resolves against Amazon EC2 — the template deleted, or the version the group pinned
//! deleted — places no instance; the capacity change records a terminal `Cancelled` scaling
//! activity whose `StatusMessage` is Amazon EC2's own sentence for the reference it could not
//! resolve, the group keeps the `DesiredCapacity` that was asked for, and
//! `DescribeScalingActivities` is where a client reads all of it. A specification that still
//! resolves launches the instance and the activity is `Successful`.
//!
//! The rows span what the rule generalises over: how the group names the template (by id, by
//! name, through a `MixedInstancesPolicy`), how it names the version (`$Default`, `$Latest`,
//! a literal number), and WHICH half of the reference stopped resolving (the template itself,
//! or the one version the group pinned). The last test is the repair the rule turns on: the
//! capacity a cancelled launch could not place stays owed, so repointing the group at a
//! template the account holds launches it with no further capacity change from anybody.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols — awsQuery and ec2Query — so every template, deletion, instance and activity is
//! written and read where a client writes and reads it.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, ec2_query, emulator, list_members, p, query, tag_value, tag_values, xml_unescape,
    REGION,
};

/// The AMI every launch template in this suite names, in the long id form Amazon EC2 issues
/// one in.
const IMAGE_ID: &str = "ami-00000000000000071";

/// The instance type every launch template in this suite names.
const INSTANCE_TYPE: &str = "t3.micro";

/// How long a case keeps looking for the outcome it waits on before it gives up — generous on
/// purpose, so a loaded host is never the reason a case fails.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The status codes a scaling activity has FINISHED in, from the model's own
/// `ScalingActivityStatusCode`: a client polling for the outcome of a capacity change stops at
/// one of these.
const TERMINAL_STATUS_CODES: &[&str] = &["Successful", "Failed", "Cancelled"];

/// The zone every group here places its instances in: the first of the Region's own catalog,
/// never written out.
fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// What a launch that could not resolve a launch template reports as its `StatusMessage`,
/// written out here rather than taken from either implementation: a case that re-used the
/// service's own literal would assert nothing about it.
///
/// SOURCE: Amazon EC2's `InvalidLaunchTemplateId.NotFound` sentence for a LAUNCH naming a
/// template the account no longer holds — the sentence a client's own `RunInstances` against
/// the deleted template is answered.
fn absent_template_message(launch_template_id: &str) -> String {
    format!("The specified launch template, with template ID {launch_template_id}, does not exist.")
}

/// The same for the other half of the reference: Amazon EC2's
/// `InvalidLaunchTemplateId.VersionNotFound` sentence for a version the template does not
/// hold.
fn absent_version_message(launch_template_id: &str, version: &str) -> String {
    format!(
        "Could not find the specified version {version} for the launch template with ID \
         {launch_template_id}."
    )
}

/// One launch template of the account: what Amazon EC2 minted for it.
#[derive(Debug, Clone)]
struct Template {
    id: String,
    name: String,
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
    let latest = tag_value(&body, "latestVersionNumber");
    assert!(
        !id.is_empty() && !latest.is_empty(),
        "case {case:?}: CreateLaunchTemplate must report the template's id and latest version \
         number, got {body}"
    );
    Template {
        id,
        name: name.to_string(),
    }
}

/// Add one version to a launch template and answer the version number Amazon EC2 gave it.
async fn add_version(router: &MultiServiceRouter, case: &str, template: &Template) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateId", &template.id),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplateVersion for {:?} expected 200, got {status}: {body}",
        template.id
    );
    let version = tag_value(&body, "versionNumber");
    assert!(
        !version.is_empty(),
        "case {case:?}: CreateLaunchTemplateVersion must report the version number, got {body}"
    );
    version
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

/// Delete ONE version of a launch template the account keeps, and assert a client can no
/// longer read that version back.
async fn delete_version(
    router: &MultiServiceRouter,
    case: &str,
    template: &Template,
    version: &str,
) {
    let (status, body) = ec2_query(
        router,
        "DeleteLaunchTemplateVersions",
        &[
            p("LaunchTemplateId", &template.id),
            p("LaunchTemplateVersion.1", version),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteLaunchTemplateVersions for {:?} expected 200, got {status}: {body}",
        template.id
    );
    let (status, body) = ec2_query(
        router,
        "DescribeLaunchTemplateVersions",
        &[
            p("LaunchTemplateId", &template.id),
            p("LaunchTemplateVersion.1", version),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: DescribeLaunchTemplateVersions must refuse the deleted version, got \
         {status}: {body}"
    );
}

/// How a group names the launch template it launches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The group's own `LaunchTemplate`, naming the template by its id.
    Id,
    /// The group's own `LaunchTemplate`, naming the template by its name.
    Name,
    /// The launch template of the group's `MixedInstancesPolicy`, named by id.
    Policy,
}

impl Named {
    /// The request parameters this spelling contributes for `template` at `version`.
    fn params(self, template: &Template, version: &str) -> Vec<(String, String)> {
        match self {
            Self::Id => vec![
                p("LaunchTemplate.LaunchTemplateId", &template.id),
                p("LaunchTemplate.Version", version),
            ],
            Self::Name => vec![
                p("LaunchTemplate.LaunchTemplateName", &template.name),
                p("LaunchTemplate.Version", version),
            ],
            Self::Policy => vec![
                p(
                    "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification\
                     .LaunchTemplateId",
                    &template.id,
                ),
                p(
                    "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.Version",
                    version,
                ),
            ],
        }
    }
}

/// Create one group of maximum size one, holding nothing yet, that launches from `version` of
/// the template `named` spells.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    named: Named,
    template: &Template,
    version: &str,
) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", zone()),
    ];
    params.extend(named.params(template, version));
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {group:?} expected 200, got {status}: {body}"
    );
}

/// Ask the group for `capacity` units of capacity.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    capacity: &str,
) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", capacity),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity {capacity} for {group:?} expected 200, got {status}: \
         {body}"
    );
}

/// What one scaling activity reported: the members a client reads the outcome of a capacity
/// change off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    status_code: String,
    status_message: String,
    progress: String,
}

/// The group's NEWEST scaling activity once it has reached a terminal status code, read
/// through `DescribeScalingActivities` the way a client polls for the outcome of the capacity
/// change it just asked for.
///
/// Looked for again until it is terminal or [`DEADLINE`] passes: an activity the clock still
/// owes is reported in progress, so this waits for the OUTCOME rather than for any length of
/// time.
async fn settled_activity(router: &MultiServiceRouter, case: &str, group: &str) -> Outcome {
    let deadline = Instant::now() + DEADLINE;
    loop {
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
        let members = list_members(&body, "Activities");
        let newest = members.first().copied().unwrap_or_default();
        let status_code = tag_value(newest, "StatusCode");
        if TERMINAL_STATUS_CODES.contains(&status_code.as_str()) {
            return Outcome {
                status_code,
                status_message: xml_unescape(&tag_value(newest, "StatusMessage")),
                progress: tag_value(newest, "Progress"),
            };
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change must record a terminal activity, got {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The instance ids the group reports holding, as its own read reports them.
///
/// Read as every `InstanceId` of the answer rather than as the members of one element,
/// because a group carrying a `MixedInstancesPolicy` reports elements of its own whose names
/// begin the same way: the ids are what a client reads the group's members off.
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

/// The `DesiredCapacity` the group reports.
async fn desired_capacity(router: &MultiServiceRouter, case: &str, group: &str) -> String {
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
    tag_value(&body, "DesiredCapacity")
}

/// Which half of the reference a row takes away, and how the group names the version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removed {
    /// The whole launch template is deleted, the group floating on `$Default`.
    TemplateThroughDefault,
    /// The whole launch template is deleted, the group floating on `$Latest`.
    TemplateThroughLatest,
    /// The template stays and the one version the group PINNED is deleted.
    PinnedVersion,
}

/// One row of the cancelled table.
struct Blocked {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    named: Named,
    removed: Removed,
}

/// A capacity change whose launch source no longer resolves against Amazon EC2 places no
/// instance and records a terminal `Cancelled` activity carrying Amazon EC2's own sentence for
/// the reference it could not resolve.
#[tokio::test]
async fn a_launch_from_a_reference_that_no_longer_resolves_is_cancelled() {
    let rows = [
        Blocked {
            case: "the template named by id is deleted, through $Default",
            group: "lt-gone-by-id",
            template: "lt-gone-by-id-template",
            named: Named::Id,
            removed: Removed::TemplateThroughDefault,
        },
        Blocked {
            case: "the template named by name is deleted, through $Latest",
            group: "lt-gone-by-name",
            template: "lt-gone-by-name-template",
            named: Named::Name,
            removed: Removed::TemplateThroughLatest,
        },
        Blocked {
            case: "the template of a mixed instances policy is deleted, through $Default",
            group: "lt-gone-by-policy",
            template: "lt-gone-by-policy-template",
            named: Named::Policy,
            removed: Removed::TemplateThroughDefault,
        },
        Blocked {
            case: "the template stays and the pinned version is deleted",
            group: "lt-version-gone",
            template: "lt-version-gone-template",
            named: Named::Id,
            removed: Removed::PinnedVersion,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, _engine) = emulator();
        let template = create_template(&router, case, row.template).await;
        // Which version the group launches from, and what the removal leaves unresolvable.
        let (version, expected_message) = match row.removed {
            Removed::TemplateThroughDefault => (
                "$Default".to_string(),
                absent_template_message(&template.id),
            ),
            Removed::TemplateThroughLatest => {
                ("$Latest".to_string(), absent_template_message(&template.id))
            }
            Removed::PinnedVersion => {
                // A template's DEFAULT version cannot be deleted, so the group pins the
                // second version and that one is taken away under it.
                let pinned = add_version(&router, case, &template).await;
                let expected = absent_version_message(&template.id, &pinned);
                (pinned, expected)
            }
        };
        create_group(&router, case, row.group, row.named, &template, &version).await;
        match row.removed {
            Removed::TemplateThroughDefault | Removed::TemplateThroughLatest => {
                delete_template(&router, case, &template).await;
            }
            Removed::PinnedVersion => delete_version(&router, case, &template, &version).await,
        }

        set_desired_capacity(&router, case, row.group, "1").await;
        let outcome = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (
                outcome.status_code.as_str(),
                outcome.status_message.as_str(),
                outcome.progress.as_str(),
            ),
            ("Cancelled", expected_message.as_str(), "100"),
            "case {case:?}: expected a cancelled activity carrying Amazon EC2's sentence for the \
             reference, got {outcome:?}"
        );
        let instances = group_instances(&router, case, row.group).await;
        assert!(
            instances.is_empty(),
            "case {case:?}: the group must hold no instance when its launch source resolves to \
             nothing, got {instances:?}"
        );
        // The capacity the caller asked for STANDS, unmet: it is the capacity the group
        // maintains, not an instruction one call carried out.
        let desired = desired_capacity(&router, case, row.group).await;
        assert_eq!(
            desired, "1",
            "case {case:?}: the group must keep the capacity that was asked for, got {desired:?}"
        );
    }
}

/// One row of the served table: how the group names a template the account still holds.
struct Served {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    named: Named,
    /// The `Version` literal the group carries, `None` for the second version's own number.
    version: Option<&'static str>,
}

/// A capacity change whose launch source still resolves launches its instance and records a
/// `Successful` activity — the control the table above is read against.
#[tokio::test]
async fn a_launch_from_a_reference_that_resolves_places_its_instance() {
    let rows = [
        Served {
            case: "a template named by id, through $Default",
            group: "lt-held-by-id",
            template: "lt-held-by-id-template",
            named: Named::Id,
            version: Some("$Default"),
        },
        Served {
            case: "a template named by name, through $Latest",
            group: "lt-held-by-name",
            template: "lt-held-by-name-template",
            named: Named::Name,
            version: Some("$Latest"),
        },
        Served {
            case: "the template of a mixed instances policy, at a literal version",
            group: "lt-held-by-policy",
            template: "lt-held-by-policy-template",
            named: Named::Policy,
            version: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, _engine) = emulator();
        let template = create_template(&router, case, row.template).await;
        let version = match row.version {
            Some(version) => version.to_string(),
            None => add_version(&router, case, &template).await,
        };
        create_group(&router, case, row.group, row.named, &template, &version).await;
        set_desired_capacity(&router, case, row.group, "1").await;

        let outcome = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (outcome.status_code.as_str(), outcome.progress.as_str()),
            ("Successful", "100"),
            "case {case:?}: a launch from a reference that resolves must succeed, got {outcome:?}"
        );
        assert!(
            outcome.status_message.is_empty(),
            "case {case:?}: an activity that did not fail carries no status message, got \
             {outcome:?}"
        );
        let instances = group_instances(&router, case, row.group).await;
        assert_eq!(
            instances.len(),
            1,
            "case {case:?}: the group must hold the instance it launched, got {instances:?}"
        );
    }
}

/// The capacity a cancelled launch could not place stays OWED: repointing the group at a
/// launch template the account holds launches it, with no further capacity change from
/// anybody, and the activity that places it is `Successful`.
#[tokio::test]
async fn the_capacity_a_cancelled_launch_could_not_place_is_launched_once_repointed() {
    let case = "a group repointed at a template the account holds";
    let (router, _engine) = emulator();
    let gone = create_template(&router, case, "lt-repointed-gone").await;
    let group = "lt-repointed";
    create_group(&router, case, group, Named::Id, &gone, "$Latest").await;
    delete_template(&router, case, &gone).await;
    set_desired_capacity(&router, case, group, "1").await;

    let cancelled = settled_activity(&router, case, group).await;
    assert_eq!(
        (
            cancelled.status_code.as_str(),
            cancelled.status_message.as_str(),
        ),
        ("Cancelled", absent_template_message(&gone.id).as_str()),
        "case {case:?}: the scale-out must be cancelled while the template is gone, got \
         {cancelled:?}"
    );

    // The client's own remediation: the group is repointed at a template that exists. Nothing
    // asks for the capacity again.
    let held = create_template(&router, case, "lt-repointed-held").await;
    let params = [
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateId", &held.id),
        p("LaunchTemplate.Version", "$Latest"),
    ];
    let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let deadline = Instant::now() + DEADLINE;
    let instances = loop {
        let instances = group_instances(&router, case, group).await;
        if !instances.is_empty() || Instant::now() >= deadline {
            break instances;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: the group must launch the capacity it was owed once repointed, got \
         {instances:?}"
    );
    let settled = settled_activity(&router, case, group).await;
    assert_eq!(
        settled.status_code, "Successful",
        "case {case:?}: the launch made after the repair must succeed, got {settled:?}"
    );
}
