// SPDX-License-Identifier: CC0-1.0

//! Where the ADDRESS of a lifecycle-action callback is checked, relative to everything else
//! the call names.
//!
//! `CompleteLifecycleAction` and `RecordLifecycleActionHeartbeat` both act on one held
//! lifecycle action, and both name it the same way: the hook and the group are required,
//! while `LifecycleActionToken` and `InstanceId` are each optional
//! (`smithy_query(query="operation",operation="CompleteLifecycleAction")` and the same for
//! the heartbeat), so a request carrying NEITHER addresses no action at all. The rule this
//! suite pins, over a set of inputs rather than one case:
//!
//! 1. A callback that names neither the token nor the instance is refused for the MISSING
//!    ADDRESS — before the group, the hook or the action it names is looked up — so what the
//!    caller is told is that no lifecycle action was found because the call carried no
//!    identifier, never that the group or the hook does not exist.
//! 2. That sentence has no variable parts: it is the same text whatever group and hook names
//!    the request carried, so no part of the request is interpolated into it.
//! 3. Both doors answer it identically, and the rule stops where the address begins: a call
//!    that DOES carry a token or an instance id is refused for the identifier it sent, even
//!    when the group it named does not exist.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_value, tag_values,
    xml_unescape};

/// The launch source the groups here launch from, created over Amazon EC2's own wire because
/// an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-action-address";

/// The transition the hooks here hold, spelled as the model documents
/// `PutLifecycleHookType$LifecycleTransition`.
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";

/// A result a callback may carry ("Valid values: CONTINUE | ABANDON"), required on
/// `CompleteLifecycleAction` and absent from the heartbeat.
const CONTINUE: &str = "CONTINUE";

/// The `LifecycleState` an instance a launch hook holds is reported in, spelled as the
/// model's own `com.amazonaws.autoscaling#LifecycleState` enum does.
const PENDING_WAIT: &str = "Pending:Wait";

/// The two operations that take a lifecycle action's address. Every row is driven through
/// both, so the two cannot disagree about when the address is checked.
const DOORS: &[&str] = &["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"];

/// The sentence a callback that addressed no action at all is refused with — wholly fixed
/// text, written out here rather than taken from the crate so a wrong constant in the
/// implementation cannot make a row pass.
const NO_ADDRESS_MESSAGE: &str =
    "No Lifecycle Action found because no token or instance ID is provided";

/// The one error code this service names for a problem with a request's arguments: neither
/// operation models an input-validation fault (both declare only `ResourceContentionFault`),
/// so an argument problem is reported through the awsQuery common client error.
const ARGUMENT_REFUSAL_CODE: &str = "ValidationError";

/// How long a leg waits for what it is waiting on, and how often it looks: driven off what
/// the account REPORTS rather than off a fixed sleep, and far above the work one capacity
/// change makes, so a loaded machine delays a row instead of breaking it.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// One emulator with Auto Scaling and Amazon EC2 on one engine, a launch template the groups
/// launch from and one subnet to place into.
async fn fixture() -> (MultiServiceRouter, String) {
    let (router, engine) = emulator();
    crate::support::create_launch_template(&router, TEMPLATE).await;
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    (router, subnet)
}

/// The `(code, message)` of a refused answer, with the message unescaped.
fn refusal_of(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// Create `group` with `desired` capacity and assert the create was served.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    subnet: &str,
    desired: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be created, got {status} with {body:?}"
    );
}

/// Put one launch lifecycle hook on `group`, and assert it was stored.
async fn put_hook(router: &MultiServiceRouter, case: &str, group: &str, hook: &str) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleTransition", LAUNCHING),
        p("DefaultResult", CONTINUE),
        p("HeartbeatTimeout", "300"),
    ];
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the hook {hook:?} must be stored, got {status} with {body:?}"
    );
}

/// The `LifecycleState` the group reports for each instance it holds.
async fn reported_states(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be readable, got {status} with {body:?}"
    );
    tag_values(&body, "LifecycleState")
}

/// Ask for one instance and look again until the group reports it waiting at its hook, or
/// [`DEADLINE`] passes: the condition is what is waited for, never a fixed span.
async fn hold_one_instance(router: &MultiServiceRouter, case: &str, group: &str) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity 1 must be accepted, got {status} with {body:?}"
    );
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let states = reported_states(router, case, group).await;
        if states.len() == 1 && states.iter().all(|state| state == PENDING_WAIT) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group must report one instance in {PENDING_WAIT:?}, got \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Delete `group` and look again until the account reports no group under that name, or
/// [`DEADLINE`] passes: a delete whose group is still settling is refused transiently, so
/// the delete is retried the way a client retries it.
async fn delete_group(router: &MultiServiceRouter, case: &str, group: &str) {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DeleteAutoScalingGroup",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        if status == 200 && reported_states(router, case, group).await.is_empty() {
            let (status, body) = query(
                router,
                "DescribeAutoScalingGroups",
                &[("AutoScalingGroupNames.member.1", group)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the account must be readable, got {status} with {body:?}"
            );
            if tag_values(&body, "AutoScalingGroupName").is_empty() {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group {group:?} must become deletable, last answer was \
             {status} with {body:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Post one callback through `door`, naming `group` and `hook` and carrying `addressing`.
async fn callback(
    router: &MultiServiceRouter,
    door: &str,
    group: &str,
    hook: &str,
    addressing: &[(&str, &str)],
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", group),
        ("LifecycleHookName", hook),
    ];
    params.extend_from_slice(addressing);
    if door == "CompleteLifecycleAction" {
        params.push(("LifecycleActionResult", CONTINUE));
    }
    query(router, door, &params).await
}

/// One row of the addressed table: what the case is, the identifying members the request
/// carries, and the identifier the refusal has to name.
type AddressedRow<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

/// What the account holds, under the names the call carries, when the unaddressed callback
/// arrives. The rows span the lookups the address check must pre-empt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// Nothing: no group of that name was ever created.
    NoGroup,
    /// A group the account held and then deleted, so the name resolves to nothing again.
    DeletedGroup,
    /// The group exists; the hook the call names does not.
    GroupWithoutTheHook,
    /// Both exist, and the group holds no instance — so there is no action under the hook.
    HookHoldingNothing,
    /// Both exist and an instance of the group IS waiting at that hook, so an action the
    /// call could have addressed really is held.
    HookHoldingAnAction,
}

/// A callback that names neither identifier is refused for its missing address, whatever the
/// account holds under the names it carried — the group, the hook and the held action are
/// never looked up ahead of it, and both doors say the same thing.
#[tokio::test]
async fn an_unaddressed_callback_is_refused_for_its_missing_address() {
    let rows: &[(&str, Holds)] = &[
        ("no group of that name", Holds::NoGroup),
        ("a group deleted again", Holds::DeletedGroup),
        ("a group without the hook", Holds::GroupWithoutTheHook),
        ("a hook holding nothing", Holds::HookHoldingNothing),
        ("a hook holding an action", Holds::HookHoldingAnAction),
    ];

    let (router, subnet) = fixture().await;
    for (index, (case, holds)) in rows.iter().enumerate() {
        let group = format!("asg-address-{index}");
        let hook = "hold";
        match holds {
            Holds::NoGroup => {}
            Holds::DeletedGroup => {
                create_group(&router, case, &group, &subnet, 0).await;
                delete_group(&router, case, &group).await;
            }
            Holds::GroupWithoutTheHook => {
                create_group(&router, case, &group, &subnet, 0).await;
            }
            Holds::HookHoldingNothing => {
                create_group(&router, case, &group, &subnet, 0).await;
                put_hook(&router, case, &group, hook).await;
            }
            Holds::HookHoldingAnAction => {
                create_group(&router, case, &group, &subnet, 0).await;
                put_hook(&router, case, &group, hook).await;
                hold_one_instance(&router, case, &group).await;
            }
        }
        // The hook the call names: one the group does not carry for the row that is about a
        // missing hook, and the group's own hook otherwise.
        let named_hook = if *holds == Holds::GroupWithoutTheHook {
            "not-a-hook"
        } else {
            hook
        };

        for door in DOORS {
            let (status, body) = callback(&router, door, &group, named_hook, &[]).await;
            assert_eq!(
                status, 400,
                "case {case:?} through {door}: expected HTTP 400, got {status} with {body:?}"
            );
            let (code, message) = refusal_of(&body);
            assert_eq!(
                (code.as_str(), message.as_str()),
                (ARGUMENT_REFUSAL_CODE, NO_ADDRESS_MESSAGE),
                "case {case:?} through {door}: expected the missing-address refusal, got \
                 {code:?} with {message:?}"
            );
            assert!(
                !message.contains(&group) && !message.contains(named_hook),
                "case {case:?} through {door}: the sentence interpolates no part of the \
                 request, got {message:?}"
            );
        }
    }
}

/// The missing-address sentence is the SAME text whatever names the request carried: the
/// rows differ along the group and hook names, including each name at the longest and the
/// shortest the model admits, and every character class the hook's pattern allows.
///
/// The bounds and the pattern are the model's own:
/// `AutoScalingGroupName` targets `ResourceName` (`@length(1..1600)`) and
/// `LifecycleHookName` targets `AsciiStringMaxLen255` (`@length(1..255)`, pattern
/// `^[A-Za-z0-9\-_\/]+$`) on both operations
/// (`smithy_query(query="operation",operation="RecordLifecycleActionHeartbeat")`).
#[tokio::test]
async fn the_missing_address_sentence_quotes_no_part_of_the_request() {
    let rows: Vec<(&str, String, String)> = vec![
        ("plain names", "asg-plain".to_string(), "hold".to_string()),
        (
            "a group name carrying spaces and punctuation",
            "asg with spaces, colons: and #symbols".to_string(),
            "hold".to_string(),
        ),
        (
            "the longest group name the model admits",
            "g".repeat(1600),
            "hold".to_string(),
        ),
        (
            "the shortest names the model admits",
            "a".to_string(),
            "h".to_string(),
        ),
        (
            "the longest hook name the model admits",
            "asg-long-hook".to_string(),
            "h".repeat(255),
        ),
        (
            "a hook name using every character class its pattern admits",
            "asg-hook-pattern".to_string(),
            "Hook_9/alpha-beta".to_string(),
        ),
    ];

    let (router, _subnet) = fixture().await;
    for (case, group, hook) in rows {
        for door in DOORS {
            let (status, body) = callback(&router, door, &group, &hook, &[]).await;
            assert_eq!(
                status, 400,
                "case {case:?} through {door}: expected HTTP 400, got {status} with {body:?}"
            );
            let (code, message) = refusal_of(&body);
            assert_eq!(
                (code.as_str(), message.as_str()),
                (ARGUMENT_REFUSAL_CODE, NO_ADDRESS_MESSAGE),
                "case {case:?} through {door}: expected the missing-address refusal, got \
                 {code:?} with {message:?}"
            );
        }
    }
}

/// Where the rule stops: a callback that DOES carry an address is refused for the identifier
/// it sent — the token, or the instance — and not for a missing address, even when the group
/// it named is one the account does not hold.
#[tokio::test]
async fn an_addressed_callback_is_refused_for_the_identifier_it_sent() {
    // A token in the shape the member is modelled in (`@length(36..36)`) that this service
    // never minted, and an instance id in Amazon EC2's own form that no group holds.
    let token = "00000000-0000-4000-8000-000000000000";
    let instance = "i-0123456789abcdef0";
    let rows: &[AddressedRow<'_>] = &[
        ("a token alone", &[("LifecycleActionToken", token)], token),
        ("an instance id alone", &[("InstanceId", instance)], instance),
        (
            "both identifiers, which the token answers",
            &[
                ("LifecycleActionToken", token),
                ("InstanceId", instance),
            ],
            token,
        ),
    ];

    let (router, subnet) = fixture().await;
    let present = "asg-addressed";
    create_group(&router, "a group that holds no action", present, &subnet, 0).await;

    for (case, addressing, quoted) in rows {
        // Both a group the account does not hold and one it does: the identifier the call
        // sent is what the refusal is about either way.
        for group in ["asg-no-such-group", present] {
            for door in DOORS {
                let (status, body) = callback(&router, door, group, "hold", addressing).await;
                assert_eq!(
                    status, 400,
                    "case {case:?} on {group:?} through {door}: expected HTTP 400, got \
                     {status} with {body:?}"
                );
                let (code, message) = refusal_of(&body);
                assert_eq!(
                    code, ARGUMENT_REFUSAL_CODE,
                    "case {case:?} on {group:?} through {door}: expected \
                     {ARGUMENT_REFUSAL_CODE}, got {code:?} with {message:?}"
                );
                assert_ne!(
                    message, NO_ADDRESS_MESSAGE,
                    "case {case:?} on {group:?} through {door}: a call that carried an \
                     address must not be refused for a missing one, got {message:?}"
                );
                assert!(
                    message.contains(quoted),
                    "case {case:?} on {group:?} through {door}: the refusal must name \
                     {quoted:?}, got {message:?}"
                );
            }
        }
    }
}
