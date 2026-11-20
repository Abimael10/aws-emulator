// SPDX-License-Identifier: CC0-1.0

//! The cooldown a group is left in FOLLOWS a scaling activity, so a change that moved no
//! capacity leaves none behind.
//!
//! The RULE under test: **the cooldown period is what "helps to ensure that your Auto Scaling
//! group doesn't launch or terminate additional instances before the previous scaling activity
//! takes effect" (`smithy_query(query="doc",shape="AutoScalingGroup",member="DefaultCooldown")`),
//! so it is started by a change that MOVED the group's desired capacity. A call that leaves the
//! capacity exactly where it already was — a group created at the capacity it asks for, a
//! request that re-states the capacity the group holds — is no scaling activity, so it starts
//! no cooldown and an execution that honours the group's cooldown is served straight away.**
//!
//! What the cooldown then blocks is pinned by `execute_policy.rs`; what this suite pins is
//! WHICH calls start one at all. The rows differ along what the rule generalises over: whether
//! the capacity moved, how it was moved (the group's own writer or a scaling policy), and in
//! which direction.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The launch source every group here is created from — a launch TEMPLATE the shared harness
/// seeds into the account's own Amazon EC2 store.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// A cooldown far longer than a test can run, so what the suite observes is decided by the
/// behaviour under test rather than by how fast the host is.
const LONG_COOLDOWN: i32 = 3_600;

/// The sentence an execution blocked by a running cooldown is refused with: fixed text.
const COOLDOWN_IN_PROGRESS_MESSAGE: &str =
    "Scaling activity is in progress and blocks the execution of this policy";

/// Create one group with a cooldown far longer than the test, at the capacity `desired`.
async fn create_group(router: &MultiServiceRouter, case: &str, name: &str, desired: i32) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", desired.to_string()),
        p("DefaultCooldown", LONG_COOLDOWN.to_string()),
        p("AvailabilityZones.member.1", format!("{}a", crate::support::REGION)),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be created, got {status} with {body}"
    );
}

/// Put one `ExactCapacity` simple policy on `group` that asks for `capacity`.
async fn put_policy(router: &MultiServiceRouter, case: &str, group: &str, capacity: i32) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", "exact"),
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", "ExactCapacity"),
        p("ScalingAdjustment", capacity.to_string()),
    ];
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the policy must be put, got {status} with {body}"
    );
}

/// Set the group's desired capacity through its own writer, which honours no cooldown.
async fn set_capacity(router: &MultiServiceRouter, case: &str, group: &str, desired: i32) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("DesiredCapacity", desired.to_string()),
    ];
    let (status, body) = query(router, "SetDesiredCapacity", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the capacity must be set, got {status} with {body}"
    );
}

/// Suspend `Launch` and `Terminate` on the group, so the capacities these rows move through
/// provision nothing: what is under test is the cooldown, not the instances.
async fn suspend_instance_processes(router: &MultiServiceRouter, case: &str, group: &str) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("ScalingProcesses.member.1", "Launch"),
        p("ScalingProcesses.member.2", "Terminate"),
    ];
    let (status, body) = query(router, "SuspendProcesses", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the processes must be suspended, got {status} with {body}"
    );
}

/// Ask for the policy once, honouring the group's cooldown, and answer whether the service
/// served it — checking that an ask it did not serve is the cooldown refusal and nothing else.
async fn honoured_execution_is_served(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> bool {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", "exact"),
        p("HonorCooldown", "true"),
    ];
    let (status, body) = query(router, "ExecutePolicy", &borrowed(&params)).await;
    if status == 200 {
        return true;
    }
    assert_eq!(
        status, 400,
        "case {case:?}: an ask the service does not serve must be the cooldown refusal, got \
         {status} with {body}"
    );
    assert!(
        body.contains("<Code>ScalingActivityInProgress</Code>"),
        "case {case:?}: expected the ScalingActivityInProgress code, got {body}"
    );
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert_eq!(
        message, COOLDOWN_IN_PROGRESS_MESSAGE,
        "case {case:?}: expected the cooldown sentence, got {message:?}"
    );
    false
}

/// How the capacity of a row's group stands before the honoured execution is asked for.
#[derive(Debug, Clone, Copy)]
enum Prepared {
    /// Nothing beyond the create.
    AsCreated,
    /// The group's own writer was given the capacity it already holds.
    CapacityRestated,
    /// The group's own writer moved the capacity to this value.
    CapacityMoved(i32),
}

#[tokio::test]
async fn only_a_change_that_moved_the_capacity_starts_the_groups_cooldown() {
    // (case, the capacity the group is created at, what is then done to it, whether an
    //  execution honouring the group's cooldown is served)
    let rows: &[(&str, i32, Prepared, bool)] = &[
        (
            "a group created at zero has run no activity",
            0,
            Prepared::AsCreated,
            true,
        ),
        (
            "a group created AT a capacity has: the create moved it",
            2,
            Prepared::AsCreated,
            false,
        ),
        (
            "re-stating the capacity the group holds moves nothing",
            0,
            Prepared::CapacityRestated,
            true,
        ),
        (
            "re-stating a capacity the group was created with moves nothing either",
            2,
            // The create already started one, and the restatement must not be what a row
            // mistakes for it: this row is served only once that first cooldown is gone, so
            // it asserts the refusal the create left.
            Prepared::CapacityRestated,
            false,
        ),
        (
            "a scale-out through the group's own writer is an activity",
            0,
            Prepared::CapacityMoved(3),
            false,
        ),
        (
            "a scale-in through the group's own writer is one too",
            3,
            Prepared::CapacityMoved(1),
            false,
        ),
    ];

    for (index, (case, created, prepared, served)) in rows.iter().enumerate() {
        let engine = new_engine();
        let router = router_with(engine);
        let group = format!("cooldown-{index}");

        create_group(&router, case, &group, *created).await;
        suspend_instance_processes(&router, case, &group).await;
        put_policy(&router, case, &group, created + 1).await;
        match prepared {
            Prepared::AsCreated => {}
            Prepared::CapacityRestated => set_capacity(&router, case, &group, *created).await,
            Prepared::CapacityMoved(to) => set_capacity(&router, case, &group, *to).await,
        }

        let answered = honoured_execution_is_served(&router, case, &group).await;
        assert_eq!(
            answered, *served,
            "case {case:?}: expected the honoured execution to be served = {served}, got \
             {answered}"
        );

        // Whatever the cooldown did, the capacity a served execution asked for is what the
        // group then holds, and a refused one left it where it was.
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status} with {body}"
        );
        let before = match prepared {
            Prepared::CapacityMoved(to) => *to,
            _ => *created,
        };
        let expected = if answered { created + 1 } else { before };
        let reported = tag_value(&body, "DesiredCapacity");
        assert_eq!(
            reported,
            expected.to_string(),
            "case {case:?}: expected DesiredCapacity {expected}, got {reported}"
        );
    }
}
