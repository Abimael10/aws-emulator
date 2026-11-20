// SPDX-License-Identifier: CC0-1.0

//! What a lifecycle hook DOES to the transition it is configured for: the wait state it
//! holds an instance in, what the group and Amazon EC2 report while it holds one, what the
//! release of the hold leaves behind, and the sentences a callback addressed at no held
//! action is refused with.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded case:
//!
//! 1. A LAUNCH hook holds every launch of its group that has not completed — an instance the
//!    group still reports `Pending` has not passed the launching transition — so a hook
//!    stored while a launch is in flight takes that instance into the wait state exactly as
//!    one stored before the launch does, one hold per instance, each under its own token.
//! 2. An instance a launch hook holds is UP: the work a wait state exists for runs on a
//!    started instance ("a user data script that runs while an instance is in a wait state
//!    due to a lifecycle hook",
//!    `smithy_query(query="operation",operation="CompleteLifecycleAction")`), so Amazon EC2
//!    reports it `running` for as long as the hold lasts.
//! 3. Releasing the last launch hold ADMITS the instance: the span this service owes an
//!    instance coming into service was spent in the wait, so the next read of the group
//!    reports `InService` rather than a further transient state — however the hold ended.
//! 4. A callback addressed at no held action is refused naming what the caller has to
//!    correct: the token it sent, or the instance it sent — and the hook as well exactly when
//!    that instance IS waiting at a hook of the group, which is when the hook name is the
//!    half of the address that matched nothing.
//! 5. A TERMINATION a hook abandoned under a group whose `TerminateHookAbandon` retention
//!    trigger is set to `retain` does not happen: the instance is reported
//!    `Terminating:Retained`, Amazon EC2 keeps running it, it counts towards no capacity, and
//!    only `TerminateInstanceInAutoScalingGroup` takes it out. Under `terminate` the same
//!    abandon shuts it down.
//! 6. The per-group hook quota is refused with the LIMIT and no part of the request.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{borrowed, ec2_query, emulator, p, query, seed_subnets, tag_value, tag_values,
    xml_unescape};

/// The launch source every group here launches from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-hook-execution";

/// The two transitions a hook can be created for, written out here (and not taken from the
/// crate) so a wrong constant in the implementation cannot make a row pass. From the model's
/// own documentation for `PutLifecycleHookType$LifecycleTransition`.
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// The results a hook may default to and a callback may carry ("Valid values: CONTINUE |
/// ABANDON").
const CONTINUE: &str = "CONTINUE";
const ABANDON: &str = "ABANDON";

/// The `LifecycleState` values this suite asserts, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum does
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const PENDING: &str = "Pending";
const PENDING_WAIT: &str = "Pending:Wait";
const IN_SERVICE: &str = "InService";
const TERMINATING_WAIT: &str = "Terminating:Wait";
const TERMINATING_RETAINED: &str = "Terminating:Retained";

/// What Amazon EC2 reports for an instance it has finished starting, and for one it is
/// shutting down, as EC2 names them on the wire.
const EC2_RUNNING: &str = "running";
const EC2_SHUTTING_DOWN: &str = "shutting-down";

/// How long a `LifecycleActionToken` is: `@length(min: 36, max: 36)` on the model's own
/// `LifecycleActionToken` shape.
const TOKEN_LENGTH: usize = 36;

/// The most hooks one group may hold, from the model's own documentation for
/// `PutLifecycleHook` ("your maximum limit of lifecycle hooks, which by default is 50 per
/// Auto Scaling group").
const MAX_HOOKS_PER_GROUP: usize = 50;

/// How long a leg waits for what it is waiting on, and how often it looks: driven off what
/// the group REPORTS rather than off a fixed sleep, and far above the work one capacity
/// change makes, so a loaded machine delays a row instead of breaking it.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// One emulator with Auto Scaling, Amazon EC2 and Elastic Load Balancing v2 on one engine,
/// a launch template the groups launch from and one subnet to place into.
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

/// Create the group `group` with `desired` capacity, carrying `extra` request members, and
/// assert the create was served.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    subnet: &str,
    desired: i32,
    extra: &[(String, String)],
) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", subnet),
    ];
    params.extend_from_slice(extra);
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be created, got {status} with {body:?}"
    );
}

/// Put one lifecycle hook on the group, and assert it was stored.
async fn put_hook(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    transition: &str,
    default_result: &str,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
        p("LifecycleTransition", transition),
        p("DefaultResult", default_result),
        p("HeartbeatTimeout", "300"),
    ];
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the hook {hook:?} must be stored, got {status} with {body:?}"
    );
}

/// Ask for `desired` capacity on the group, which must be accepted.
async fn set_capacity(router: &MultiServiceRouter, case: &str, group: &str, desired: i32) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity {desired} must be accepted, got {status} with {body:?}"
    );
}

/// The `LifecycleState` the group reports for each instance it holds, in the order it
/// reports them.
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

/// The instance ids the group reports, in the order it reports them.
async fn reported_instances(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
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
    tag_values(&body, "InstanceId")
}

/// Look at the group until every instance it holds is reported in `expected`, and it holds
/// `count` of them, or [`DEADLINE`] passes; answers those instance ids.
async fn poll_states(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    expected: &str,
    count: usize,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let states = reported_states(router, case, group).await;
        if states.len() == count && states.iter().all(|state| state == expected) {
            return reported_instances(router, case, group).await;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group must report {count} instance(s) in {expected:?}, got \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What Amazon EC2 reports one instance's state as — the service that OWNS the instance is
/// what says whether it is running.
async fn ec2_state(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) must answer, got {status} with {body:?}"
    );
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// Look at Amazon EC2 until it reports `expected` for `instance_id`, or [`DEADLINE`] passes.
async fn poll_ec2_state(router: &MultiServiceRouter, case: &str, instance_id: &str, expected: &str) {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let state = ec2_state(router, case, instance_id).await;
        if state == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: Amazon EC2 must report {instance_id} {expected:?}, got {state:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The lifecycle actions the store holds for the group, as `(instance id, hook, token)`.
///
/// Read off the store because the token is published to the account's event bus and nowhere
/// in this service's own API: it is taken from where the handler wrote it and passed back
/// over the wire, which is the only way a callback can address the action at all.
fn held_actions(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    group: &str,
) -> Vec<(String, String, String)> {
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
                .map(|action| {
                    (
                        instance.instance_id.clone(),
                        action.hook_name.clone(),
                        action.token.clone(),
                    )
                })
                .collect::<Vec<(String, String, String)>>()
        })
        .collect()
}

/// Put the group's launch BACK IN FLIGHT: every instance it has launched and not sent out
/// again is reported `Pending` with the instant it settles at moved out of reach.
///
/// A launch settles on the clock, and this suite drives that clock rather than racing it: a
/// row that is about what happens WHILE a launch is in flight states that premise here, so it
/// holds whatever the host's pace, and a row about what happens after a launch polls for the
/// state instead.
fn launch_still_in_flight(engine: &std::sync::Arc<engine::InMemoryEngine>, group: &str) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let stored = store
        .groups
        .get_mut(group)
        .unwrap_or_else(|| panic!("the group {group:?} must be stored"));
    for instance in &mut stored.instances {
        if instance.is_leaving_group() {
            continue;
        }
        instance.lifecycle_state = PENDING.to_string();
        instance.transition_due =
            Some(std::time::SystemTime::now() + std::time::Duration::from_secs(3_600));
    }
}

/// Backdate every hold the group has taken, so each one's own deadline has passed.
///
/// The shortest `HeartbeatTimeout` the service accepts is far longer than a test may wait, so
/// a row that is about the hook's own deadline drives the CLOCK: the deadline the hold stored
/// is moved into the past and the next read materialises what that instant made due, exactly
/// as it would have on its own.
fn time_out_every_hold(engine: &std::sync::Arc<engine::InMemoryEngine>, group: &str) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let stored = store
        .groups
        .get_mut(group)
        .unwrap_or_else(|| panic!("the group {group:?} must be stored"));
    for instance in &mut stored.instances {
        for action in &mut instance.pending_lifecycle_actions {
            action.heartbeat_due = std::time::SystemTime::UNIX_EPOCH;
            action.global_due = std::time::SystemTime::UNIX_EPOCH;
        }
    }
}

/// The `(code, message)` of a refused answer, with the message unescaped.
fn refusal_of(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// When the group's launch hook was stored, relative to the launch it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookStored {
    /// Before anything was launched: the group is created empty and scaled out after.
    BeforeTheLaunch,
    /// While the launch is in flight: the group is created with capacity and the hook is
    /// stored before those instances have finished launching.
    DuringTheLaunch,
}

/// RULE 1 and 2: a launch hook holds every launch of its group that has not completed — one
/// hold per instance, each under its own token — and Amazon EC2 reports every held instance
/// as running.
///
/// The rows differ along what the rule generalises over: HOW MANY instances the launch
/// places (one, two, three — the recorded cases are one and two) and WHEN the hook was
/// stored relative to that launch.
#[tokio::test]
async fn a_launch_hook_holds_every_launch_still_in_flight() {
    let rows: &[(&str, i32, HookStored)] = &[
        ("one instance, hook stored first", 1, HookStored::BeforeTheLaunch),
        ("two instances, hook stored first", 2, HookStored::BeforeTheLaunch),
        ("three instances, hook stored first", 3, HookStored::BeforeTheLaunch),
        ("one instance launching, hook stored during", 1, HookStored::DuringTheLaunch),
        ("two instances launching, hook stored during", 2, HookStored::DuringTheLaunch),
    ];

    for (index, (case, count, stored)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-holds-{index}");
        let hook = "hold";
        match stored {
            HookStored::BeforeTheLaunch => {
                create_group(&router, case, &group, &subnet, 0, &[]).await;
                put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
                set_capacity(&router, case, &group, *count).await;
            }
            HookStored::DuringTheLaunch => {
                create_group(&router, case, &group, &subnet, *count, &[]).await;
                // The launch is still in flight when the hook arrives, whatever the host's
                // pace.
                launch_still_in_flight(&engine, &group);
                put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
            }
        }

        let instances = poll_states(
            &router,
            case,
            &group,
            PENDING_WAIT,
            usize::try_from(*count).expect("a small positive count"),
        )
        .await;
        // One hold per instance, each under the group's own hook and its own token of the
        // modelled length.
        let actions = held_actions(&engine, &group);
        assert_eq!(
            actions.len(),
            instances.len(),
            "case {case:?}: every held instance must hold one action, got {actions:?} for \
             {instances:?}"
        );
        let mut tokens: Vec<String> = Vec::new();
        for (held_id, hook_name, token) in &actions {
            assert!(
                instances.contains(held_id),
                "case {case:?}: the action {token:?} must be held against an instance the group \
                 reports, got {held_id:?} for {instances:?}"
            );
            assert_eq!(
                hook_name, hook,
                "case {case:?}: the hold must be taken by the group's own hook, got {actions:?}"
            );
            assert_eq!(
                token.chars().count(),
                TOKEN_LENGTH,
                "case {case:?}: the minted token must be {TOKEN_LENGTH} characters, got {token:?}"
            );
            tokens.push(token.clone());
        }
        tokens.sort_unstable();
        let minted = tokens.len();
        tokens.dedup();
        assert_eq!(
            tokens.len(),
            minted,
            "case {case:?}: every hold must carry its own token, got {actions:?}"
        );

        // RULE 2: the work the wait state exists for runs on a started instance, so Amazon
        // EC2 reports every held instance running while the hold lasts.
        for instance_id in &instances {
            let state = ec2_state(&router, case, instance_id).await;
            assert_eq!(
                state, EC2_RUNNING,
                "case {case:?}: Amazon EC2 must report the held instance {instance_id} \
                 {EC2_RUNNING:?}, got {state:?}"
            );
        }
    }
}

/// How the hold on a launch comes to an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// `CompleteLifecycleAction` naming the action's own token.
    ByToken,
    /// `CompleteLifecycleAction` naming the instance instead.
    ByInstance,
    /// `RecordLifecycleActionHeartbeat` first — which must not release anything — and then
    /// the token.
    HeartbeatThenToken,
    /// Nothing: the hook's own deadline applies its `DefaultResult`, which is `CONTINUE`
    /// here.
    ItsOwnDeadline,
}

/// RULE 3: releasing the last launch hold ADMITS the instance — the next read of the group
/// reports `InService`, with no further transient state in between — and Amazon EC2 runs it.
///
/// The rows differ along HOW the hold ended, which is what the rule generalises over: each
/// ending is the same release of the same wait.
#[tokio::test]
async fn a_released_launch_is_admitted_on_the_next_read() {
    let rows: &[(&str, Release)] = &[
        ("released by its token", Release::ByToken),
        ("released by the instance id", Release::ByInstance),
        ("heartbeated and then released", Release::HeartbeatThenToken),
        ("released by the hook's own deadline", Release::ItsOwnDeadline),
    ];

    for (index, (case, release)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-released-{index}");
        let hook = "hold";
        create_group(&router, case, &group, &subnet, 0, &[]).await;
        put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
        set_capacity(&router, case, &group, 1).await;
        let instances = poll_states(&router, case, &group, PENDING_WAIT, 1).await;
        let instance_id = instances
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
        let token = held_actions(&engine, &group)
            .first()
            .map(|(_, _, token)| token.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the hold must carry a token"));

        match release {
            Release::ItsOwnDeadline => time_out_every_hold(&engine, &group),
            Release::HeartbeatThenToken => {
                let (status, body) = query(
                    &router,
                    "RecordLifecycleActionHeartbeat",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("LifecycleHookName", hook),
                        ("LifecycleActionToken", token.as_str()),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the heartbeat must be accepted, got {status} with {body:?}"
                );
                // A heartbeat does not itself release the instance.
                let waiting = reported_states(&router, case, &group).await;
                assert_eq!(
                    waiting,
                    vec![PENDING_WAIT.to_string()],
                    "case {case:?}: a heartbeat keeps the instance waiting, got {waiting:?}"
                );
                complete(&router, case, &group, hook, &[
                    ("LifecycleActionToken", token.as_str()),
                    ("LifecycleActionResult", CONTINUE),
                ])
                .await;
            }
            Release::ByToken => {
                complete(&router, case, &group, hook, &[
                    ("LifecycleActionToken", token.as_str()),
                    ("LifecycleActionResult", CONTINUE),
                ])
                .await;
            }
            Release::ByInstance => {
                complete(&router, case, &group, hook, &[
                    ("InstanceId", instance_id.as_str()),
                    ("LifecycleActionResult", CONTINUE),
                ])
                .await;
            }
        }

        // The NEXT read: the span this service owes an instance coming into service was spent
        // in the wait, so nothing further is owed and the group reports it in service at once.
        let states = reported_states(&router, case, &group).await;
        assert_eq!(
            states,
            vec![IN_SERVICE.to_string()],
            "case {case:?}: the released instance must be reported {IN_SERVICE:?} on the next \
             read, got {states:?}"
        );
        let state = ec2_state(&router, case, &instance_id).await;
        assert_eq!(
            state, EC2_RUNNING,
            "case {case:?}: Amazon EC2 must report the admitted instance {instance_id} \
             {EC2_RUNNING:?}, got {state:?}"
        );
        assert!(
            held_actions(&engine, &group).is_empty(),
            "case {case:?}: a resolved hold is no longer held, got {:?}",
            held_actions(&engine, &group)
        );
    }
}

/// Post one `CompleteLifecycleAction` carrying `members` beyond the group and hook names,
/// which must be accepted.
async fn complete(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
    members: &[(&str, &str)],
) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", group),
        ("LifecycleHookName", hook),
    ];
    params.extend_from_slice(members);
    let (status, body) = query(router, "CompleteLifecycleAction", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the callback must be accepted, got {status} with {body:?}"
    );
}

/// Which action a refused callback addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// A hook of the group that is holding nothing, while the instance waits at another.
    AnotherHook,
    /// The hook that WAS holding the instance, after its action was consumed and the
    /// instance is in service.
    ConsumedAction,
    /// An instance the group does not hold at all.
    AbsentInstance,
    /// A token no action of the group is held under.
    UnknownToken,
    /// Neither identifier.
    Nothing,
}

/// RULE 4: a callback addressed at no held action is refused naming what the caller has to
/// correct — and naming the hook as well exactly when the instance it named IS waiting at a
/// hook of the group.
///
/// The rows differ along which half of the address matched nothing, and each row is driven
/// through BOTH doors that take the address (`CompleteLifecycleAction` and
/// `RecordLifecycleActionHeartbeat`), which refuse it identically.
#[tokio::test]
async fn a_callback_naming_no_held_action_says_which_half_matched_nothing() {
    let rows: &[(&str, Addressed)] = &[
        ("a hook of the group holding nothing", Addressed::AnotherHook),
        ("an action already consumed", Addressed::ConsumedAction),
        ("an instance the group does not hold", Addressed::AbsentInstance),
        ("a token no action is held under", Addressed::UnknownToken),
        ("neither the token nor the instance", Addressed::Nothing),
    ];

    for (index, (case, addressed)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-refusal-{index}");
        let hook = "hold";
        create_group(&router, case, &group, &subnet, 0, &[]).await;
        put_hook(&router, case, &group, hook, LAUNCHING, CONTINUE).await;
        set_capacity(&router, case, &group, 1).await;
        let instances = poll_states(&router, case, &group, PENDING_WAIT, 1).await;
        let instance_id = instances
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
        let token = held_actions(&engine, &group)
            .first()
            .map(|(_, _, token)| token.clone())
            .unwrap_or_else(|| panic!("case {case:?}: the hold must carry a token"));

        // A token the service never minted, in the shape the member is modelled in.
        let unknown_token = "00000000-0000-4000-8000-000000000000";
        let absent_instance = "i-0123456789abcdef0";
        if *addressed == Addressed::ConsumedAction {
            complete(&router, case, &group, hook, &[
                ("LifecycleActionToken", token.as_str()),
                ("LifecycleActionResult", CONTINUE),
            ])
            .await;
            let states = reported_states(&router, case, &group).await;
            assert_eq!(
                states,
                vec![IN_SERVICE.to_string()],
                "case {case:?}: the consumed action must have released the instance, got {states:?}"
            );
        }

        let (addressing, expected): (Vec<(&str, &str)>, String) = match addressed {
            Addressed::AnotherHook => (
                vec![("InstanceId", instance_id.as_str())],
                format!(
                    "No active Lifecycle Action found with instance ID {instance_id} and HookName \
                     not-a-hook"
                ),
            ),
            Addressed::ConsumedAction => (
                vec![("InstanceId", instance_id.as_str())],
                format!("No active Lifecycle Action found with instance ID {instance_id}"),
            ),
            Addressed::AbsentInstance => (
                vec![("InstanceId", absent_instance)],
                format!("No active Lifecycle Action found with instance ID {absent_instance}"),
            ),
            Addressed::UnknownToken => (
                vec![("LifecycleActionToken", unknown_token)],
                format!("No active Lifecycle Action found with token {unknown_token}"),
            ),
            Addressed::Nothing => (
                Vec::new(),
                "Either the lifecycle action token or the instance ID must be specified."
                    .to_string(),
            ),
        };
        // The hook the call names: the one holding nothing for the row that is about the hook
        // half of the address, and the group's own hook otherwise.
        let named_hook = if *addressed == Addressed::AnotherHook {
            "not-a-hook"
        } else {
            hook
        };

        for door in ["CompleteLifecycleAction", "RecordLifecycleActionHeartbeat"] {
            let mut params: Vec<(&str, &str)> = vec![
                ("AutoScalingGroupName", group.as_str()),
                ("LifecycleHookName", named_hook),
            ];
            params.extend_from_slice(&addressing);
            if door == "CompleteLifecycleAction" {
                params.push(("LifecycleActionResult", CONTINUE));
            }
            let (status, body) = query(&router, door, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?} through {door}: expected HTTP 400, got {status} with {body:?}"
            );
            assert_eq!(
                refusal_of(&body),
                ("ValidationError".to_string(), expected.clone()),
                "case {case:?} through {door}: got {body}"
            );
        }
    }
}

/// How a termination hook's hold comes to be abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandon {
    /// `CompleteLifecycleAction` with `ABANDON`.
    ByCallback,
    /// The hook's own deadline, whose `DefaultResult` is `ABANDON`.
    ByDeadline,
}

/// RULE 5: a termination a hook abandoned is RETAINED exactly while the group's
/// `TerminateHookAbandon` retention trigger says `retain` — the instance stays a member
/// Amazon EC2 runs, counts towards no capacity, and leaves only on
/// `TerminateInstanceInAutoScalingGroup`; under `terminate` the same abandon shuts it down.
///
/// The rows differ along the trigger's two published values
/// (`smithy_query(query="shape",shape="RetentionAction")`) and along HOW the hold was
/// abandoned, which the trigger's documentation covers together: "when a termination
/// lifecycle hook is abandoned due to failure, timeout, or explicit abandonment".
#[tokio::test]
async fn an_abandoned_termination_is_retained_only_under_the_retention_trigger() {
    let rows: &[(&str, &str, Abandon)] = &[
        ("retain, abandoned by the callback", "retain", Abandon::ByCallback),
        ("retain, abandoned by the deadline", "retain", Abandon::ByDeadline),
        ("terminate, abandoned by the callback", "terminate", Abandon::ByCallback),
        ("terminate, abandoned by the deadline", "terminate", Abandon::ByDeadline),
    ];

    for (index, (case, trigger, abandon)) in rows.iter().enumerate() {
        let (router, engine, subnet) = fixture().await;
        let group = format!("asg-retained-{index}");
        let hook = "teardown";
        create_group(
            &router,
            case,
            &group,
            &subnet,
            1,
            &[p(
                "InstanceLifecyclePolicy.RetentionTriggers.TerminateHookAbandon",
                *trigger,
            )],
        )
        .await;
        let members = poll_states(&router, case, &group, IN_SERVICE, 1).await;
        let subject = members
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the group's one member must be reported"));
        put_hook(&router, case, &group, hook, TERMINATING, ABANDON).await;

        // Take that one member out of service WITHOUT decrementing the capacity, so the
        // group's desired capacity has to be met by another instance while the hook holds
        // this one.
        let (status, body) = query(
            &router,
            "TerminateInstanceInAutoScalingGroup",
            &[
                ("InstanceId", subject.as_str()),
                ("ShouldDecrementDesiredCapacity", "false"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the termination must be accepted, got {status} with {body:?}"
        );
        let held = held_actions(&engine, &group);
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: the termination hook must hold the instance it took, got {held:?}"
        );
        assert_eq!(
            held[0].0, subject,
            "case {case:?}: the hold must be taken against the terminating instance, got {held:?}"
        );
        // And the group reports the held instance in the wait state of that transition.
        let instances = reported_instances(&router, case, &group).await;
        let states = reported_states(&router, case, &group).await;
        assert!(
            instances
                .iter()
                .zip(states.iter())
                .any(|(id, state)| id == &subject && state == TERMINATING_WAIT),
            "case {case:?}: the group must report {subject} {TERMINATING_WAIT:?}, got \
             {instances:?} {states:?}"
        );
        let token = held[0].2.clone();

        match abandon {
            Abandon::ByCallback => {
                complete(&router, case, &group, hook, &[
                    ("LifecycleActionToken", token.as_str()),
                    ("LifecycleActionResult", ABANDON),
                ])
                .await;
            }
            Abandon::ByDeadline => time_out_every_hold(&engine, &group),
        }

        if *trigger == "retain" {
            // The retained instance stays a member in its own state, and Amazon EC2 keeps
            // running it: the termination the hook abandoned did not happen.
            let deadline = std::time::Instant::now() + DEADLINE;
            loop {
                let instances = reported_instances(&router, case, &group).await;
                let states = reported_states(&router, case, &group).await;
                let retained = instances
                    .iter()
                    .zip(states.iter())
                    .any(|(id, state)| id == &subject && state == TERMINATING_RETAINED);
                // The group also meets its desired capacity with ANOTHER instance, because a
                // retained instance counts towards none.
                let in_service = states.iter().filter(|state| *state == IN_SERVICE).count();
                if retained && in_service == 1 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "case {case:?}: the group must report {subject} {TERMINATING_RETAINED:?} \
                     beside one instance in service, got {instances:?} {states:?}"
                );
                tokio::time::sleep(POLL).await;
            }
            let state = ec2_state(&router, case, &subject).await;
            assert_eq!(
                state, EC2_RUNNING,
                "case {case:?}: Amazon EC2 must keep running the retained instance {subject}, \
                 got {state:?}"
            );

            // And it leaves only on the call the trigger's documentation names.
            let (status, body) = query(
                &router,
                "TerminateInstanceInAutoScalingGroup",
                &[
                    ("InstanceId", subject.as_str()),
                    ("ShouldDecrementDesiredCapacity", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the retained instance must be terminable, got {status} with \
                 {body:?}"
            );
            let deadline = std::time::Instant::now() + DEADLINE;
            loop {
                let instances = reported_instances(&router, case, &group).await;
                if !instances.contains(&subject) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "case {case:?}: the terminated instance must leave the group, got \
                     {instances:?}"
                );
                tokio::time::sleep(POLL).await;
            }
            let state = ec2_state(&router, case, &subject).await;
            assert!(
                state == EC2_SHUTTING_DOWN || state == "terminated",
                "case {case:?}: Amazon EC2 must shut the terminated instance down, got {state:?}"
            );
        } else {
            // Under the default trigger the abandon is the termination carrying on.
            let deadline = std::time::Instant::now() + DEADLINE;
            loop {
                let instances = reported_instances(&router, case, &group).await;
                if !instances.contains(&subject) {
                    break;
                }
                let states = reported_states(&router, case, &group).await;
                assert!(
                    std::time::Instant::now() < deadline,
                    "case {case:?}: the abandoned instance must leave the group, got \
                     {instances:?} {states:?}"
                );
                tokio::time::sleep(POLL).await;
            }
            poll_ec2_state(&router, case, &subject, "terminated").await;
        }
    }
}

/// RULE 6: the per-group hook quota is refused with the LIMIT and no part of the request —
/// the same sentence whatever hook the request names.
///
/// Two rows, differing only in the name the 51st hook is asked for under, which is what the
/// rule generalises over: a quota refusal is about the ceiling, not about the resource that
/// happened to hit it.
#[tokio::test]
async fn the_hook_quota_refusal_states_the_limit_and_nothing_of_the_request() {
    let (router, _engine, subnet) = fixture().await;
    let group = "asg-hook-quota";
    create_group(&router, "the quota group", group, &subnet, 0, &[]).await;
    for index in 0..MAX_HOOKS_PER_GROUP {
        put_hook(
            &router,
            "filling the group to its ceiling",
            group,
            &format!("hook-{index}"),
            LAUNCHING,
            CONTINUE,
        )
        .await;
    }

    // Written out here rather than taken from the crate, so the assertion does not take the
    // handler's own word for the sentence.
    let expected =
        format!("You cannot have more than {MAX_HOOKS_PER_GROUP} hooks per Auto Scaling group");
    for case in ["past-the-limit", "a-differently-named-hook"] {
        let params = vec![
            p("AutoScalingGroupName", group),
            p("LifecycleHookName", case),
            p("LifecycleTransition", LAUNCHING),
        ];
        let (status, body) = query(&router, "PutLifecycleHook", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: one hook past the ceiling must be refused, got {status} with {body:?}"
        );
        assert_eq!(
            refusal_of(&body),
            ("LimitExceeded".to_string(), expected.clone()),
            "case {case:?}: got {body}"
        );
    }
}
