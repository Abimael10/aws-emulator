// SPDX-License-Identifier: CC0-1.0

//! A failed health check the group's own check DECLINES to act on, and the clock it leaves
//! behind.
//!
//! RULE: an instant this store reports as owed is one its materialising pass ACTS at — so a
//! replacement the group's health check is owed for a member it performs no check on, an
//! instance parked in `Standby` ("if you choose not to decrement the desired capacity of the
//! Auto Scaling group, the Auto Scaling group launches new instances to replace the instances
//! on standby", `EnterStandby`) or one already on its way out of the group, is NOT owed at an
//! instant while that condition holds. The debt itself stays on the instance and is acted on
//! as soon as the member is back in service, which is the rule
//! `all/group_state_and_standby_transitions.rs` reads from the other side; what must not
//! happen is a clock left waiting on an instant at which nothing happens, because the
//! service's own clock then wakes, retires nothing and waits again — for as long as the store
//! holds that member — competing for this store and for the stores it reads with every
//! request a client makes.
//!
//! The table spans the states a member can be in when a failed check falls due: in service
//! (the control, where the check acts), parked in `Standby`, and held at a termination hook on
//! its way out. The clock is read through the store's own reader
//! (`AutoScalingState::next_clock_due_at`, the one the service's clock waits on) because what
//! the rule is about is not in any response: two emulators that answer identically differ in
//! whether one of them is spinning.

use std::time::{Duration, Instant, SystemTime};

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_values};

/// The launch source every group here is created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// How long a leg waits for a condition the CLOCK owes, and how often it looks. Generous on
/// purpose: every wait below is on a condition, so a loaded host is given room to spend
/// rather than failed.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How many further reads a leg holds a settled clock over, so a transient "nothing is owed"
/// between two due instants cannot pass for a clock that has settled.
const HELD_READS: usize = 10;

/// What the member of one row is doing when the failed check falls due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// In service: the control, where the group's check ACTS and replaces it.
    InService,
    /// Parked in `Standby`, which the group performs no health check on.
    Parked,
    /// Held at a termination hook, already on its way out of the group.
    LeavingAtAHook,
}

/// One row of the table.
struct Row {
    case: &'static str,
    standing: Standing,
}

/// The instances one group reports, as `(id, lifecycle state, health status)`.
async fn members(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {group:?} back expected 200, got {status}: {body}"
    );
    let ids = tag_values(&body, "InstanceId");
    let states = tag_values(&body, "LifecycleState");
    let health = tag_values(&body, "HealthStatus");
    assert_eq!(
        (ids.len(), ids.len()),
        (states.len(), health.len()),
        "case {case:?}: every member reports a state and a health status, got {ids:?}, \
         {states:?} and {health:?}"
    );
    ids.into_iter()
        .zip(states)
        .zip(health)
        .map(|((id, state), status)| (id, state, status))
        .collect()
}

/// The instant the store is waiting on next, or `None` when it is waiting on nothing.
fn owed(engine: &std::sync::Arc<engine::InMemoryEngine>) -> Option<SystemTime> {
    autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine")
        .lock()
        .next_clock_due_at()
}

/// The instant the group's health check was recorded as owed at for one member, read off the
/// store: what the clock would be waiting for on that member's behalf.
fn recorded_check_due(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    group: &str,
    instance_id: &str,
) -> Option<SystemTime> {
    autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine")
        .lock()
        .groups
        .get(group)
        .and_then(|held| {
            held.instances
                .iter()
                .find(|instance| instance.instance_id == instance_id)
        })
        .and_then(|instance| instance.health_check_due.map(|debt| debt.due))
}

/// Poll one group until `settled` holds of the members it reports, and answer them.
async fn until<F>(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
    what: &str,
    settled: F,
) -> Vec<(String, String, String)>
where
    F: Fn(&[(String, String, String)]) -> bool,
{
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let reported = members(router, case, group).await;
        if settled(&reported) {
            return reported;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {what}, got {reported:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Create one group holding one instance, and answer that instance's id once it is in
/// service.
async fn group_with_one_member(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
    subnet: &str,
) -> String {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "1"),
            ("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating {group:?} expected 200, got {status}: {body}"
    );
    let reported = until(router, case, group, "one member in service", |members| {
        members.len() == 1 && members[0].1 == "InService"
    })
    .await;
    reported[0].0.clone()
}

/// Record `instance_id` unhealthy, without respecting the group's grace period, so the
/// replacement the check owes falls due on the service's own span rather than at the end of
/// that period.
async fn record_unhealthy(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    instance_id: &str,
) {
    let (status, body) = query(
        router,
        "SetInstanceHealth",
        &[
            ("InstanceId", instance_id),
            ("HealthStatus", "Unhealthy"),
            ("ShouldRespectGracePeriod", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: recording {instance_id:?} unhealthy expected 200, got {status}: {body}"
    );
}

/// Put the member of one row in the standing that row names.
async fn put_in_standing(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Row,
    group: &str,
    instance_id: &str,
) {
    match row.standing {
        Standing::InService => {}
        Standing::Parked => {
            let (status, body) = query(
                router,
                "EnterStandby",
                &[
                    ("AutoScalingGroupName", group),
                    ("InstanceIds.member.1", instance_id),
                    ("ShouldDecrementDesiredCapacity", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: parking {instance_id:?} expected 200, got {status}: {body}",
                row.case
            );
            until(router, row.case, group, "the member parked", |members| {
                members
                    .iter()
                    .any(|(id, state, _)| id == instance_id && state == "Standby")
            })
            .await;
        }
        Standing::LeavingAtAHook => {
            let (status, body) = query(
                router,
                "PutLifecycleHook",
                &borrowed(&[
                    p("AutoScalingGroupName", group),
                    p("LifecycleHookName", "hold-the-termination"),
                    p("LifecycleTransition", "autoscaling:EC2_INSTANCE_TERMINATING"),
                    p("DefaultResult", "CONTINUE"),
                    p("HeartbeatTimeout", "3600"),
                ]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: putting the termination hook expected 200, got {status}: {body}",
                row.case
            );
            let (status, body) = query(
                router,
                "TerminateInstanceInAutoScalingGroup",
                &[
                    ("InstanceId", instance_id),
                    ("ShouldDecrementDesiredCapacity", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: terminating {instance_id:?} expected 200, got {status}: {body}",
                row.case
            );
            until(
                router,
                row.case,
                group,
                "the member held at the termination hook",
                |members| {
                    members
                        .iter()
                        .any(|(id, state, _)| id == instance_id && state == "Terminating:Wait")
                },
            )
            .await;
        }
    }
}

/// A failed check the group declines to act on leaves the clock waiting on nothing, and the
/// one it can act on is acted on.
#[tokio::test]
async fn a_deferred_health_check_leaves_the_clock_nothing_to_wait_for() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);

    let rows = [
        Row {
            case: "the member is in service: the check acts",
            standing: Standing::InService,
        },
        Row {
            case: "the member is parked in Standby",
            standing: Standing::Parked,
        },
        Row {
            case: "the member is held at a termination hook",
            standing: Standing::LeavingAtAHook,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let group = format!("deferred-{index}");
        let instance_id = group_with_one_member(&router, row.case, &group, &subnets[0].id).await;
        put_in_standing(&router, row, &group, &instance_id).await;
        record_unhealthy(&router, row.case, &instance_id).await;

        if row.standing == Standing::InService {
            // The control: the group's check acts on the member, which leaves the group and
            // is replaced — so the debt is retired rather than deferred.
            let after = until(
                &router,
                row.case,
                &group,
                "the unhealthy member replaced",
                |members| {
                    members.len() == 1
                        && members[0].0 != instance_id
                        && members[0].1 == "InService"
                },
            )
            .await;
            assert_eq!(
                after.len(),
                1,
                "case {:?}: the group holds its replacement, got {after:?}",
                row.case
            );
            continue;
        }

        // The deferred rows: the check the group declines to act on has fallen due — the
        // instant the clock would have been waiting for has arrived and passed...
        let recorded = recorded_check_due(&engine, &group, &instance_id);
        let check_due_at = recorded.unwrap_or_else(|| {
            panic!(
                "case {:?}: the recorded check must leave the replacement owed on the \
                 instance, got {recorded:?}",
                row.case
            )
        });
        let giving_up_at = Instant::now() + DEADLINE;
        while SystemTime::now() <= check_due_at {
            assert!(
                Instant::now() < giving_up_at,
                "case {:?}: the recorded check never fell due, it is owed at {check_due_at:?}",
                row.case
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        // ...and the clock is left waiting on no instant that has already passed, over a run
        // of reads rather than one.
        for read in 0..HELD_READS {
            let reported = members(&router, row.case, &group).await;
            let waiting = owed(&engine);
            assert!(
                waiting.is_none_or(|due| due > SystemTime::now()),
                "case {:?}, read {read}: nothing may be owed at an instant already past, got \
                 {waiting:?} with the members {reported:?}: a clock waiting on an instant \
                 nothing retires never settles",
                row.case
            );
            // And the member the check declines to act on is left exactly where it is.
            assert!(
                reported
                    .iter()
                    .any(|(id, _, health)| id == &instance_id && health == "Unhealthy"),
                "case {:?}, read {read}: the member keeps the status the call recorded, got \
                 {reported:?}",
                row.case
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        if row.standing == Standing::Parked {
            // The debt was DEFERRED, not dropped: the member comes back into service and the
            // check that was owed for it acts then.
            let (status, body) = query(
                &router,
                "ExitStandby",
                &[
                    ("AutoScalingGroupName", group.as_str()),
                    ("InstanceIds.member.1", instance_id.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: returning {instance_id:?} to service expected 200, got {status}: \
                 {body}",
                row.case
            );
            until(
                &router,
                row.case,
                &group,
                "the returned member acted on by the check it was owed",
                |members| members.iter().all(|(id, _, _)| id != &instance_id),
            )
            .await;
        }
    }
}
