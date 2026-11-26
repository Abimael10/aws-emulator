// SPDX-License-Identifier: CC0-1.0

//! The sentence a lifecycle-action callback addressed by INSTANCE is refused with once the
//! group holds no active action for that instance, and what makes a hook hold nothing.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded case:
//!
//! 1. A call addressed at an instance the named group holds NO outstanding lifecycle action
//!    for is refused HTTP 400 with the Query code `ValidationError` and ONE fixed sentence
//!    whose only variable part is the request's own `InstanceId` — however the action stopped
//!    being active (a callback consumed it, whichever result it carried; the clock timed it
//!    out; the hook holding it was deleted; the instance or the group is one this account
//!    never held) and whatever hook the call names. Both doors that take the address,
//!    `CompleteLifecycleAction` and `RecordLifecycleActionHeartbeat`, refuse it identically,
//!    and the refusal leaves every hold the group does carry exactly as it was.
//! 2. The hook is named in that sentence in exactly one situation, which rule 1's rows are
//!    the complement of: the instance the call names IS still held by an outstanding action,
//!    just not one of the hook it named.
//! 3. Deleting a lifecycle hook ends the holds it had taken: "If there are any outstanding
//!    lifecycle actions, they are completed first (`ABANDON` for launching instances,
//!    `CONTINUE` for terminating instances)"
//!    (`smithy_query(query="doc",shape="DeleteLifecycleHook")`) — so from the instant a hook
//!    is removed the group holds no active action under it, the instance it was holding
//!    carries on as that documented result takes it, and a later callback naming the deleted
//!    hook is refused by rule 1 rather than by rule 2.

use smithy_server_runtime::MultiServiceRouter;

use crate::lifecycle_hook_execution::{
    complete, create_group, fixture, held_actions, poll_states, put_hook, refusal_of,
    reported_states, set_capacity, time_out_every_hold, ABANDON, CONTINUE, DEADLINE, IN_SERVICE,
    LAUNCHING, PENDING_WAIT, POLL, TERMINATING, TERMINATING_WAIT,
};
use crate::support::query;

/// The Query error code every semantic refusal of these two doors carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// Instance ids NO group here ever held, spanning the lengths the member admits: `InstanceId`
/// is `XmlStringMaxLen19` with `@length(min: 1, max: 19)`
/// (`smithy_query(query="operation",operation="RecordLifecycleActionHeartbeat")`), so the
/// shortest id a client can send, a middling one and one at the published maximum are all
/// quoted back by the same sentence.
const NEVER_HELD_INSTANCES: [&str; 3] = ["i-1", "i-abcdef0123456789", "i-0123456789abcdef0"];

/// The whole message a call addressed at an instance the group holds no active action for is
/// refused with: fixed text and the ONE variable part, the instance the request named.
///
/// Spelled out here rather than taken from the crate, so no row can pass on the handler's own
/// word for the sentence.
fn no_active_action_sentence(instance_id: &str) -> String {
    format!("No active Lifecycle Action found with instance ID {instance_id}")
}

/// The same sentence QUALIFIED by the hook, which is the one other form: for an instance that
/// IS still held, under a hook holding none of it.
fn hook_qualified_sentence(instance_id: &str, hook_name: &str) -> String {
    format!("No active Lifecycle Action found with instance ID {instance_id} and HookName {hook_name}")
}

/// Post one callback through `door`, addressed at `instance_id` under `hook`, and answer the
/// status and the `(code, message)` it was refused with.
async fn refused_callback(
    router: &MultiServiceRouter,
    case: &str,
    door: &str,
    (group, hook, instance_id): (&str, &str, &str),
) -> (u16, String, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", group),
        ("LifecycleHookName", hook),
        ("InstanceId", instance_id),
    ];
    if door == "CompleteLifecycleAction" {
        params.push(("LifecycleActionResult", CONTINUE));
    }
    let (status, body) = query(router, door, &params).await;
    let (code, message) = refusal_of(&body);
    assert_eq!(
        status, 400,
        "case {case:?} through {door}: expected HTTP 400, got {status} with {body:?}"
    );
    (status, code, message)
}

/// Look at the group until it no longer reports `instance_id` in `state`, or [`DEADLINE`]
/// passes.
///
/// What a row that is about an instance HAVING LEFT a wait state waits on: the condition
/// itself, looked at again until it holds, so a loaded or a fast host changes nothing but how
/// many looks it takes.
async fn poll_until_out_of(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
    state: &str,
) {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        if !instance_states(router, case, group).await.contains(&(
            instance_id.to_string(),
            state.to_string(),
        )) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {instance_id} must leave {state:?}, got {:?}",
            instance_states(router, case, group).await
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Look at the group until it reports no instance at all, or [`DEADLINE`] passes; answers the
/// states of its last read.
async fn poll_until_group_is_empty(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let states = reported_states(router, case, group).await;
        if states.is_empty() {
            return states;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group must end up holding no instance, got {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What the group reports for each instance it holds, as `(instance id, LifecycleState)`
/// pairs, read through the account-wide instance view so each state is paired with the
/// instance it belongs to.
async fn instance_states(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingInstances",
        &[("MaxRecords", "50")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's instances must be readable, got {status} with {body:?}"
    );
    crate::support::list_members(&body, "AutoScalingInstances")
        .into_iter()
        .filter(|member| crate::support::tag_value(member, "AutoScalingGroupName") == group)
        .map(|member| {
            (
                crate::support::tag_value(member, "InstanceId"),
                crate::support::tag_value(member, "LifecycleState"),
            )
        })
        .collect()
}

/// HOW the action the refused call addresses stopped being active — one row of rule 1 each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// A callback carried `CONTINUE` back, and the instance came into service.
    ConsumedByContinue,
    /// A callback carried `ABANDON` back, and the instance was handed back instead.
    ConsumedByAbandon,
    /// Nobody called back inside the time the hook allows, so the clock resolved it with the
    /// hook's own `DefaultResult`.
    TimedOut,
    /// The action was consumed and the hook that had held it was then deleted.
    HookDeletedAfterwards,
    /// The hook was deleted while it was STILL holding the instance, which completes the
    /// action it was holding (rule 3).
    HookDeletedWhileHolding,
    /// The instance the call names is one the group never held, at this published length.
    NeverHeld(&'static str),
    /// The GROUP the call names is one the account never held, addressed by an instance that
    /// another group does hold.
    AbsentGroup,
}

/// RULE 1: a callback addressed at an instance its group holds no active action for is refused
/// with the fixed sentence around that instance id and nothing else — whatever ended the
/// action, and whatever hook the call names.
///
/// The rows differ along what the rule generalises over: HOW the action stopped being active
/// (consumed by either result, timed out by the clock, carried off by the hook's deletion
/// before or while it held the instance, never held at all, named under a group the account
/// does not hold) and, for the rows about an instance nothing ever held, the LENGTH of the
/// instance id the sentence has to quote. Every row is driven through BOTH doors that take the
/// address and under BOTH a hook the group carries and one it does not, because neither
/// changes the answer.
#[tokio::test]
async fn a_callback_for_an_instance_holding_nothing_names_only_that_instance() {
    let rows: &[(&str, Ended)] = &[
        ("an action a callback continued", Ended::ConsumedByContinue),
        ("an action a callback abandoned", Ended::ConsumedByAbandon),
        ("an action the clock timed out", Ended::TimedOut),
        ("a hook deleted after its action was consumed", Ended::HookDeletedAfterwards),
        ("a hook deleted while it held the instance", Ended::HookDeletedWhileHolding),
        ("the shortest instance id nothing holds", Ended::NeverHeld(NEVER_HELD_INSTANCES[0])),
        ("a middling instance id nothing holds", Ended::NeverHeld(NEVER_HELD_INSTANCES[1])),
        ("the longest instance id nothing holds", Ended::NeverHeld(NEVER_HELD_INSTANCES[2])),
        ("a group the account does not hold", Ended::AbsentGroup),
    ];

    for (index, (case, ended)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-no-active-{index}");
        let hook = "hold";
        create_group(&router, case, &group, &subnet, 0, &[]).await;
        // `DefaultResult` CONTINUE, so the row about the CLOCK resolving the hold has it
        // carry the instance on rather than hand it back.
        put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
        set_capacity(&router, case, &group, 1).await;
        let held = poll_states(&router, case, &group, PENDING_WAIT, 1).await;
        let held_instance = held
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));

        match ended {
            Ended::ConsumedByContinue | Ended::HookDeletedAfterwards => {
                complete(&router, case, &group, hook, &[
                    ("InstanceId", held_instance.as_str()),
                    ("LifecycleActionResult", CONTINUE),
                ])
                .await;
                poll_until_out_of(&router, case, &group, &held_instance, PENDING_WAIT).await;
            }
            Ended::ConsumedByAbandon => {
                complete(&router, case, &group, hook, &[
                    ("InstanceId", held_instance.as_str()),
                    ("LifecycleActionResult", ABANDON),
                ])
                .await;
                poll_until_out_of(&router, case, &group, &held_instance, PENDING_WAIT).await;
            }
            Ended::TimedOut => {
                // The shortest `HeartbeatTimeout` a hook accepts is longer than a test may
                // wait, so the row drives the CLOCK: the deadline the hold stored is moved
                // into the past, and the next read materialises what that instant made due.
                time_out_every_hold(&engine, &group);
                poll_until_out_of(&router, case, &group, &held_instance, PENDING_WAIT).await;
            }
            Ended::HookDeletedWhileHolding => {}
            Ended::NeverHeld(_) | Ended::AbsentGroup => {}
        }
        if *ended == Ended::HookDeletedAfterwards || *ended == Ended::HookDeletedWhileHolding {
            let (status, body) = query(&router, "DeleteLifecycleHook", &[
                ("AutoScalingGroupName", group.as_str()),
                ("LifecycleHookName", hook),
            ])
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the hook must be removable, got {status} with {body:?}"
            );
            // RULE 3 at its own row: the delete completed the action it was holding, so the
            // instance is no longer waiting at a hook nothing can address.
            poll_until_out_of(&router, case, &group, &held_instance, PENDING_WAIT).await;
        }

        // The instance the refused call addresses, and the group it names it under.
        let addressed = match ended {
            Ended::NeverHeld(instance_id) => (*instance_id).to_string(),
            _ => held_instance.clone(),
        };
        let named_group = match ended {
            Ended::AbsentGroup => "asg-no-such-group".to_string(),
            _ => group.clone(),
        };
        let expected = no_active_action_sentence(&addressed);
        // Every hold the group still carries, which a refused call must leave alone.
        let held_before = held_actions(&engine, &group);

        for named_hook in [hook, "no-such-hook"] {
            for door in ["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"] {
                let (_, code, message) = refused_callback(
                    &router,
                    case,
                    door,
                    (&named_group, named_hook, &addressed),
                )
                .await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (VALIDATION_ERROR, expected.as_str()),
                    "case {case:?} through {door} under hook {named_hook:?}: expected \
                     {VALIDATION_ERROR} {expected:?}, got {code:?} {message:?}"
                );
                assert!(
                    !message.contains(named_hook),
                    "case {case:?} through {door}: the refusal must not name the hook \
                     {named_hook:?}, got {message:?}"
                );
                assert_eq!(
                    held_actions(&engine, &group),
                    held_before,
                    "case {case:?} through {door} under hook {named_hook:?}: a refused \
                     callback releases nothing"
                );
            }
        }
    }
}

/// WHY the hook the refused call names is holding nothing, while the instance it names is
/// still held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StillHeld {
    /// The call names a hook the group never carried.
    UnknownHook,
    /// The call names the group's own hook whose action this caller already consumed, while a
    /// sibling hook goes on holding the instance.
    ConsumedHook,
}

/// RULE 2: the hook IS named — the same fixed text with the request's `LifecycleHookName`
/// after the instance id — exactly while the instance the call names is still held by an
/// outstanding action of some other hook.
///
/// The boundary of rule 1, so the two are pinned together: the rows differ along why the named
/// hook holds nothing, and each is driven through both doors.
#[tokio::test]
async fn a_callback_under_a_hook_holding_nothing_still_names_that_hook() {
    let rows: &[(&str, StillHeld)] = &[
        ("a hook the group never carried", StillHeld::UnknownHook),
        ("a hook whose own action was consumed", StillHeld::ConsumedHook),
    ];

    for (index, (case, under)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-still-held-{index}");
        create_group(&router, case, &group, &subnet, 0, &[]).await;
        for holder in ["hold-0", "hold-1"] {
            put_hook(&router, case, &group, holder, LAUNCHING, CONTINUE).await;
        }
        set_capacity(&router, case, &group, 1).await;
        let held = poll_states(&router, case, &group, PENDING_WAIT, 1).await;
        let held_instance = held
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));

        let named_hook = match under {
            StillHeld::UnknownHook => "no-such-hook",
            StillHeld::ConsumedHook => "hold-0",
        };
        if *under == StillHeld::ConsumedHook {
            complete(&router, case, &group, named_hook, &[
                ("InstanceId", held_instance.as_str()),
                ("LifecycleActionResult", CONTINUE),
            ])
            .await;
        }
        let held_before = held_actions(&engine, &group);
        assert!(
            !held_before.is_empty(),
            "case {case:?}: the instance must still be held by the sibling hook, got \
             {held_before:?}"
        );

        let expected = hook_qualified_sentence(&held_instance, named_hook);
        for door in ["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"] {
            let (_, code, message) =
                refused_callback(&router, case, door, (&group, named_hook, &held_instance)).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (VALIDATION_ERROR, expected.as_str()),
                "case {case:?} through {door}: expected {VALIDATION_ERROR} {expected:?}, got \
                 {code:?} {message:?}"
            );
            assert_eq!(
                held_actions(&engine, &group),
                held_before,
                "case {case:?} through {door}: a refused callback releases nothing"
            );
        }
    }
}

/// Which transition the deleted hook was holding, which is what the delete completes its
/// outstanding action with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    /// A launch, completed with `ABANDON`: the held instance never comes into service.
    Launch,
    /// A termination, completed with `CONTINUE`: the termination carries on.
    Termination,
}

/// RULE 3: deleting a hook completes the lifecycle actions it was holding, with the result its
/// own transition documents — so the hold is ended rather than left behind, and what the held
/// instance does next is what that result does to it.
///
/// The rows are the two transitions a hook can hold, because the documented result is stated
/// one per direction.
#[tokio::test]
async fn deleting_a_hook_completes_the_actions_it_was_holding() {
    let rows: &[(&str, Holding)] = &[
        ("a launch hook holding an instance", Holding::Launch),
        ("a termination hook holding an instance", Holding::Termination),
    ];

    for (index, (case, holding)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-delete-holding-{index}");
        let hook = "hold";
        create_group(&router, case, &group, &subnet, 0, &[]).await;
        let wait_state = match holding {
            Holding::Launch => {
                put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
                set_capacity(&router, case, &group, 1).await;
                PENDING_WAIT
            }
            Holding::Termination => {
                // The instance is brought into service with no hook in front of it, and the
                // termination hook is what holds its way OUT.
                set_capacity(&router, case, &group, 1).await;
                poll_states(&router, case, &group, IN_SERVICE, 1).await;
                put_hook(&router, case, &group, hook, TERMINATING, CONTINUE).await;
                set_capacity(&router, case, &group, 0).await;
                TERMINATING_WAIT
            }
        };
        let held = poll_states(&router, case, &group, wait_state, 1).await;
        let held_instance = held
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
        assert_eq!(
            held_actions(&engine, &group).len(),
            1,
            "case {case:?}: the hook must be holding exactly one action, got {:?}",
            held_actions(&engine, &group)
        );

        let (status, body) = query(&router, "DeleteLifecycleHook", &[
            ("AutoScalingGroupName", group.as_str()),
            ("LifecycleHookName", hook),
        ])
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the hook must be removable, got {status} with {body:?}"
        );
        assert!(
            held_actions(&engine, &group).is_empty(),
            "case {case:?}: the delete must complete the action it was holding, got {:?}",
            held_actions(&engine, &group)
        );

        // What the documented result does to the instance the hold had: an ABANDONed launch
        // never comes into service, so the capacity the group is still owed is met by a
        // DIFFERENT instance; a CONTINUEd termination carries on and the group ends up empty.
        poll_until_out_of(&router, case, &group, &held_instance, wait_state).await;
        match holding {
            Holding::Launch => {
                let serving = poll_states(&router, case, &group, IN_SERVICE, 1).await;
                assert!(
                    !serving.contains(&held_instance),
                    "case {case:?}: the abandoned instance {held_instance} must not come into \
                     service, got {serving:?}"
                );
            }
            Holding::Termination => {
                let states = poll_until_group_is_empty(&router, case, &group).await;
                assert!(
                    states.is_empty(),
                    "case {case:?}: the resumed termination must empty the group, got {states:?}"
                );
            }
        }

        // RULE 1 under rule 3: the deleted hook holds nothing, so a later callback naming it
        // is refused for the INSTANCE it addressed and nothing else.
        let expected = no_active_action_sentence(&held_instance);
        for door in ["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"] {
            let (_, code, message) =
                refused_callback(&router, case, door, (&group, hook, &held_instance)).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (VALIDATION_ERROR, expected.as_str()),
                "case {case:?} through {door}: expected {VALIDATION_ERROR} {expected:?}, got \
                 {code:?} {message:?}"
            );
        }
    }
}
