// SPDX-License-Identifier: CC0-1.0

//! The teardown `DeleteAutoScalingGroup` documents CONVERGES: a delete that was not asked to
//! force itself ends in a deleted group.
//!
//! RULE: a delete without `ForceDelete` is refused `ResourceInUse` only while the group still
//! HOLDS something — a member that is staying, an instance its warm pool is keeping, or
//! capacity it still asks for; when the only thing left is a change the clock is still
//! carrying out (a member on its way out of the group, a capacity change waiting for a
//! cooldown) the refusal is the transient `ScalingActivityInProgress` the model declares for
//! the operation, and once that change has settled the delete succeeds. "If the group has
//! instances or scaling activities in progress, you must specify the option to force the
//! deletion in order for it to succeed. … To remove instances from the Auto Scaling group
//! before deleting it, call the DetachInstances API with the list of instances and the option
//! to decrement the desired capacity. … To terminate all instances before deleting the Auto
//! Scaling group, call the UpdateAutoScalingGroup API and set the minimum size and desired
//! capacity of the Auto Scaling group to zero"
//! (`smithy_query(query="doc",shape="DeleteAutoScalingGroup")`): both sequences AWS documents
//! for removing what a group holds end in a delete that is served, so neither may stand
//! refused with a code a client is not told to retry.
//!
//! The table spans what the rule generalises over: which documented sequence took the
//! instances out (the detach with the decrement, and the update of the minimum size and
//! desired capacity to zero), how many members it took out (one, two), whether a target group
//! is attached to the group — an attachment is not something that holds the group — and
//! whether the released instances were then terminated where they live, as a client tearing a
//! fixture down does.
//!
//! The rows below it pin the two refusals apart: a member nothing on the clock will take out
//! (one a termination lifecycle hook is holding) still HOLDS the group, so it is the standing
//! `ResourceInUse`; a capacity change waiting for a cooldown holds nothing, so it is the
//! transient `ScalingActivityInProgress`.
//!
//! Nothing here waits for a span: every wait looks AGAIN until the condition the clock owes
//! holds, or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, create_target_group, ec2_query, emulator, p, query,
    seed_subnets, tag_value, tag_values, vpc_of, xml_unescape, SeededSubnet,
};

/// How long a case here waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every loop below looks again until the condition holds, so this
/// only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The launch template every group here launches from.
const TEMPLATE: &str = "lt-teardown";

/// The `LifecycleState` values this suite reads, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum spells them
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const IN_SERVICE: &str = "InService";
const TERMINATING_WAIT: &str = "Terminating:Wait";

/// The transition a hook can hold a termination at, and the results a hook may default to and
/// a callback may carry ("Valid values: CONTINUE | ABANDON"), from the model's own
/// documentation for `PutLifecycleHookType$LifecycleTransition` and `$DefaultResult`.
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";
const CONTINUE: &str = "CONTINUE";

/// The `awsQueryError` code of each error class this operation models for a group it will not
/// delete, from the model's own `ResourceInUseFault` and `ScalingActivityInProgressFault`
/// (`smithy_query(query="operation",operation="DeleteAutoScalingGroup")`).
const RESOURCE_IN_USE: &str = "ResourceInUse";
const SCALING_ACTIVITY_IN_PROGRESS: &str = "ScalingActivityInProgress";

/// The sentence Amazon EC2 Auto Scaling states for a delete refused because the group still
/// holds instances. Written out here rather than taken from the crate, so a wrong sentence in
/// the implementation cannot make a row pass.
const IN_USE_SENTENCE: &str = "You cannot delete an AutoScalingGroup while there are instances \
                               or pending Spot instance request(s) still in the group.";

/// And the sentence it states for one refused because a scaling activity of the group is still
/// in progress.
const IN_PROGRESS_SENTENCE: &str = "You cannot delete an AutoScalingGroup while there are \
                                    scaling activities in progress for that group.";

/// The code and the unescaped message of one refusal.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// What `DescribeAutoScalingGroups` answers for one group.
async fn describe(router: &MultiServiceRouter, name: &str) -> String {
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
    body
}

/// Create one group placed in `subnet`, with room for `capacity` members and asking for all of
/// them.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &SeededSubnet,
    capacity: usize,
) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", capacity.max(1).to_string()),
        p("DesiredCapacity", capacity.to_string()),
        p("VPCZoneIdentifier", subnet.id.clone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Look again until the group reports exactly `wanted` members, every one of them in service,
/// and answer their ids.
async fn settled_members(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if ids.len() == wanted && states.iter().all(|state| state == IN_SERVICE) {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to report {wanted} member(s) in service, last read \
             {ids:?} in {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the group reports exactly `wanted` members in `state`, and answer their
/// ids.
async fn members_in_state(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let matching: Vec<String> = ids
            .iter()
            .zip(states.iter())
            .filter(|(_, held)| held.as_str() == state)
            .map(|(id, _)| id.clone())
            .collect();
        if matching.len() == wanted && ids.len() == wanted {
            return matching;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to report {wanted} member(s) in {state}, last read \
             {ids:?} in {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Post one `DeleteAutoScalingGroup`, forcing the deletion only when asked to.
async fn delete(router: &MultiServiceRouter, name: &str, force: bool) -> (u16, String) {
    let mut params = vec![("AutoScalingGroupName", name)];
    if force {
        params.push(("ForceDelete", "true"));
    }
    query(router, "DeleteAutoScalingGroup", &params).await
}

/// Post the delete again and again — as a client that retries the transient refusal does —
/// until the group is deleted, asserting that every refusal on the way is the TRANSIENT one.
///
/// Answers whether a transient refusal was seen at all: a host fast enough to settle the
/// change before the first retry arrives sees none, which is just as correct.
async fn delete_converges(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let deadline = Instant::now() + DEADLINE;
    let mut waited = false;
    loop {
        let (status, body) = delete(router, name, false).await;
        if status == 200 {
            return waited;
        }
        let (code, message) = refusal(&body);
        assert_eq!(
            status, 400,
            "case {case:?}: a delete of a group holding nothing must be served or refused \
             transiently, got {status}: {body}"
        );
        assert_eq!(
            code, SCALING_ACTIVITY_IN_PROGRESS,
            "case {case:?}: a group whose only remaining change is settling must answer \
             {SCALING_ACTIVITY_IN_PROGRESS}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, IN_PROGRESS_SENTENCE,
            "case {case:?}: expected the in-progress sentence, got {message:?}"
        );
        waited = true;
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the delete never converged; the group {name} is still answering \
             {code}: {message}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert the group is gone: the account no longer reports one under that name.
async fn assert_deleted(router: &MultiServiceRouter, case: &str, name: &str) {
    let body = describe(router, name).await;
    assert!(
        tag_values(&body, "AutoScalingGroupName").is_empty(),
        "case {case:?}: a deleted group must no longer be reported, got {body}"
    );
}

/// Which documented sequence a row takes the group's instances out with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Teardown {
    /// `DetachInstances` naming every member, with the decrement.
    Detach,
    /// `UpdateAutoScalingGroup` setting the minimum size and desired capacity to zero.
    UpdateToZero,
}

/// One row of the teardown table.
struct Row {
    case: &'static str,
    /// How many members the group holds before the teardown.
    members: usize,
    /// The documented sequence that takes them out.
    teardown: Teardown,
    /// Whether a target group is attached to the group while it is torn down.
    target_group: bool,
    /// Whether the instances a detach released are then terminated where they live, as a
    /// client tearing its own fixture down does.
    terminate_released: bool,
}

#[tokio::test]
async fn the_documented_teardown_ends_in_a_delete_that_is_served() {
    let rows = [
        Row {
            case: "one attached member detached with the decrement, then terminated, with a \
                   target group attached",
            members: 1,
            teardown: Teardown::Detach,
            target_group: true,
            terminate_released: true,
        },
        Row {
            case: "both members detached with the decrement and left running",
            members: 2,
            teardown: Teardown::Detach,
            target_group: false,
            terminate_released: false,
        },
        Row {
            case: "one member terminated by an update to zero",
            members: 1,
            teardown: Teardown::UpdateToZero,
            target_group: false,
            terminate_released: false,
        },
        Row {
            case: "both members terminated by an update to zero, with a target group attached",
            members: 2,
            teardown: Teardown::UpdateToZero,
            target_group: true,
            terminate_released: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, engine) = emulator();
        let subnet = seed_subnets(&engine, 1)
            .first()
            .expect("one seeded subnet")
            .clone();
        create_launch_template(&router, TEMPLATE).await;
        let name = format!("asg-teardown-{index}");
        create_group(&router, case, &name, &subnet, row.members).await;

        if row.target_group {
            let vpc = vpc_of(&router, &subnet).await;
            let target_group =
                create_target_group(&router, &format!("tg-teardown-{index}"), 80, "instance", &vpc)
                    .await;
            let (status, body) = query(
                &router,
                "AttachLoadBalancerTargetGroups",
                &[
                    ("AutoScalingGroupName", name.as_str()),
                    ("TargetGroupARNs.member.1", target_group.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: AttachLoadBalancerTargetGroups expected 200, got {status}: {body}"
            );
        }

        let members = settled_members(&router, case, &name, row.members).await;

        // While the group still holds its members the delete is refused with the STANDING
        // refusal: the caller has to take them out first, which is what the two sequences
        // below do.
        let (status, body) = delete(&router, &name, false).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a group holding {} member(s) must be refused, got {status}: {body}",
            row.members
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            code, RESOURCE_IN_USE,
            "case {case:?}: expected {RESOURCE_IN_USE}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, IN_USE_SENTENCE,
            "case {case:?}: expected the in-use sentence, got {message:?}"
        );

        match row.teardown {
            Teardown::Detach => {
                let mut params = vec![
                    p("AutoScalingGroupName", name.clone()),
                    p("ShouldDecrementDesiredCapacity", "true"),
                ];
                for (slot, instance_id) in members.iter().enumerate() {
                    params.push(p(
                        &format!("InstanceIds.member.{}", slot + 1),
                        instance_id.clone(),
                    ));
                }
                let (status, body) = query(&router, "DetachInstances", &borrowed(&params)).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DetachInstances expected 200, got {status}: {body}"
                );
                if row.terminate_released {
                    let terminating: Vec<(String, String)> = members
                        .iter()
                        .enumerate()
                        .map(|(slot, instance_id)| {
                            p(&format!("InstanceId.{}", slot + 1), instance_id.clone())
                        })
                        .collect();
                    let (status, body) =
                        ec2_query(&router, "TerminateInstances", &terminating).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: TerminateInstances expected 200, got {status}: {body}"
                    );
                }
            }
            Teardown::UpdateToZero => {
                let (status, body) = query(
                    &router,
                    "UpdateAutoScalingGroup",
                    &[
                        ("AutoScalingGroupName", name.as_str()),
                        ("MinSize", "0"),
                        ("DesiredCapacity", "0"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
                );
            }
        }

        // The delete issued straight after it: whatever is still settling, the answer is never
        // the standing refusal again, and a client that retries the transient one gets the
        // group deleted.
        delete_converges(&router, case, &name).await;
        assert_deleted(&router, case, &name).await;

        // A detach RELEASES rather than terminates, so an instance a row left alone is still
        // the account's own running instance after the group that held it is gone.
        if row.teardown == Teardown::Detach && !row.terminate_released {
            for instance_id in &members {
                let (status, body) =
                    ec2_query(&router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DescribeInstances({instance_id}) expected 200, got {status}: \
                     {body}"
                );
                let state = tag_value(&tag_value(&body, "instanceState"), "name");
                assert_eq!(
                    state, "running",
                    "case {case:?}: the released {instance_id} must still run, got {state:?}"
                );
            }
        }
    }
}

/// A group holding nothing at all is deleted on the first call: there is neither something to
/// take out of it nor anything to wait for.
#[tokio::test]
async fn an_empty_group_is_deleted_without_forcing() {
    let case = "a group that asks for no capacity and holds no instance";
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .clone();
    create_launch_template(&router, TEMPLATE).await;
    let name = "asg-teardown-empty";
    create_group(&router, case, name, &subnet, 0).await;

    let (status, body) = delete(&router, name, false).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status}: {body}"
    );
    assert_deleted(&router, case, name).await;
}

/// A member nothing on the clock will take out still HOLDS the group: a termination a
/// lifecycle hook is holding waits for a callback, not for the clock, so the delete is refused
/// with the standing refusal — and once the callback releases it, the delete converges.
#[tokio::test]
async fn a_member_a_lifecycle_hook_holds_still_holds_the_group() {
    let case = "a termination waiting at a lifecycle hook";
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .clone();
    create_launch_template(&router, TEMPLATE).await;
    let name = "asg-teardown-held";
    create_group(&router, case, name, &subnet, 1).await;
    let members = settled_members(&router, case, name, 1).await;
    let instance_id = members.first().expect("one member").clone();

    let hook = "hold-the-termination";
    let (status, body) = query(
        &router,
        "PutLifecycleHook",
        &[
            ("AutoScalingGroupName", name),
            ("LifecycleHookName", hook),
            ("LifecycleTransition", TERMINATING),
            ("DefaultResult", CONTINUE),
            ("HeartbeatTimeout", "300"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutLifecycleHook expected 200, got {status}: {body}"
    );

    // The documented update to zero, which this group's hook then holds the termination of.
    let (status, body) = query(
        &router,
        "UpdateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("MinSize", "0"),
            ("DesiredCapacity", "0"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let waiting = members_in_state(&router, case, name, TERMINATING_WAIT, 1).await;
    assert_eq!(
        waiting,
        vec![instance_id.clone()],
        "case {case:?}: expected {instance_id} to wait at the hook, got {waiting:?}"
    );

    // The group still holds it, so the refusal is the standing one however long a client waits:
    // only the callback (or the hook's own deadline) moves this termination on.
    let (status, body) = delete(&router, name, false).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status}: {body}"
    );
    let (code, message) = refusal(&body);
    assert_eq!(
        code, RESOURCE_IN_USE,
        "case {case:?}: expected {RESOURCE_IN_USE}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, IN_USE_SENTENCE,
        "case {case:?}: expected the in-use sentence, got {message:?}"
    );

    let (status, body) = query(
        &router,
        "CompleteLifecycleAction",
        &[
            ("AutoScalingGroupName", name),
            ("LifecycleHookName", hook),
            ("InstanceId", instance_id.as_str()),
            ("LifecycleActionResult", CONTINUE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CompleteLifecycleAction expected 200, got {status}: {body}"
    );
    delete_converges(&router, case, name).await;
    assert_deleted(&router, case, name).await;
}

/// A capacity change waiting for the group's cooldown holds nothing, so a delete is refused
/// with the TRANSIENT refusal — and the forcing form of the delete is not held up by it.
#[tokio::test]
async fn a_capacity_change_waiting_for_a_cooldown_is_a_transient_refusal() {
    let case = "a capacity change deferred until the cooldown has elapsed";
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .clone();
    create_launch_template(&router, TEMPLATE).await;
    let name = "asg-teardown-cooldown";
    // A cooldown long enough that the deferred change below is still waiting whatever the host
    // is doing: what the case is about is the WAIT, so nothing here depends on it elapsing.
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("DefaultCooldown", "100000"),
        p("VPCZoneIdentifier", subnet.id.clone()),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let members = settled_members(&router, case, name, 1).await;

    // The member out of the group first, so what is left is the deferred change alone.
    let (status, body) = query(
        &router,
        "DetachInstances",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", members[0].as_str()),
            ("ShouldDecrementDesiredCapacity", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetachInstances expected 200, got {status}: {body}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(&router, name).await;
        if tag_values(&body, "InstanceId").is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to hold no instance, got {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }

    // A capacity change the caller asked to honour the cooldown: the group is inside one, so
    // the change waits for it rather than being applied.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", name),
            ("DesiredCapacity", "0"),
            ("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected 200, got {status}: {body}"
    );

    let (status, body) = delete(&router, name, false).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status}: {body}"
    );
    let (code, message) = refusal(&body);
    assert_eq!(
        code, SCALING_ACTIVITY_IN_PROGRESS,
        "case {case:?}: expected {SCALING_ACTIVITY_IN_PROGRESS}, got {code:?} with {message:?}"
    );
    assert_eq!(
        message, IN_PROGRESS_SENTENCE,
        "case {case:?}: expected the in-progress sentence, got {message:?}"
    );
    let body = describe(&router, name).await;
    assert_eq!(
        tag_value(&body, "AutoScalingGroupName"),
        name,
        "case {case:?}: a refused delete leaves the group where it was, got {body}"
    );

    // And the forcing form is not held up by a change the group is waiting on.
    let (status, body) = delete(&router, name, true).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a forced delete expected 200, got {status}: {body}"
    );
    assert_deleted(&router, case, name).await;
}
