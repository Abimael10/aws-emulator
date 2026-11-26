// SPDX-License-Identifier: CC0-1.0

//! The HOOK-QUALIFIED form of the sentence a lifecycle-action callback addressed by INSTANCE
//! is refused with, and the boundary where that form stops.
//!
//! Both doors that take a lifecycle action's address — `CompleteLifecycleAction` and
//! `RecordLifecycleActionHeartbeat` — refuse a call naming no outstanding action with HTTP 400
//! and the Query code `ValidationError`, and the SENTENCE is the discriminator a client
//! branches on, since neither operation models a client-side error (both declare only
//! `ResourceContentionFault`, `smithy_query(query="operation",operation="CompleteLifecycleAction")`).
//! The rules under test, each asserted over a SET of inputs rather than one recorded case:
//!
//! 1. While the INSTANCE a call names is still held by an outstanding lifecycle action of the
//!    group, but not by one of the hook it named, the refusal is qualified by that hook: fixed
//!    text, the request's own `InstanceId`, ' and HookName ', and the request's own
//!    `LifecycleHookName` — quoted VERBATIM whatever form that member admits
//!    (`AsciiStringMaxLen255`, `@length(min: 1, max: 255)`, pattern `^[A-Za-z0-9\-_\/]+$`), and
//!    whether the named hook is one the group never carried or one whose own action this
//!    caller already consumed.
//! 2. The form is selected by the INSTANCE's outstanding actions and by nothing else, so it
//!    holds at either transition's wait state — a launch the hooks hold and a termination they
//!    hold alike — and drops to the instance-only sentence the moment the instance holds no
//!    outstanding action at all, for the very same request.
//! 3. A refused callback is inert: every hold the group carries, and the wait state it reports
//!    the instance in, are left exactly as they were, and the hook that IS holding the
//!    instance goes on taking a heartbeat.
//! 4. The hook name is a request value, so a name past its published maximum is refused for
//!    its LENGTH by the constraint walk and never reaches these sentences at all.
//!
//! The lifecycle-hold harness (groups, hooks, callbacks, the readers and the clock drivers) is
//! owned by `lifecycle_hook_execution.rs`; this suite drives those helpers rather than keeping
//! a second copy.

use smithy_server_runtime::MultiServiceRouter;

use crate::lifecycle_hook_execution::{
    complete, create_group, fixture, held_actions, poll_states, put_hook, refusal_of,
    reported_states, set_capacity, CONTINUE, DEADLINE, IN_SERVICE, LAUNCHING, PENDING_WAIT, POLL,
    TERMINATING, TERMINATING_WAIT,
};
use crate::support::query;

/// The Query error code every semantic refusal of these two doors carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The two operations that take a lifecycle action's address. Every row is driven through
/// both, so the two cannot disagree about which sentence a call is refused with.
const DOORS: &[&str] = &["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"];

/// The published maximum length of `LifecycleHookName`: `AsciiStringMaxLen255` carries
/// `@length(min: 1, max: 255)`
/// (`smithy_query(query="operation",operation="CompleteLifecycleAction")`).
const MAX_HOOK_NAME_LENGTH: usize = 255;

/// The whole sentence a call is refused with while the instance it names IS still held, under
/// a hook holding none of it: fixed text and the two variable parts, both taken from the
/// request.
///
/// Spelled out here rather than taken from the crate, so no row can pass on the handler's own
/// word for the sentence.
fn hook_qualified_sentence(instance_id: &str, hook_name: &str) -> String {
    format!("No active Lifecycle Action found with instance ID {instance_id} and HookName {hook_name}")
}

/// The other form, for an instance the group holds no outstanding action for at all: the same
/// fixed text with the instance id as its ONLY variable part.
fn instance_only_sentence(instance_id: &str) -> String {
    format!("No active Lifecycle Action found with instance ID {instance_id}")
}

/// Post one callback through `door`, addressed at `instance_id` under `hook`, assert it was
/// refused with HTTP 400, and answer the `(code, message)` it was refused with.
async fn refusal_through(
    router: &MultiServiceRouter,
    case: &str,
    door: &str,
    (group, hook, instance_id): (&str, &str, &str),
) -> (String, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", group),
        ("LifecycleHookName", hook),
        ("InstanceId", instance_id),
    ];
    if door == "CompleteLifecycleAction" {
        params.push(("LifecycleActionResult", CONTINUE));
    }
    let (status, body) = query(router, door, &params).await;
    assert_eq!(
        status, 400,
        "case {case:?} through {door}: expected HTTP 400, got {status} with {body:?}"
    );
    refusal_of(&body)
}

/// Record a heartbeat against the action `hook` is holding for `instance_id`, which must be
/// accepted: the hook that IS holding the instance is addressable throughout.
async fn accepted_heartbeat(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    instance_id: &str,
) {
    let (status, body) = query(
        router,
        "RecordLifecycleActionHeartbeat",
        &[
            ("AutoScalingGroupName", group),
            ("LifecycleHookName", hook),
            ("InstanceId", instance_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a heartbeat on the holding hook {hook:?} must be accepted, got {status} \
         with {body:?}"
    );
}

/// Look at the group until it no longer reports any instance in `state`, or [`DEADLINE`]
/// passes.
///
/// What a leg that is about an instance HAVING LEFT a wait state waits on: the condition
/// itself, looked at again until it holds, so a loaded or a fast host changes nothing but how
/// many looks it takes.
async fn poll_until_out_of(router: &MultiServiceRouter, case: &str, group: &str, state: &str) {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let states = reported_states(router, case, group).await;
        if !states.iter().any(|reported| reported == state) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: no instance may stay in {state:?}, got {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// WHY the hook a refused call names is holding nothing while the instance it names is still
/// held — the two ways that can be true, which the sentence does not distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedHook {
    /// A hook the group never carried.
    NeverCarried,
    /// A hook of the group whose own action this caller already consumed, while a sibling hook
    /// goes on holding the instance.
    OwnActionConsumed,
}

/// RULE 1 and 3: while the instance is still held, the refusal quotes the request's own
/// `LifecycleHookName` verbatim — whatever form the member admits — and releases nothing.
///
/// The rows differ along what the rule generalises over: the FORM of the hook name the request
/// carries (its shortest admitted length, a name using every character class its pattern
/// admits, and a name at its published maximum length) and WHY that hook holds nothing. Each
/// row is driven through both doors.
#[tokio::test]
async fn the_hook_qualified_refusal_quotes_the_hook_name_the_request_sent() {
    // The forms the member admits, from `LifecycleHookName`'s own constraints: `@length(min:
    // 1, max: 255)` and pattern `^[A-Za-z0-9\-_\/]+$`.
    let forms: [(&str, String); 3] = [
        ("the shortest name the member admits", "h".to_string()),
        (
            "a name using every character class the pattern admits",
            "Hook-9_x/Y".to_string(),
        ),
        (
            "a name at the published maximum length",
            "h".repeat(MAX_HOOK_NAME_LENGTH),
        ),
    ];

    let mut index = 0usize;
    for (form, named) in &forms {
        for under in [NamedHook::NeverCarried, NamedHook::OwnActionConsumed] {
            let case = format!("{form}, {under:?}");
            let case = case.as_str();
            let (router, engine, subnet) = fixture().await;
            let group = format!("asg-hook-quoted-{index}");
            index += 1;
            create_group(&router, case, &group, &subnet, 0, &[]).await;
            // The hook that holds the instance throughout, so the instance has an outstanding
            // action whichever way the NAMED hook comes to hold nothing.
            let keeper = "keeper";
            put_hook(&router, case, &group, keeper, LAUNCHING, CONTINUE).await;
            if under == NamedHook::OwnActionConsumed {
                put_hook(&router, case, &group, named, LAUNCHING, CONTINUE).await;
            }
            set_capacity(&router, case, &group, 1).await;
            let instances = poll_states(&router, case, &group, PENDING_WAIT, 1).await;
            let instance = instances
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
            if under == NamedHook::OwnActionConsumed {
                complete(
                    &router,
                    case,
                    &group,
                    named,
                    &[
                        ("InstanceId", instance.as_str()),
                        ("LifecycleActionResult", CONTINUE),
                    ],
                )
                .await;
            }

            let held_before = held_actions(&engine, &group);
            assert!(
                held_before
                    .iter()
                    .any(|(id, hook, _)| id == &instance && hook == keeper),
                "case {case:?}: the keeper hook must still hold {instance}, got {held_before:?}"
            );
            assert!(
                !held_before.iter().any(|(_, hook, _)| hook == named),
                "case {case:?}: the named hook must hold nothing, got {held_before:?}"
            );

            let expected = hook_qualified_sentence(&instance, named);
            for door in DOORS {
                let (code, message) =
                    refusal_through(&router, case, door, (&group, named, &instance)).await;
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

            // The hold is untouched by the refusals, so the hook that has it still takes a
            // heartbeat and the instance is still reported waiting.
            accepted_heartbeat(&router, case, &group, keeper, &instance).await;
            assert_eq!(
                held_actions(&engine, &group),
                held_before,
                "case {case:?}: the accepted heartbeat must leave the hold where it was"
            );
            let states = reported_states(&router, case, &group).await;
            assert_eq!(
                states,
                vec![PENDING_WAIT.to_string()],
                "case {case:?}: the instance must stay held, got {states:?}"
            );
        }
    }
}

/// RULE 2's first half: the form is selected by the instance's outstanding actions, so it is
/// the same sentence at either transition's wait state.
///
/// The rows are the two transitions a hook can hold
/// (`PutLifecycleHookType$LifecycleTransition`), since the wait state the instance is reported
/// in — and the direction the group is taking it — is what differs; each row is driven under
/// both reasons the named hook can hold nothing and through both doors.
#[tokio::test]
async fn a_held_termination_names_the_hook_as_a_held_launch_does() {
    let rows: &[(&str, &str, &str)] = &[
        ("a launch the hooks hold", LAUNCHING, PENDING_WAIT),
        ("a termination the hooks hold", TERMINATING, TERMINATING_WAIT),
    ];

    for (index, (case, transition, wait_state)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-held-transition-{index}");
        let holders = ["hold-0", "hold-1"];
        // A launch is held on the way IN, so the group is created empty and scaled out behind
        // its hooks; a termination is held on the way OUT, so the member is brought into
        // service first and the hooks hold the call that takes it out.
        let instance = if *transition == LAUNCHING {
            create_group(&router, case, &group, &subnet, 0, &[]).await;
            for holder in holders {
                put_hook(&router, case, &group, holder, LAUNCHING, CONTINUE).await;
            }
            set_capacity(&router, case, &group, 1).await;
            poll_states(&router, case, &group, PENDING_WAIT, 1).await
        } else {
            create_group(&router, case, &group, &subnet, 1, &[]).await;
            let members = poll_states(&router, case, &group, IN_SERVICE, 1).await;
            let member = members
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: the group's member must be reported"));
            for holder in holders {
                put_hook(&router, case, &group, holder, TERMINATING, CONTINUE).await;
            }
            let (status, body) = query(
                &router,
                "TerminateInstanceInAutoScalingGroup",
                &[
                    ("InstanceId", member.as_str()),
                    ("ShouldDecrementDesiredCapacity", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the termination must be accepted, got {status} with {body:?}"
            );
            poll_states(&router, case, &group, TERMINATING_WAIT, 1).await
        }
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));

        // One of the two holds is consumed, which leaves the instance held by the other.
        complete(
            &router,
            case,
            &group,
            holders[0],
            &[
                ("InstanceId", instance.as_str()),
                ("LifecycleActionResult", CONTINUE),
            ],
        )
        .await;
        let held_before = held_actions(&engine, &group);
        assert_eq!(
            held_before
                .iter()
                .filter(|(id, hook, _)| id == &instance && hook == holders[1])
                .count(),
            1,
            "case {case:?}: the sibling hook must go on holding {instance}, got {held_before:?}"
        );

        for named in [holders[0], "no-such-hook"] {
            let expected = hook_qualified_sentence(&instance, named);
            for door in DOORS {
                let (code, message) =
                    refusal_through(&router, case, door, (&group, named, &instance)).await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (VALIDATION_ERROR, expected.as_str()),
                    "case {case:?} through {door} under hook {named:?}: expected \
                     {VALIDATION_ERROR} {expected:?}, got {code:?} {message:?}"
                );
                assert_eq!(
                    held_actions(&engine, &group),
                    held_before,
                    "case {case:?} through {door} under hook {named:?}: a refused callback \
                     releases nothing"
                );
            }
        }
        let states = reported_states(&router, case, &group).await;
        assert_eq!(
            states,
            vec![(*wait_state).to_string()],
            "case {case:?}: the instance must stay held in {wait_state:?}, got {states:?}"
        );
    }
}

/// RULE 2's second half: the very same request is answered with the hook-qualified sentence
/// while the instance is held and with the instance-only sentence once it holds nothing — so
/// the form is read off the group's state and not off the hook the caller named.
///
/// One group walks the boundary, and the two sides are asserted for the SAME two hook names (a
/// hook of the group whose action was consumed, and a hook the group never carried) through
/// both doors: nothing about the request changes between them.
#[tokio::test]
async fn the_sentence_drops_the_hook_once_the_instance_holds_nothing() {
    let case = "two launch hooks, consumed one at a time";
    let (router, engine, subnet) = fixture().await;
    let group = "asg-sentence-boundary";
    let holders = ["hold-0", "hold-1"];
    create_group(&router, case, group, &subnet, 0, &[]).await;
    for holder in holders {
        put_hook(&router, case, group, holder, LAUNCHING, CONTINUE).await;
    }
    set_capacity(&router, case, group, 1).await;
    let instances = poll_states(&router, case, group, PENDING_WAIT, 1).await;
    let instance = instances
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));

    // The first hold is consumed: the instance is still held by the second, so each of the two
    // names the sentence qualifies is the name the request sent.
    complete(
        &router,
        case,
        group,
        holders[0],
        &[
            ("InstanceId", instance.as_str()),
            ("LifecycleActionResult", CONTINUE),
        ],
    )
    .await;
    let named_hooks = [holders[0], "no-such-hook"];
    for named in named_hooks {
        let expected = hook_qualified_sentence(&instance, named);
        for door in DOORS {
            let (code, message) =
                refusal_through(&router, case, door, (group, named, &instance)).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (VALIDATION_ERROR, expected.as_str()),
                "case {case:?} through {door} under hook {named:?} while held: expected \
                 {VALIDATION_ERROR} {expected:?}, got {code:?} {message:?}"
            );
        }
    }

    // The last hold is consumed, so the instance leaves the wait state and holds nothing.
    complete(
        &router,
        case,
        group,
        holders[1],
        &[
            ("InstanceId", instance.as_str()),
            ("LifecycleActionResult", CONTINUE),
        ],
    )
    .await;
    poll_until_out_of(&router, case, group, PENDING_WAIT).await;
    assert!(
        held_actions(&engine, group).is_empty(),
        "case {case:?}: the group must hold no action left, got {:?}",
        held_actions(&engine, group)
    );

    let expected = instance_only_sentence(&instance);
    for named in named_hooks {
        for door in DOORS {
            let (code, message) =
                refusal_through(&router, case, door, (group, named, &instance)).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (VALIDATION_ERROR, expected.as_str()),
                "case {case:?} through {door} under hook {named:?} once nothing is held: \
                 expected {VALIDATION_ERROR} {expected:?}, got {code:?} {message:?}"
            );
            assert!(
                !message.contains(named),
                "case {case:?} through {door}: the refusal must not name the hook {named:?}, got \
                 {message:?}"
            );
        }
    }
}

/// RULE 4: the hook name is a REQUEST value, so one character past its published maximum is
/// refused for its length — the constraint clause quoting the whole value the caller sent —
/// and neither sentence above is reached.
///
/// The bound either side of which the two answers differ is asserted in one place: a name AT
/// the maximum is one of the rows of the first test above, and this is the row one past it.
#[tokio::test]
async fn a_hook_name_past_the_published_maximum_is_refused_for_its_length() {
    let case = "a hook name one character past the published maximum";
    let (router, engine, subnet) = fixture().await;
    let group = "asg-hook-name-overlong";
    let keeper = "keeper";
    create_group(&router, case, group, &subnet, 0, &[]).await;
    put_hook(&router, case, group, keeper, LAUNCHING, CONTINUE).await;
    set_capacity(&router, case, group, 1).await;
    let instances = poll_states(&router, case, group, PENDING_WAIT, 1).await;
    let instance = instances
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
    let held_before = held_actions(&engine, group);

    let overlong = "h".repeat(MAX_HOOK_NAME_LENGTH + 1);
    let expected = format!(
        "1 validation error detected: Value '{overlong}' at 'lifecycleHookName' failed to \
         satisfy constraint: Member must have length less than or equal to \
         {MAX_HOOK_NAME_LENGTH}"
    );
    for door in DOORS {
        let (code, message) =
            refusal_through(&router, case, door, (group, overlong.as_str(), &instance)).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (VALIDATION_ERROR, expected.as_str()),
            "case {case:?} through {door}: expected the length constraint refusal, got {code:?} \
             {message:?}"
        );
        assert_eq!(
            held_actions(&engine, group),
            held_before,
            "case {case:?} through {door}: a refused callback releases nothing"
        );
    }
}
