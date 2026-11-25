// SPDX-License-Identifier: CC0-1.0

//! An abandoned LAUNCH never serves, and the group launches another instance for the
//! capacity it was launched for.
//!
//! RULE under test: `CompleteLifecycleAction`'s `LifecycleActionResult` says "whether the
//! instance should continue to launch or terminate" and takes "either `CONTINUE` or
//! `ABANDON`"
//! (`smithy_query(query="doc",shape="CompleteLifecycleActionType",member="LifecycleActionResult")`),
//! so a launch a hook ABANDONS terminates instead of coming into service: the instance
//! never reports `InService`, the group hands it back to Amazon EC2, and the capacity it
//! was launched for is still the capacity the group is configured for — "Amazon EC2 Auto
//! Scaling launches instances until the desired capacity is totally fulfilled"
//! (`smithy_query(query="doc",shape="LaunchTemplateOverrides",member="WeightedCapacity")`)
//! — so the group launches another instance in its place, which the same hook then holds in
//! turn.
//!
//! The rows span what that rule generalises over: HOW the launch was abandoned (a callback
//! naming the action's token, a callback naming the instance, and the hook's own deadline
//! under a `DefaultResult` of `ABANDON`) and how much capacity the group is configured for,
//! so an abandoned launch out of several in flight leaves the others waiting where they
//! are.
//!
//! Auto Scaling and Amazon EC2 are mounted on ONE engine and driven over their own wire
//! protocols, so the instance this suite watches leave is the account's own. Nothing here
//! sleeps for a span: every wait looks again until the condition it needs holds.

use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{borrowed, ec2_query, emulator, p, query, seed_subnets, tag_value, tag_values};

/// The launch source every group here launches from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-abandoned-launch";

/// The transition the hooks in this suite are created for, written out here (and not taken
/// from the crate) so a wrong constant in the implementation cannot make a row pass. From
/// the model's own documentation for `PutLifecycleHookType$LifecycleTransition`.
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";

/// The two results a hook may default to and a callback may carry ("Valid values: CONTINUE
/// | ABANDON").
const CONTINUE: &str = "CONTINUE";
const ABANDON: &str = "ABANDON";

/// The `LifecycleState` values this suite asserts, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum does
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const PENDING_WAIT: &str = "Pending:Wait";
const IN_SERVICE: &str = "InService";

/// What Amazon EC2 reports for an instance it has finished shutting down, as EC2 names it
/// on the wire.
const EC2_TERMINATED: &str = "terminated";

/// How long a wait waits for what it is waiting on, and how often it looks: driven off what
/// the group REPORTS rather than off a fixed sleep, and far above the work one capacity
/// change makes, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

/// How the hold on the launch was abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandon {
    /// `CompleteLifecycleAction` naming the action's own token.
    Token,
    /// `CompleteLifecycleAction` naming the instance instead.
    Instance,
    /// Nothing: the hook's own deadline applies its `DefaultResult`, which is `ABANDON`
    /// for that row's hook.
    Deadline,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The capacity the group is configured for, which is how many launches the hook holds.
    capacity: i32,
    /// How the hold on the first of them was abandoned.
    abandon: Abandon,
}

const ROWS: &[Row] = &[
    Row {
        case: "one launch, abandoned by a callback naming the action's token",
        capacity: 1,
        abandon: Abandon::Token,
    },
    Row {
        case: "one launch, abandoned by a callback naming the instance",
        capacity: 1,
        abandon: Abandon::Instance,
    },
    Row {
        case: "three launches, one of them abandoned by a callback",
        capacity: 3,
        abandon: Abandon::Instance,
    },
    Row {
        case: "two launches, one abandoned by the hook's own deadline",
        capacity: 2,
        abandon: Abandon::Deadline,
    },
];

/// One emulator with Auto Scaling and Amazon EC2 on one engine, a launch template the
/// groups launch from and one subnet to place into.
async fn fixture() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, String) {
    let (router, engine) = emulator();
    crate::support::create_launch_template(&router, TEMPLATE).await;
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    (router, engine, subnet)
}

/// Create one group holding `capacity` instances behind a launching hook whose
/// `DefaultResult` is `default_result`, so every launch it makes is held.
async fn create_group(router: &MultiServiceRouter, row: &Row, group: &str, subnet: &str) {
    // A hook whose deadline is what abandons the launch defaults to ABANDON; one a caller
    // abandons by hand defaults to CONTINUE, so the row's own callback is what ends the
    // hold rather than a deadline nothing waited for.
    let default_result = if row.abandon == Abandon::Deadline {
        ABANDON
    } else {
        CONTINUE
    };
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", (row.capacity + 1).to_string()),
        p("DesiredCapacity", row.capacity.to_string()),
        p("VPCZoneIdentifier", subnet),
        p("LifecycleHookSpecificationList.member.1.LifecycleHookName", HOOK),
        p(
            "LifecycleHookSpecificationList.member.1.LifecycleTransition",
            LAUNCHING,
        ),
        p(
            "LifecycleHookSpecificationList.member.1.DefaultResult",
            default_result,
        ),
        p(
            "LifecycleHookSpecificationList.member.1.HeartbeatTimeout",
            "3600",
        ),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
        row.case
    );
}

/// The hook every group in this suite holds its launches behind.
const HOOK: &str = "admit";

/// The instances one group reports, each with the lifecycle state it is in.
async fn group_members(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
        .into_iter()
        .zip(tag_values(&body, "LifecycleState"))
        .collect()
}

/// Look again until the group reports `wanted` instances in `state`, and answer their ids.
async fn poll_members_in(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let members = group_members(router, group).await;
        let matching: Vec<String> = members
            .iter()
            .filter(|(_, reported)| reported == state)
            .map(|(instance_id, _)| instance_id.clone())
            .collect();
        if matching.len() == wanted {
            return matching;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected {wanted} instance(s) of {group} reported {state:?}, \
             last saw {members:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What Amazon EC2 reports as the state of one instance.
async fn ec2_state(router: &MultiServiceRouter, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_id} expected 200, got {status}: {body}"
    );
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// The `(instance id, token)` of every hold the group has taken, read from the store
/// because a token is handed to the client through the hook's notification target rather
/// than through an API read.
fn held_tokens(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    group: &str,
) -> Vec<(String, String)> {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let store = state.lock();
    let Some(stored) = store.groups.get(group) else {
        return Vec::new();
    };
    stored
        .instances
        .iter()
        .flat_map(|instance| {
            instance
                .pending_lifecycle_actions
                .iter()
                .map(|action| (instance.instance_id.clone(), action.token.clone()))
                .collect::<Vec<(String, String)>>()
        })
        .collect()
}

/// Backdate the hold taken against ONE instance, so that hold's own deadline has passed.
///
/// The shortest `HeartbeatTimeout` the service accepts is far longer than a test may wait,
/// so the row that is about the hook's own deadline drives the CLOCK: the deadline the hold
/// stored is moved into the past and the next read materialises what that instant made due,
/// exactly as it would have on its own. Only that instance's hold is moved, so the launches
/// the row is not abandoning are left waiting where they are.
fn time_out_hold_of(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    group: &str,
    instance_id: &str,
) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let stored = store
        .groups
        .get_mut(group)
        .unwrap_or_else(|| panic!("the group {group:?} must be stored"));
    for instance in &mut stored
        .instances
        .iter_mut()
        .filter(|instance| instance.instance_id == instance_id)
    {
        for action in &mut instance.pending_lifecycle_actions {
            action.heartbeat_due = std::time::SystemTime::UNIX_EPOCH;
            action.global_due = std::time::SystemTime::UNIX_EPOCH;
        }
    }
}

/// Every row: the abandoned launch never serves, leaves the group and is terminated where
/// it lives, while the group launches another instance for the capacity it was launched
/// for.
async fn the_abandoned_launch_rule_holds_for(index: usize, row: &Row) {
    let (router, engine, subnet) = fixture().await;
    let group = format!("asg-abandoned-launch-{index}");
    create_group(&router, row, &group, &subnet).await;

    let waiting = usize::try_from(row.capacity).expect("a capacity is a count");
    let held = poll_members_in(&router, row.case, &group, PENDING_WAIT, waiting).await;
    let subject = held
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {:?}: a held launch must be reported", row.case));

    match row.abandon {
        Abandon::Deadline => time_out_hold_of(&engine, &group, &subject),
        Abandon::Token => {
            let token = held_tokens(&engine, &group)
                .into_iter()
                .find(|(instance_id, _)| *instance_id == subject)
                .map(|(_, token)| token)
                .unwrap_or_else(|| panic!("case {:?}: the hold must carry a token", row.case));
            complete(
                &router,
                row,
                &group,
                &[
                    ("LifecycleActionToken", token.as_str()),
                    ("LifecycleActionResult", ABANDON),
                ],
            )
            .await;
        }
        Abandon::Instance => {
            complete(
                &router,
                row,
                &group,
                &[
                    ("InstanceId", subject.as_str()),
                    ("LifecycleActionResult", ABANDON),
                ],
            )
            .await;
        }
    }

    // The abandoned instance leaves the group WITHOUT ever being reported in service, and
    // the group launches another instance for the capacity it was launched for: the hook
    // holds that one in turn, so the group is back to holding every launch it is
    // configured for.
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let members = group_members(&router, &group).await;
        for (instance_id, state) in &members {
            assert_ne!(
                (instance_id.as_str(), state.as_str()),
                (subject.as_str(), IN_SERVICE),
                "case {:?}: an abandoned launch must never come into service, got {members:?}",
                row.case
            );
        }
        let gone = !members
            .iter()
            .any(|(instance_id, _)| *instance_id == subject);
        let replaced = members
            .iter()
            .filter(|(instance_id, state)| state == PENDING_WAIT && *instance_id != subject)
            .count();
        if gone && replaced == waiting {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {:?}: expected {subject} gone and {waiting} launch(es) held in its place, \
             last saw {members:?}",
            row.case
        );
        tokio::time::sleep(POLL).await;
    }

    // Amazon EC2 has the instance the group gave back.
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let state = ec2_state(&router, &subject).await;
        if state == EC2_TERMINATED {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {:?}: expected Amazon EC2 to report {subject} {EC2_TERMINATED:?}, \
             last saw {state:?}",
            row.case
        );
        tokio::time::sleep(POLL).await;
    }

    // Every launch the row did not abandon is still waiting where it was, and the hold the
    // abandoned one carried is gone.
    let holds = held_tokens(&engine, &group);
    assert!(
        !holds
            .iter()
            .any(|(instance_id, _)| *instance_id == subject),
        "case {:?}: the abandoned action must not still be held, got {holds:?}",
        row.case
    );
    for kept in held.iter().filter(|instance_id| **instance_id != subject) {
        assert!(
            holds.iter().any(|(instance_id, _)| instance_id == kept),
            "case {:?}: {kept} was not abandoned, so its hold stands, got {holds:?}",
            row.case
        );
    }
}

/// `CompleteLifecycleAction` on one hold of this suite's hook, asserted served.
async fn complete(
    router: &MultiServiceRouter,
    row: &Row,
    group: &str,
    address: &[(&str, &str)],
) {
    let mut params = vec![
        ("AutoScalingGroupName", group),
        ("LifecycleHookName", HOOK),
    ];
    params.extend_from_slice(address);
    let (status, body) = query(router, "CompleteLifecycleAction", &params).await;
    assert_eq!(
        status, 200,
        "case {:?}: CompleteLifecycleAction expected 200, got {status}: {body}",
        row.case
    );
}

/// One test per row, so the rows run beside each other.
#[tokio::test]
async fn a_launch_abandoned_by_its_token_is_terminated_and_replaced() {
    the_abandoned_launch_rule_holds_for(0, &ROWS[0]).await;
}

#[tokio::test]
async fn a_launch_abandoned_by_its_instance_id_is_terminated_and_replaced() {
    the_abandoned_launch_rule_holds_for(1, &ROWS[1]).await;
}

#[tokio::test]
async fn abandoning_one_of_several_launches_leaves_the_others_waiting() {
    the_abandoned_launch_rule_holds_for(2, &ROWS[2]).await;
}

#[tokio::test]
async fn a_launch_abandoned_by_the_hooks_own_deadline_is_terminated_and_replaced() {
    the_abandoned_launch_rule_holds_for(3, &ROWS[3]).await;
}
