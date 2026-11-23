// SPDX-License-Identifier: CC0-1.0

//! Which HALF of a group's handling of an unhealthy member each suspended process stops.
//!
//! RULE: the health of a group's members is two processes, and a suspension stops exactly
//! what the process it names does — `HealthCheck` is what REACHES the verdict (it marks a
//! member unhealthy when the member's Amazon EC2 state, or the load balancer checking it,
//! says it is out of service) and `ReplaceUnhealthy` is what ACTS on that verdict (it
//! terminates the member and launches a replacement for the capacity it was providing). So a
//! group that has suspended the replacement alone goes on reporting the member it found
//! unhealthy — `Instances[].LifecycleState` the state that member already held, with
//! `Instances[].HealthStatus` `Unhealthy`
//! (`smithy_query(query="operation",operation="DescribeAutoScalingGroups")`) — at the
//! `DesiredCapacity` it was holding and with nothing taken out or launched for it, while a
//! group that has suspended the check alone reaches no verdict to act on and reports the
//! member exactly as it stood. A status a CALLER records with `SetInstanceHealth` is that
//! caller's own check rather than either process, so it is recorded whatever the group has
//! suspended.
//!
//! The table spans what the rule generalises over: WHICH check reaches the verdict (the
//! Amazon EC2 state change that leaves a member no longer running, and a status a caller
//! records) crossed with WHICH processes are suspended (neither — the control that is
//! replaced — the replacement alone, the check alone, and the whole published catalog, which
//! a request naming no process suspends). Every row is driven end to end over the awsQuery
//! wire against one router, and read back through `DescribeAutoScalingGroups` and
//! `DescribeScalingActivities`, where a client reads them.
//!
//! "Nothing further happens" is asserted on the STORE'S OWN CLOCK rather than on the wall
//! clock: a group that holds a member it will neither take out nor replace is waiting on no
//! instant at all, so every wait below looks again until the condition it needs holds, and
//! never for a fixed span.

use std::time::{Duration, Instant, SystemTime};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, list_members, p, query, seed_subnets,
    tag_value, xml_unescape, SeededSubnet,
};

/// The `LifecycleState` a member that is in service is reported in, and the two
/// `HealthStatus` values a member is reported with — the pair `SetInstanceHealth$HealthStatus`
/// names ("`Healthy`", "`Unhealthy`").
const LIFECYCLE_IN_SERVICE: &str = "InService";
const HEALTH_STATUS_HEALTHY: &str = "Healthy";
const HEALTH_STATUS_UNHEALTHY: &str = "Unhealthy";

/// The processes a row suspends, as `ProcessType$ProcessName` names them.
const PROCESS_HEALTH_CHECK: &str = "HealthCheck";
const PROCESS_REPLACE_UNHEALTHY: &str = "ReplaceUnhealthy";

/// The capacity every group here holds: one member, which is what makes the member the whole
/// of the group's capacity.
const DESIRED_CAPACITY: &str = "1";

/// How long a wait looks again for the condition it needs. Generous on purpose — many times
/// the spans this service moves an instance over — because what is asserted is that the
/// condition is reached at all, never how fast the host runs.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// How often it looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which check reaches the verdict that the group's one member is out of service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// Amazon EC2 no longer runs the member: a caller of that service's own
    /// `TerminateInstances` took it away under the group.
    Ec2StateChange,
    /// A caller recorded the member `Unhealthy` with `SetInstanceHealth`.
    CallerRecordedStatus,
}

/// What the group does with its member once that check has been made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The member stays listed, in the lifecycle state it held, reported `Unhealthy`, and
    /// nothing is taken out or launched for it.
    HeldUnhealthy,
    /// The group reaches no verdict at all: the member stays listed exactly as it stood.
    HeldHealthy,
    /// The member is terminated and a replacement is brought into service.
    Replaced,
}

/// One row: which check condemns the member, the processes suspended before it is made, and
/// what the group then does.
struct Row {
    case: &'static str,
    check: Check,
    /// The processes `SuspendProcesses` names. `None` makes no `SuspendProcesses` call at
    /// all, so the group has suspended nothing; `Some(&[])` names no process, which suspends
    /// every process of the published catalog ("if you omit this property, all processes are
    /// specified", `ScalingProcessQuery$ScalingProcesses`).
    suspend: Option<&'static [&'static str]>,
    expect: Expect,
}

/// One member of a group, as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    instance_id: String,
    lifecycle_state: String,
    health_status: String,
}

/// What one group reports: the capacity it holds and the members it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    desired_capacity: String,
    members: Vec<Member>,
}

/// Read one group back over the awsQuery wire.
async fn reported(router: &MultiServiceRouter, case: &str, name: &str) -> Reported {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    Reported {
        desired_capacity: tag_value(&body, "DesiredCapacity"),
        members: list_members(&body, "Instances")
            .into_iter()
            .map(|member| Member {
                instance_id: tag_value(member, "InstanceId"),
                lifecycle_state: tag_value(member, "LifecycleState"),
                health_status: tag_value(member, "HealthStatus"),
            })
            .collect(),
    }
}

/// Every scaling activity the group has recorded, as `(Description, Cause)`.
async fn activities(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .map(|member| {
            (
                xml_unescape(&tag_value(member, "Description")),
                xml_unescape(&tag_value(member, "Cause")),
            )
        })
        .collect()
}

/// The instant the Auto Scaling store is waiting on next, or `None` when it is waiting on
/// nothing — which is what says that nothing further is going to happen to the group.
fn owed(engine: &std::sync::Arc<engine::InMemoryEngine>) -> Option<SystemTime> {
    autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine")
        .lock()
        .next_clock_due_at()
}

/// Create one group over the subnets given: one member, placed by subnet, with no grace period
/// holding a failed check off.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
) {
    let placement: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", DESIRED_CAPACITY),
        p("VPCZoneIdentifier", placement.join(",")),
        p("HealthCheckType", "EC2"),
        p("HealthCheckGracePeriod", "0"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Suspend `processes` for one group — every published process when the row names none.
async fn suspend(router: &MultiServiceRouter, case: &str, name: &str, processes: &[&str]) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            (*process).to_string(),
        ));
    }
    let (status, body) = query(router, "SuspendProcesses", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses for {name:?} expected 200, got {status}: {body}"
    );
}

/// Resume `processes` for one group.
async fn resume(router: &MultiServiceRouter, case: &str, name: &str, processes: &[&str]) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            (*process).to_string(),
        ));
    }
    let (status, body) = query(router, "ResumeProcesses", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ResumeProcesses for {name:?} expected 200, got {status}: {body}"
    );
}

/// Look again until the group holds exactly one member in service and the store is waiting on
/// nothing, and answer that member's id: the steady state every row starts from.
async fn settled_member(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
) -> String {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        let settled = held.members.len() == 1
            && held.members[0].lifecycle_state == LIFECYCLE_IN_SERVICE
            && owed(engine).is_none();
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected {name:?} to settle on one member in \
                 {LIFECYCLE_IN_SERVICE} with nothing owed on the clock, got {held:?} with \
                 {:?} owed",
                owed(engine)
            );
            return held.members[0].instance_id.clone();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Terminate one instance through Amazon EC2's OWN API — out of band, as far as the group is
/// concerned — so that service stops running a member the group still holds.
async fn terminate_in_ec2(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let (status, body) = ec2_query(
        router,
        "TerminateInstances",
        &[p("InstanceId.1", instance_id.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: TerminateInstances for {instance_id} expected 200, got {status}: {body}"
    );
}

/// Record the member `Unhealthy` through this service's own `SetInstanceHealth`, which is a
/// CALLER's check rather than one of the group's processes.
async fn record_unhealthy(router: &MultiServiceRouter, case: &str, instance_id: &str) {
    let (status, body) = query(
        router,
        "SetInstanceHealth",
        &[
            ("InstanceId", instance_id),
            ("HealthStatus", HEALTH_STATUS_UNHEALTHY),
            ("ShouldRespectGracePeriod", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetInstanceHealth for {instance_id} expected 200, got {status}: {body}"
    );
}

/// Make the check `check` names against the group's one member.
async fn make_the_check(router: &MultiServiceRouter, case: &str, check: Check, instance_id: &str) {
    match check {
        Check::Ec2StateChange => terminate_in_ec2(router, case, instance_id).await,
        Check::CallerRecordedStatus => record_unhealthy(router, case, instance_id).await,
    }
}

/// Look again until the group reports its one member with the health status given, and answer
/// what it then reports.
async fn member_reported_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
    health_status: &str,
) -> Reported {
    let wanted = Member {
        instance_id: instance_id.to_string(),
        lifecycle_state: LIFECYCLE_IN_SERVICE.to_string(),
        health_status: health_status.to_string(),
    };
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        if held.members == vec![wanted.clone()] || Instant::now() >= deadline {
            assert_eq!(
                held.members,
                vec![wanted],
                "case {case:?}: expected {name:?} to list its member as the one in \
                 {LIFECYCLE_IN_SERVICE} reported {health_status}, got {:?}",
                held.members
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the group holds one member in service whose id is not `replaced`.
async fn replacement_of(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    replaced: &str,
) -> String {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        let fresh = (held.members.len() == 1
            && held.members[0].lifecycle_state == LIFECYCLE_IN_SERVICE
            && held.members[0].instance_id != replaced)
            .then(|| held.members[0].instance_id.clone());
        if fresh.is_some() || Instant::now() >= deadline {
            return fresh.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected {name:?} to replace {replaced} with another member \
                     in {LIFECYCLE_IN_SERVICE}, got {:?}",
                    held.members
                )
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that the group has recorded nothing beyond the launch of its original member: no
/// termination of it, and no second launch.
async fn only_the_original_launch(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) {
    let recorded = activities(router, case, name).await;
    let launches = recorded
        .iter()
        .filter(|(description, _)| description.starts_with("Launching"))
        .count();
    let terminations = recorded
        .iter()
        .filter(|(description, _)| description.starts_with("Terminating"))
        .count();
    assert_eq!(
        (launches, terminations),
        (1, 0),
        "case {case:?}: expected {name:?} to have recorded only the launch of {instance_id}, \
         got {recorded:?}"
    );
}

/// Drive one row end to end.
async fn drive(row: &Row, index: usize) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = format!("lt-held-{index}");
    create_launch_template(&router, &template).await;
    let name = format!("asg-held-{index}");

    create_group(&router, case, &name, &template, &subnets).await;
    let original = settled_member(&router, &engine, case, &name).await;
    if let Some(processes) = row.suspend {
        suspend(&router, case, &name, processes).await;
    }
    make_the_check(&router, case, row.check, &original).await;

    match row.expect {
        Expect::HeldUnhealthy => {
            // The verdict stands and the member stays: reported in the state it already held,
            // unhealthy, and still the whole of the group's capacity.
            let held =
                member_reported_as(&router, case, &name, &original, HEALTH_STATUS_UNHEALTHY).await;
            assert_eq!(
                held.desired_capacity, DESIRED_CAPACITY,
                "case {case:?}: expected {name:?} to hold the capacity it was created with, \
                 got {held:?}"
            );
            // And nothing is COMING: the store is waiting on no instant on this group's
            // behalf, so there is no replacement a later reader would see.
            let waiting = owed(&engine);
            assert_eq!(
                waiting, None,
                "case {case:?}: a group that will neither take the member out nor replace it \
                 waits on nothing, got {waiting:?} with {held:?}"
            );
            only_the_original_launch(&router, case, &name, &original).await;
            // A read reports state, it does not move it.
            let again = reported(&router, case, &name).await;
            assert_eq!(
                again, held,
                "case {case:?}: a second read of {name:?} reports the same group, got {again:?}"
            );
        }
        Expect::HeldHealthy => {
            // No verdict was reached at all, so the member is reported exactly as it stood —
            // and nothing is owed on its behalf either.
            let held =
                member_reported_as(&router, case, &name, &original, HEALTH_STATUS_HEALTHY).await;
            let waiting = owed(&engine);
            assert_eq!(
                waiting, None,
                "case {case:?}: a group that reaches no verdict about its member waits on \
                 nothing, got {waiting:?} with {held:?}"
            );
            only_the_original_launch(&router, case, &name, &original).await;
            let again = reported(&router, case, &name).await;
            assert_eq!(
                again, held,
                "case {case:?}: a second read of {name:?} reports the same group, got {again:?}"
            );
        }
        Expect::Replaced => {
            let replacement = replacement_of(&router, case, &name, &original).await;
            assert_ne!(
                replacement, original,
                "case {case:?}: the replacement of {original} is another instance"
            );
            let held = reported(&router, case, &name).await;
            assert_eq!(
                held.desired_capacity, DESIRED_CAPACITY,
                "case {case:?}: expected {name:?} to hold the capacity it was created with, \
                 got {held:?}"
            );
            assert!(
                held.members
                    .iter()
                    .all(|member| member.instance_id != original),
                "case {case:?}: expected {original} to have left {name:?}, got {held:?}"
            );
        }
    }
}

/// RULE: with `ReplaceUnhealthy` suspended, the member Amazon EC2 no longer runs stays listed
/// `InService` and reported `Unhealthy`, at the capacity the group holds and with no activity
/// beyond its original launch.
#[tokio::test]
async fn a_suspended_replacement_holds_an_ec2_terminated_member_unhealthy() {
    drive(
        &Row {
            case: "Amazon EC2 no longer runs the member, ReplaceUnhealthy suspended",
            check: Check::Ec2StateChange,
            suspend: Some(&[PROCESS_REPLACE_UNHEALTHY]),
            expect: Expect::HeldUnhealthy,
        },
        0,
    )
    .await;
}

/// The same rule for the other check that reaches the verdict: a status a CALLER recorded is
/// acted on by the same process, so a group that has suspended it holds that member too.
#[tokio::test]
async fn a_suspended_replacement_holds_a_caller_condemned_member_unhealthy() {
    drive(
        &Row {
            case: "a caller records the member Unhealthy, ReplaceUnhealthy suspended",
            check: Check::CallerRecordedStatus,
            suspend: Some(&[PROCESS_REPLACE_UNHEALTHY]),
            expect: Expect::HeldUnhealthy,
        },
        1,
    )
    .await;
}

/// A caller's own check is recorded whatever the group has suspended, so a group with
/// `HealthCheck` suspended still holds that member — the replacement is the other process, and
/// `HealthCheck` is suspended beside `ReplaceUnhealthy` here as the whole catalog.
#[tokio::test]
async fn every_process_suspended_holds_a_caller_condemned_member_unhealthy() {
    drive(
        &Row {
            case: "a caller records the member Unhealthy, every published process suspended",
            check: Check::CallerRecordedStatus,
            suspend: Some(&[]),
            expect: Expect::HeldUnhealthy,
        },
        2,
    )
    .await;
}

/// RULE: with `HealthCheck` suspended the group reaches no verdict from the member's Amazon
/// EC2 state, so the member is reported exactly as it stood — `Healthy` — and nothing is
/// replaced.
#[tokio::test]
async fn a_suspended_health_check_reaches_no_verdict_about_an_ec2_terminated_member() {
    drive(
        &Row {
            case: "Amazon EC2 no longer runs the member, HealthCheck suspended",
            check: Check::Ec2StateChange,
            suspend: Some(&[PROCESS_HEALTH_CHECK]),
            expect: Expect::HeldHealthy,
        },
        3,
    )
    .await;
}

/// And with every published process suspended — which a request naming none suspends — the
/// check is among them, so the group reaches no verdict from Amazon EC2 either.
#[tokio::test]
async fn every_process_suspended_reaches_no_verdict_about_an_ec2_terminated_member() {
    drive(
        &Row {
            case: "Amazon EC2 no longer runs the member, every published process suspended",
            check: Check::Ec2StateChange,
            suspend: Some(&[]),
            expect: Expect::HeldHealthy,
        },
        4,
    )
    .await;
}

/// The control the rule is read against: a group that has suspended NOTHING replaces the
/// member Amazon EC2 no longer runs.
#[tokio::test]
async fn an_unsuspended_group_replaces_an_ec2_terminated_member() {
    drive(
        &Row {
            case: "Amazon EC2 no longer runs the member, nothing suspended",
            check: Check::Ec2StateChange,
            suspend: None,
            expect: Expect::Replaced,
        },
        5,
    )
    .await;
}

/// And the control for the other check: a group that has suspended nothing replaces the member
/// a caller recorded unhealthy.
#[tokio::test]
async fn an_unsuspended_group_replaces_a_caller_condemned_member() {
    drive(
        &Row {
            case: "a caller records the member Unhealthy, nothing suspended",
            check: Check::CallerRecordedStatus,
            suspend: None,
            expect: Expect::Replaced,
        },
        6,
    )
    .await;
}

/// RULE, the other way round: a suspension withholds the replacement rather than cancelling
/// it, so a group whose `ReplaceUnhealthy` process is RESUMED performs the replacement that was
/// owed, with no further call from anybody.
#[tokio::test]
async fn resuming_the_replacement_performs_the_one_that_was_owed() {
    let case = "ReplaceUnhealthy resumed after the member was found unhealthy";
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = "lt-held-resume";
    create_launch_template(&router, template).await;
    let name = "asg-held-resume";

    create_group(&router, case, name, template, &subnets).await;
    let original = settled_member(&router, &engine, case, name).await;
    suspend(&router, case, name, &[PROCESS_REPLACE_UNHEALTHY]).await;
    terminate_in_ec2(&router, case, &original).await;
    member_reported_as(&router, case, name, &original, HEALTH_STATUS_UNHEALTHY).await;

    resume(&router, case, name, &[PROCESS_REPLACE_UNHEALTHY]).await;
    let replacement = replacement_of(&router, case, name, &original).await;
    assert_ne!(
        replacement, original,
        "case {case:?}: the replacement of {original} is another instance"
    );
    // The member it replaced left the group, and the group is back to the capacity it holds.
    let held = reported(&router, case, name).await;
    assert_eq!(
        held.desired_capacity, DESIRED_CAPACITY,
        "case {case:?}: expected {name:?} to hold the capacity it was created with, got {held:?}"
    );
    assert!(
        held.members
            .iter()
            .all(|member| member.instance_id != original),
        "case {case:?}: expected {original} to have left {name:?} once the replacement was \
         resumed, got {held:?}"
    );
}
