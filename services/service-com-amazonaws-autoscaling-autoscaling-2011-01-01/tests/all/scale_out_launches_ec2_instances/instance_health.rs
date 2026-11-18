// SPDX-License-Identifier: CC0-1.0

//! The health status a caller records for an instance of an Auto Scaling group, and the
//! replacement that status owes.
//!
//! `SetInstanceHealth` "sets the health status of the specified instance", and "set to
//! `Healthy` to have the instance remain in service. Set to `Unhealthy` to have the
//! instance be out of service. Amazon EC2 Auto Scaling terminates and replaces the
//! unhealthy instance"
//! (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="HealthStatus")`), so
//! the group's membership is decided by the health of its members as well as by its size —
//! the parent suite's own concern, in its own file.
//!
//! The rules under test:
//!
//! 1. The status the call records is what EVERY read of that instance reports — the group's
//!    own projection (`DescribeAutoScalingGroups`) and the instance reader
//!    (`DescribeAutoScalingInstances`), which report one stored instance — and recording a
//!    status again, or recording `Healthy` after `Unhealthy`, leaves exactly the last status
//!    recorded.
//! 2. An instance recorded `Unhealthy` is taken out of service and REPLACED: the group goes
//!    on holding the capacity it is configured for, the unhealthy member leaves it, the
//!    Amazon EC2 instance it was is terminated in the account's own Amazon EC2 state and the
//!    replacement runs there, and the activity log states both why the member went out of
//!    service and why the launch was made.
//! 3. "If the Auto Scaling group of the specified instance has a `HealthCheckGracePeriod`
//!    specified for the group, by default, this call respects the grace period. Set this to
//!    `False`, to have the call not respect the grace period associated with the group"
//!    (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="ShouldRespectGracePeriod")`):
//!    while a respected grace period has not elapsed the member stays in service with the
//!    status recorded on it, and a call that does not respect it has the member replaced.
//! 4. Recording `Healthy` again takes the replacement back off: the member the call returned
//!    to service is still the group's member afterwards.
//! 5. A request that cannot be satisfied — a status outside the two the member documents, or
//!    an instance no Auto Scaling group of the account manages — is refused with HTTP 400
//!    `ValidationError` carrying the sentence the service states for that class, and NOTHING
//!    is written.
//!
//! Each table spans what its rule generalises over (which status is recorded and how often,
//! how large the group is, whether it states a grace period and whether the call respects
//! it, which part of a request cannot be satisfied) rather than one recorded row, and every
//! wait below looks again until the condition it needs holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    activity_causes, borrowed, ec2_query, p, query, seed_subnets, tag_value, xml_unescape,
};

use super::{create_launch_template, described_instance, emulator, settle_deadline, LOOK_AGAIN};

/// The `LifecycleState` of a member that is in service, and the two `HealthStatus` values
/// `SetInstanceHealth` documents — written out here rather than taken from the crate, so no
/// assertion takes the implementation's own word for them.
const IN_SERVICE: &str = "InService";
const HEALTHY: &str = "Healthy";
const UNHEALTHY: &str = "Unhealthy";

/// The image and instance type every launch template in this suite carries.
const IMAGE_ID: &str = "ami-00000000000000071";
const INSTANCE_TYPE: &str = "t3.micro";

/// A `HealthCheckGracePeriod` long enough that nothing in a test here outlives it, so a
/// respected grace period holds the health check off for the whole of a case.
const HOLDING_GRACE_PERIOD: i32 = 600;

/// The sentence the service records as the reason a member went out of service because a
/// health check of the CALLER's own found it unhealthy, and the sentence it records as the
/// reason the replacement was launched.
///
/// Written out here rather than read from the crate: the activity log is what a client reads
/// these off, so the assertion states the wording itself.
const TAKEN_OUT_BY_USER_HEALTH_CHECK: &str =
    "an instance was taken out of service in response to a user health-check";
const LAUNCHED_TO_REPLACE_UNHEALTHY: &str =
    "an instance was launched in response to an unhealthy instance needing to be replaced";

/// One instance as a read reports it: its id, the lifecycle state it is in and the health
/// status recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    instance_id: String,
    lifecycle_state: String,
    health_status: String,
}

/// The instances the group `name` reports, in the order reported, read through the group's
/// own projection.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<Reported> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected 200, got {status}: {body}"
    );
    tag_value(&body, "Instances")
        .split("</member>")
        .filter(|member| member.contains("<InstanceId"))
        .map(|member| Reported {
            instance_id: tag_value(member, "InstanceId"),
            lifecycle_state: tag_value(member, "LifecycleState"),
            health_status: tag_value(member, "HealthStatus"),
        })
        .collect()
}

/// What `DescribeAutoScalingInstances` reports for ONE instance, and the group it says the
/// instance belongs to.
async fn described_auto_scaling_instance(
    router: &MultiServiceRouter,
    instance_id: &str,
) -> Option<(Reported, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingInstances",
        &[("InstanceIds.member.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingInstances for {instance_id} expected 200, got {status}: {body}"
    );
    tag_value(&body, "AutoScalingInstances")
        .split("</member>")
        .filter(|member| member.contains("<InstanceId"))
        .map(|member| {
            (
                Reported {
                    instance_id: tag_value(member, "InstanceId"),
                    lifecycle_state: tag_value(member, "LifecycleState"),
                    health_status: tag_value(member, "HealthStatus"),
                },
                tag_value(member, "AutoScalingGroupName"),
            )
        })
        .next()
}

/// What BOTH reads report for one member of `name`, which must be one instance: the group's
/// own projection and the instance reader report the same stored instance, so a case that
/// read only one of them would not know whether the other agreed.
async fn reported_member(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> Reported {
    let held = group_instances(router, name).await;
    let from_group = held
        .iter()
        .find(|instance| instance.instance_id == instance_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the group {name} must still hold {instance_id}, got {held:?}")
        });
    let (from_instance_read, group_of) = described_auto_scaling_instance(router, instance_id)
        .await
        .unwrap_or_else(|| {
            panic!("case {case:?}: DescribeAutoScalingInstances must report {instance_id}")
        });
    assert_eq!(
        from_instance_read, from_group,
        "case {case:?}: the two reads must report one instance for {instance_id}"
    );
    assert_eq!(
        group_of, name,
        "case {case:?}: the instance reader must report the group holding {instance_id}"
    );
    from_group
}

/// Create one group of `capacity` instances in one subnet of the account, carrying the
/// `HealthCheckGracePeriod` `grace_period` names, and answer the ids of the instances it
/// holds once every one of them is in service.
async fn group_holding(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    capacity: i32,
    grace_period: Option<i32>,
) -> Vec<String> {
    let subnets = seed_subnets(engine, 1);
    let template = format!("lt-{name}");
    create_launch_template(router, &template, IMAGE_ID, INSTANCE_TYPE).await;
    let capacity_value = capacity.to_string();
    let grace_value = grace_period.map(|value| value.to_string());
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", name),
        ("LaunchTemplate.LaunchTemplateName", template.as_str()),
        ("MinSize", "0"),
        ("MaxSize", capacity_value.as_str()),
        ("DesiredCapacity", capacity_value.as_str()),
        ("VPCZoneIdentifier", subnets[0].id.as_str()),
    ];
    if let Some(value) = grace_value.as_deref() {
        params.push(("HealthCheckGracePeriod", value));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &params).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
    let wanted = usize::try_from(capacity).expect("a capacity that fits in a usize");
    let held: Vec<String> = super::settled_instances(router, name, wanted)
        .await
        .into_iter()
        .map(|(instance_id, _, _)| instance_id)
        .collect();
    assert_eq!(
        held.len(),
        wanted,
        "the group {name} must hold the capacity it was created with, got {held:?}"
    );
    held
}

/// Record `health_status` for `instance_id`, stating `ShouldRespectGracePeriod` only when
/// `respect` names it, and answer the status and body of the call.
async fn set_instance_health(
    router: &MultiServiceRouter,
    instance_id: &str,
    health_status: &str,
    respect: Option<bool>,
) -> (u16, String) {
    let mut params = vec![
        p("InstanceId", instance_id),
        p("HealthStatus", health_status),
    ];
    if let Some(respect) = respect {
        params.push(p("ShouldRespectGracePeriod", respect.to_string()));
    }
    query(router, "SetInstanceHealth", &borrowed(&params)).await
}

/// Record `health_status` for `instance_id` and require the call to have succeeded.
async fn record_health(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    health_status: &str,
    respect: Option<bool>,
) {
    let (status, body) = set_instance_health(router, instance_id, health_status, respect).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetInstanceHealth {health_status} for {instance_id} expected 200, got \
         {status}: {body}"
    );
}

/// How long a case that asserts the health check has NOT acted keeps looking: several times
/// the span this emulator gives the check, derived from the service's own span so the case
/// and the implementation cannot drift apart.
fn held_off_window() -> Duration {
    autoscaling::state::UNHEALTHY_INSTANCE_REPLACEMENT * 2
}

/// Look again until the group holds `wanted` instances, every one in service, and `gone` is
/// no longer one of them; answer the ids it then holds.
async fn instances_after_replacement(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
    gone: &str,
) -> Vec<String> {
    let deadline = Instant::now() + settle_deadline();
    loop {
        let held = group_instances(router, name).await;
        let replaced = held.len() == wanted
            && held
                .iter()
                .all(|instance| instance.lifecycle_state == IN_SERVICE)
            && held.iter().all(|instance| instance.instance_id != gone);
        if replaced {
            return held
                .into_iter()
                .map(|instance| instance.instance_id)
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group {name} must replace the unhealthy {gone} and hold \
             {wanted} instance(s) in service; last read {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `Cause` the group's scaling activities record comes from the harness's own reader
/// ([`activity_causes`]), so this suite keeps no second copy of that read.
///
/// How many of them state `reason`: a check that acted ONCE leaves one such activity, so a
/// count is what tells an act apart from an act repeated on every later call.
async fn activities_stating(router: &MultiServiceRouter, name: &str, reason: &str) -> usize {
    activity_causes(router, name)
        .await
        .iter()
        .filter(|cause| cause.contains(reason))
        .count()
}

/// One row of the status table: the statuses recorded in order, and whether the request
/// states `ShouldRespectGracePeriod`.
struct StatusRow {
    case: &'static str,
    recorded: &'static [&'static str],
    respect: Option<bool>,
}

/// The status a call records is what every read of that instance reports, and the LAST
/// status recorded is the one in force.
#[tokio::test]
async fn the_recorded_health_status_is_what_every_read_reports() {
    let rows = [
        StatusRow {
            case: "unhealthy, with the grace period left to the default",
            recorded: &[UNHEALTHY],
            respect: None,
        },
        StatusRow {
            case: "unhealthy, with the grace period respected outright",
            recorded: &[UNHEALTHY],
            respect: Some(true),
        },
        StatusRow {
            case: "unhealthy recorded twice: one status, not two",
            recorded: &[UNHEALTHY, UNHEALTHY],
            respect: None,
        },
        StatusRow {
            case: "healthy on a member nothing has failed a check for",
            recorded: &[HEALTHY],
            respect: None,
        },
        StatusRow {
            case: "healthy recorded after unhealthy: the status goes back",
            recorded: &[UNHEALTHY, HEALTHY],
            respect: None,
        },
        StatusRow {
            case: "healthy, with the grace period not respected",
            recorded: &[HEALTHY],
            respect: Some(false),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's. Every group here carries a grace period no case outlives, so
        // the status a row records stands while the row reads it back.
        let (router, engine) = emulator();
        let name = format!("asg-health-{index}");
        let held = group_holding(&router, &engine, &name, 1, Some(HOLDING_GRACE_PERIOD)).await;
        let instance_id = held[0].clone();

        for health_status in row.recorded {
            record_health(&router, row.case, &instance_id, health_status, row.respect).await;
        }

        let expected = *row
            .recorded
            .last()
            .expect("every row records at least one status");
        let reported = reported_member(&router, row.case, &name, &instance_id).await;
        assert_eq!(
            reported.health_status, expected,
            "case {:?}: the reads must report the status recorded last, got {reported:?}",
            row.case
        );
        assert_eq!(
            reported.lifecycle_state, IN_SERVICE,
            "case {:?}: recording a status moves no member out of service by itself, got \
             {reported:?}",
            row.case
        );
        let held_after = group_instances(&router, &name).await;
        assert_eq!(
            held_after.len(),
            1,
            "case {:?}: the group still holds the one member it was created with, got \
             {held_after:?}",
            row.case
        );
    }
}

/// One row of the replacement table: how large the group is, what grace period it states,
/// and whether the call respects it.
struct ReplacementRow {
    case: &'static str,
    capacity: i32,
    grace_period: Option<i32>,
    respect: Option<bool>,
}

/// An instance recorded unhealthy is taken out of service and replaced: the group holds its
/// capacity, the member leaves, Amazon EC2 terminates the instance it was and runs the
/// replacement, and the activity log states both reasons.
#[tokio::test]
async fn an_instance_recorded_unhealthy_is_taken_out_of_service_and_replaced() {
    let rows = [
        ReplacementRow {
            case: "a group of one that states no grace period at all",
            capacity: 1,
            grace_period: None,
            respect: None,
        },
        ReplacementRow {
            case: "a group of two whose grace period the member is already past",
            capacity: 2,
            grace_period: Some(1),
            respect: None,
        },
        ReplacementRow {
            case: "a group of two with a long grace period the call does not respect",
            capacity: 2,
            grace_period: Some(HOLDING_GRACE_PERIOD),
            respect: Some(false),
        },
        ReplacementRow {
            case: "a group of three: only the member recorded unhealthy is replaced",
            capacity: 3,
            grace_period: Some(0),
            respect: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let name = format!("asg-replace-{index}");
        let before = group_holding(&router, &engine, &name, row.capacity, row.grace_period).await;
        let target = before[0].clone();
        let wanted = before.len();

        record_health(&router, row.case, &target, UNHEALTHY, row.respect).await;

        let after = instances_after_replacement(&router, row.case, &name, wanted, &target).await;
        assert_eq!(
            after.len(),
            wanted,
            "case {:?}: the group holds the capacity it is configured for, got {after:?}",
            row.case
        );
        assert_eq!(
            after
                .iter()
                .filter(|instance_id| before.contains(instance_id))
                .count(),
            wanted - 1,
            "case {:?}: every member but the unhealthy one stays, got {after:?} for {before:?}",
            row.case
        );
        let replacement = after
            .iter()
            .find(|instance_id| !before.contains(instance_id))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the group must hold a replacement for {target}, got {after:?}",
                    row.case
                )
            });
        assert_eq!(
            reported_member(&router, row.case, &name, replacement)
                .await
                .health_status,
            HEALTHY,
            "case {:?}: a freshly launched replacement has failed no health check",
            row.case
        );

        // The two views are one account: the member that went out of service is terminated
        // where Amazon EC2 holds it, and the replacement runs there.
        let terminated = described_instance(&router, &target)
            .await
            .unwrap_or_else(|| panic!("case {:?}: Amazon EC2 resolves {target}", row.case));
        assert_eq!(
            terminated.state, "terminated",
            "case {:?}: the unhealthy instance is terminated in Amazon EC2, got {terminated:?}",
            row.case
        );
        let running = described_instance(&router, replacement)
            .await
            .unwrap_or_else(|| panic!("case {:?}: Amazon EC2 resolves {replacement}", row.case));
        assert_eq!(
            running.state, "running",
            "case {:?}: the replacement runs in Amazon EC2, got {running:?}",
            row.case
        );

        let causes = activity_causes(&router, &name).await;
        assert!(
            causes
                .iter()
                .any(|cause| cause.contains(TAKEN_OUT_BY_USER_HEALTH_CHECK)),
            "case {:?}: the log states why the member went out of service, got {causes:?}",
            row.case
        );
        assert!(
            causes
                .iter()
                .any(|cause| cause.contains(LAUNCHED_TO_REPLACE_UNHEALTHY)),
            "case {:?}: the log states why the replacement was launched, got {causes:?}",
            row.case
        );
    }
}

/// A respected grace period holds the health check off: the member stays in service with the
/// status recorded on it for as long as the period has not elapsed.
#[tokio::test]
async fn a_respected_grace_period_holds_the_health_check_off() {
    // The rows differ in HOW the request respects the period: by saying so, and by saying
    // nothing at all — which the member documents as the same thing.
    let rows = [
        ("the grace period respected by default", None),
        ("the grace period respected outright", Some(true)),
    ];

    for (index, (case, respect)) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let name = format!("asg-grace-{index}");
        let held = group_holding(&router, &engine, &name, 1, Some(HOLDING_GRACE_PERIOD)).await;
        let instance_id = held[0].clone();

        record_health(&router, case, &instance_id, UNHEALTHY, *respect).await;

        // Looked at AGAIN and again, over a window several times the span the check takes
        // once it is owed: the period has not elapsed, so every look must find the member
        // where it was.
        let until = Instant::now() + held_off_window();
        loop {
            let reported = reported_member(&router, case, &name, &instance_id).await;
            assert_eq!(
                reported.lifecycle_state, IN_SERVICE,
                "case {case:?}: a respected grace period leaves the member in service, got \
                 {reported:?}"
            );
            assert_eq!(
                reported.health_status, UNHEALTHY,
                "case {case:?}: the status recorded on it stands, got {reported:?}"
            );
            if Instant::now() >= until {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

/// Recording `Healthy` takes the replacement back off: the member returned to service is
/// still the group's member afterwards.
#[tokio::test]
async fn a_healthy_status_takes_the_owed_replacement_back_off() {
    // The rows differ in whether the unhealthy status the second call withdraws was owed
    // under the group's grace period or outside it.
    let rows = [
        (
            "an unhealthy status withdrawn under no grace period",
            None,
            None,
        ),
        (
            "an unhealthy status that did not respect a long grace period, withdrawn",
            Some(HOLDING_GRACE_PERIOD),
            Some(false),
        ),
    ];

    for (index, (case, grace_period, respect)) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let name = format!("asg-withdrawn-{index}");
        let held = group_holding(&router, &engine, &name, 1, *grace_period).await;
        let instance_id = held[0].clone();

        record_health(&router, case, &instance_id, UNHEALTHY, *respect).await;
        record_health(&router, case, &instance_id, HEALTHY, None).await;

        let until = Instant::now() + held_off_window();
        loop {
            let reported = reported_member(&router, case, &name, &instance_id).await;
            assert_eq!(
                reported.health_status, HEALTHY,
                "case {case:?}: the member was returned to service, got {reported:?}"
            );
            assert_eq!(
                reported.lifecycle_state, IN_SERVICE,
                "case {case:?}: a withdrawn unhealthy status replaces nothing, got {reported:?}"
            );
            if Instant::now() >= until {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
}

/// Which part of the request cannot be satisfied.
enum Unsatisfiable {
    /// The `HealthStatus` names neither of the two states the member documents.
    Status(&'static str),
    /// The `InstanceId` names an instance no Auto Scaling group manages; the flag says
    /// whether Amazon EC2 runs an instance of that id at all.
    Instance {
        instance_id: &'static str,
        runs_in_ec2: bool,
    },
}

/// One row of the refusal table.
struct RefusalRow {
    case: &'static str,
    unsatisfiable: Unsatisfiable,
}

/// A request that cannot be satisfied is refused with HTTP 400 `ValidationError` carrying
/// the sentence its class states, and the group keeps the member it had, with the health
/// status it had.
#[tokio::test]
async fn an_unsatisfiable_request_is_refused_and_writes_nothing() {
    let rows = [
        RefusalRow {
            case: "a status in the wrong case",
            unsatisfiable: Unsatisfiable::Status("healthy"),
        },
        RefusalRow {
            case: "a status in capitals",
            unsatisfiable: Unsatisfiable::Status("UNHEALTHY"),
        },
        RefusalRow {
            case: "a status that names no state of an instance",
            unsatisfiable: Unsatisfiable::Status("Degraded"),
        },
        RefusalRow {
            case: "the two statuses at once",
            unsatisfiable: Unsatisfiable::Status("Healthy,Unhealthy"),
        },
        RefusalRow {
            case: "an instance id the account holds no instance of",
            unsatisfiable: Unsatisfiable::Instance {
                instance_id: "i-0abcdef1234567890",
                runs_in_ec2: false,
            },
        },
        RefusalRow {
            case: "an instance of the account that is part of no group",
            unsatisfiable: Unsatisfiable::Instance {
                instance_id: "",
                runs_in_ec2: true,
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let name = format!("asg-refused-{index}");
        let held = group_holding(&router, &engine, &name, 1, None).await;
        let member = held[0].clone();

        let (instance_id, health_status, expected) = match &row.unsatisfiable {
            Unsatisfiable::Status(health_status) => (
                member.clone(),
                (*health_status).to_string(),
                // The sentence this service states for a `HealthStatus` outside the two
                // the member admits: it states what the member admits rather than what
                // the request sent, so it is the same sentence for every such value —
                // written out here rather than taken from the crate.
                "Valid instance health states are: [Healthy, Unhealthy].".to_string(),
            ),
            Unsatisfiable::Instance {
                instance_id,
                runs_in_ec2,
            } => {
                // An instance of the ACCOUNT that no group holds is refused exactly as an id
                // of nothing at all is: either way there is no managed instance of that id.
                let named = if *runs_in_ec2 {
                    let subnets = seed_subnets(&engine, 1);
                    standalone_instance(&router, &subnets[0].id).await
                } else {
                    (*instance_id).to_string()
                };
                let message = format!("Instance Id not found - Instance {named} not found");
                (named, UNHEALTHY.to_string(), message)
            }
        };

        let (status, body) =
            set_instance_health(&router, &instance_id, &health_status, None).await;
        assert_eq!(
            status, 400,
            "case {:?}: SetInstanceHealth expected 400, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {:?}: the refusal carries this service's own error code, got {body}",
            row.case
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            expected,
            "case {:?}: the refusal states the sentence its class carries, got {body}",
            row.case
        );

        // Nothing was written: the group's own member is where it was, with the health it
        // had.
        let reported = reported_member(&router, row.case, &name, &member).await;
        assert_eq!(
            reported.health_status, HEALTHY,
            "case {:?}: a refused request records no status, got {reported:?}",
            row.case
        );
        assert_eq!(
            reported.lifecycle_state, IN_SERVICE,
            "case {:?}: a refused request moves no member, got {reported:?}",
            row.case
        );
    }
}

/// Launch one standalone Amazon EC2 instance into `subnet_id` over Amazon EC2's own wire and
/// answer its id: an instance of the account that no Auto Scaling group holds.
async fn standalone_instance(router: &MultiServiceRouter, subnet_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "RunInstances",
        &[
            p("ImageId", IMAGE_ID),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "RunInstances expected 200, got {status}: {body}"
    );
    let instance_id = tag_value(&body, "instanceId");
    assert!(
        !instance_id.is_empty(),
        "RunInstances must report its instance, got {body}"
    );
    instance_id
}

/// The transition a hook "for scale-in events" states, and the `LifecycleState` an instance
/// waiting at such a hook is reported in — the model's own spellings
/// (`smithy_query(query="shape",shape="LifecycleState")`), written out here rather than
/// taken from the crate.
const TERMINATING_TRANSITION: &str = "autoscaling:EC2_INSTANCE_TERMINATING";
const TERMINATING_WAIT: &str = "Terminating:Wait";

/// The health check acts on a member exactly ONCE: a group whose termination hook holds the
/// member the check took out keeps that hold, and no later call takes the same member out
/// again.
///
/// The hold is what makes this observable: a held member is reported `Terminating:Wait` and
/// stays a member of the group for as long as the hold lasts, so a check that re-armed
/// itself would be seen taking it out over and over — a reporting call would then move state
/// on every poll.
#[tokio::test]
async fn the_health_check_acts_once_on_a_member_a_termination_hook_holds() {
    let case = "a termination hook holding the member the check took out";
    let (router, engine) = emulator();
    let name = "asg-health-held".to_string();
    let held = group_holding(&router, &engine, &name, 1, None).await;
    let target = held[0].clone();

    // A heartbeat far longer than this case runs for, so the hold itself never times out:
    // what is under test is the check, not the hook's timeout.
    let (status, code, message) = crate::lifecycle_hooks::put_hook(
        &router,
        &name,
        "hold-out",
        &[
            p("LifecycleTransition", TERMINATING_TRANSITION),
            p("HeartbeatTimeout", "7200"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the termination hook must be stored, got {status} with {code:?} \
         {message:?}"
    );

    record_health(&router, case, &target, UNHEALTHY, None).await;

    // The check acts: the member goes out of service and the hook holds it there.
    let deadline = Instant::now() + settle_deadline();
    loop {
        let reported = reported_member(&router, case, &name, &target).await;
        if reported.lifecycle_state == TERMINATING_WAIT {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the termination hook must hold {target} at {TERMINATING_WAIT}, last \
             read {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // Looked at AGAIN and again, over a window several times the span the check takes: the
    // hold stands, and the log still states ONE taking-out of this member.
    let until = Instant::now() + held_off_window();
    loop {
        let reported = reported_member(&router, case, &name, &target).await;
        assert_eq!(
            reported.lifecycle_state, TERMINATING_WAIT,
            "case {case:?}: the hold stands until it is resolved, got {reported:?}"
        );
        assert_eq!(
            activities_stating(&router, &name, TAKEN_OUT_BY_USER_HEALTH_CHECK).await,
            1,
            "case {case:?}: the check took {target} out once, not once per later call"
        );
        assert_eq!(
            activities_stating(&router, &name, LAUNCHED_TO_REPLACE_UNHEALTHY).await,
            1,
            "case {case:?}: and launched one replacement for it"
        );
        if Instant::now() >= until {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
