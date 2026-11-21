// SPDX-License-Identifier: CC0-1.0

//! The capacity a group with its `Launch` process suspended is asked to hold: recorded at
//! once, launched never, and settled the moment the call that asked for it is answered.
//!
//! RULE: a capacity change asked of a group that has suspended `Launch` RECORDS the capacity
//! the caller asked for and adds no instance — "if you suspend either the Launch or Terminate
//! process types, it can prevent other process types from functioning properly"
//! (`smithy_query(query="operation",operation="SuspendProcesses")`), and `Launch` is the
//! process that adds instances to a group — and the change is COMPLETE when the call is
//! answered: the next `DescribeAutoScalingGroups` reports the new `DesiredCapacity` with the
//! group's instances as they were, and the group is left owing the clock nothing, since
//! nothing is going to happen later. A group whose capacity nothing is ever going to move is
//! one a client can read a steady state off straight away, rather than one it has to watch
//! converge.
//!
//! The table below spans HOW the capacity is asked for, which is what the rule generalises
//! over — the writers that set a group's capacity (`SetDesiredCapacity`,
//! `UpdateAutoScalingGroup` by `DesiredCapacity` and by `MinSize` alone, which carries the
//! capacity up with it, and `ExecutePolicy`) — and WHICH suspensions are in force beside
//! `Launch` (it alone, it with `Terminate`, and the whole published catalog, which a request
//! naming no process suspends). Every row goes over the awsQuery wire through one router, and
//! each is read back twice: a read reports state, it does not move it.
//!
//! What a suspension IS and what a scale-in with `Terminate` suspended does are
//! `all/suspend_processes.rs`; this suite is about the group SETTLING on the capacity it was
//! asked to hold.

use std::time::SystemTime;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, emulator, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values,
};

/// The launch source every group here is created from: a launch TEMPLATE, because no account
/// can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// How the capacity of one row's group is raised, after its processes are suspended.
enum Raise {
    /// `SetDesiredCapacity`, naming the capacity.
    DesiredCapacity(i32),
    /// `UpdateAutoScalingGroup`, naming the capacity.
    UpdatedCapacity(i32),
    /// `UpdateAutoScalingGroup` naming only a new `MinSize` above the group's capacity, which
    /// "sets the group's DesiredCapacity to the new MinSize value"
    /// (`smithy_query(query="doc",shape="UpdateAutoScalingGroupType",member="MinSize")`).
    MinSize(i32),
    /// A simple scaling policy of this adjustment, performed with `ExecutePolicy`.
    PolicyAdjustment(i32),
}

/// One row: the suspensions in force, the way the capacity is raised, and the capacity the
/// group then holds.
struct Row {
    case: &'static str,
    /// The processes `SuspendProcesses` names; empty names every published process.
    suspend: Vec<&'static str>,
    raise: Raise,
    expected_capacity: i32,
}

/// Create one group over the subnets `placement` names, holding no capacity to start with.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    placement: &str,
) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", placement),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected 200, got {status}: {body}"
    );
}

/// Suspend `processes` for one group — every published process when it is empty.
async fn suspend(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
    processes: &[&str],
) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            (*process).to_string(),
        ));
    }
    let (status, body) = query(router, "SuspendProcesses", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: suspending expected 200, got {status}: {body}"
    );
}

/// Ask for `raise` on one group, through the writer that row names.
async fn ask_for_capacity(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
    raise: &Raise,
) {
    let (action, params) = match raise {
        Raise::DesiredCapacity(capacity) => (
            "SetDesiredCapacity",
            vec![
                p("AutoScalingGroupName", group),
                p("DesiredCapacity", capacity.to_string()),
            ],
        ),
        Raise::UpdatedCapacity(capacity) => (
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", group),
                p("DesiredCapacity", capacity.to_string()),
            ],
        ),
        Raise::MinSize(min_size) => (
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", group),
                p("MinSize", min_size.to_string()),
            ],
        ),
        Raise::PolicyAdjustment(adjustment) => {
            let (status, body) = query(
                router,
                "PutScalingPolicy",
                &borrowed(&[
                    p("AutoScalingGroupName", group),
                    p("PolicyName", "raise-capacity"),
                    p("PolicyType", "SimpleScaling"),
                    p("AdjustmentType", "ChangeInCapacity"),
                    p("ScalingAdjustment", adjustment.to_string()),
                ]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: putting the policy expected 200, got {status}: {body}"
            );
            (
                "ExecutePolicy",
                vec![
                    p("AutoScalingGroupName", group),
                    p("PolicyName", "raise-capacity"),
                ],
            )
        }
    };
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} expected 200, got {status}: {body}"
    );
}

/// What one group reports: its capacity, the instances it holds and the processes it has
/// suspended.
async fn reported(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
) -> (String, Vec<String>, Vec<String>) {
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
    (
        tag_value(&body, "DesiredCapacity"),
        tag_values(&body, "InstanceId"),
        tag_values(&body, "ProcessName"),
    )
}

/// The instant the store is waiting on next, or `None` when it is waiting on nothing.
fn owed(engine: &std::sync::Arc<engine::InMemoryEngine>) -> Option<SystemTime> {
    autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine")
        .lock()
        .next_clock_due_at()
}

/// The capacity a group with `Launch` suspended is asked to hold is recorded, launches
/// nothing, and is readable on the very next describe — whichever writer asked for it.
#[tokio::test]
async fn a_capacity_change_with_launch_suspended_settles_at_once() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let placement = format!("{},{}", subnets[0].id, subnets[1].id);

    let rows = [
        Row {
            case: "SetDesiredCapacity with Launch suspended",
            suspend: vec!["Launch"],
            raise: Raise::DesiredCapacity(2),
            expected_capacity: 2,
        },
        Row {
            case: "UpdateAutoScalingGroup naming the capacity",
            suspend: vec!["Launch"],
            raise: Raise::UpdatedCapacity(3),
            expected_capacity: 3,
        },
        Row {
            case: "UpdateAutoScalingGroup naming only a higher MinSize",
            suspend: vec!["Launch"],
            raise: Raise::MinSize(2),
            expected_capacity: 2,
        },
        Row {
            case: "a simple scaling policy performed on the group",
            suspend: vec!["Launch"],
            raise: Raise::PolicyAdjustment(2),
            expected_capacity: 2,
        },
        Row {
            case: "Launch suspended beside Terminate",
            suspend: vec!["Launch", "Terminate"],
            raise: Raise::DesiredCapacity(4),
            expected_capacity: 4,
        },
        Row {
            case: "every published process suspended",
            suspend: Vec::new(),
            raise: Raise::DesiredCapacity(1),
            expected_capacity: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let group = format!("intent-{index}");
        create_group(&router, &group, &placement).await;
        suspend(&router, row.case, &group, &row.suspend).await;
        ask_for_capacity(&router, row.case, &group, &row.raise).await;

        // The very NEXT read, with no waiting: the capacity is recorded and no instance was
        // added for it.
        let (capacity, instances, suspended) = reported(&router, row.case, &group).await;
        assert_eq!(
            capacity,
            row.expected_capacity.to_string(),
            "case {:?}: expected the group to hold the capacity it was asked for, got \
             {capacity:?} with the instances {instances:?}",
            row.case
        );
        assert!(
            instances.is_empty(),
            "case {:?}: a group whose Launch process is suspended adds no instance, got \
             {instances:?}",
            row.case
        );
        assert!(
            suspended.contains(&"Launch".to_string()),
            "case {:?}: the group must still report Launch suspended, got {suspended:?}",
            row.case
        );

        // And the change is COMPLETE: the group owes the clock nothing, so a client reading
        // again reads the same steady state rather than watching it converge.
        let waiting = owed(&engine);
        assert_eq!(
            waiting, None,
            "case {:?}: a capacity change that moves no instance leaves nothing owed on the \
             clock, got {waiting:?}",
            row.case
        );
        let again = reported(&router, row.case, &group).await;
        assert_eq!(
            again,
            (capacity, instances, suspended),
            "case {:?}: a second read reports the same group, got {again:?}",
            row.case
        );
    }
}

/// A run of capacity changes none of which can move an instance leaves the group holding the
/// last capacity asked for, its instances untouched and its clock waiting on nothing.
#[tokio::test]
async fn a_capacity_change_that_moves_nothing_owes_the_clock_nothing() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 2);
    let placement = format!("{},{}", subnets[0].id, subnets[1].id);
    let group = "intent-steady";

    create_group(&router, group, &placement).await;
    suspend(&router, "the steady group", group, &["Launch"]).await;

    // Up, back to the capacity the group started at, and up again.
    for capacity in [2, 0, 3] {
        ask_for_capacity(
            &router,
            "the steady group",
            group,
            &Raise::DesiredCapacity(capacity),
        )
        .await;
        let (reported_capacity, instances, _) = reported(&router, "the steady group", group).await;
        assert_eq!(
            reported_capacity,
            capacity.to_string(),
            "case {capacity}: expected the capacity the request set, got \
             {reported_capacity:?} with {instances:?}"
        );
        assert!(
            instances.is_empty(),
            "case {capacity}: no instance is added while Launch is suspended, got {instances:?}"
        );
        let waiting = owed(&engine);
        assert!(
            waiting.is_none_or(|due| due > SystemTime::now()),
            "case {capacity}: nothing may be owed at an instant already past, got {waiting:?}"
        );
    }
}
