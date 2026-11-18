// SPDX-License-Identifier: CC0-1.0

//! The lifecycle hooks of an Auto Scaling group over the awsQuery wire:
//! `PutLifecycleHook`, `DescribeLifecycleHooks`, `DeleteLifecycleHook` and the hook
//! specifications a group can be created with.
//!
//! The rule under test: a lifecycle hook is a NAMED CHILD of one Auto Scaling group, so
//! `PutLifecycleHook` is an upsert over that group's own collection — a name the group
//! does not hold is CREATED with the documented defaults for every member the request
//! omits (`HeartbeatTimeout` 3600, `DefaultResult` ABANDON) and requires a
//! `LifecycleTransition`, while a name it holds is UPDATED per member: an omitted member
//! keeps the stored value, a supplied one overwrites it, and an empty
//! `NotificationTargetARN` clears the stored target rather than counting as omitted. The
//! group's reader reports exactly what is stored (with `GlobalTimeout` derived from the
//! hook's own heartbeat timeout), the delete removes exactly the hook it names, and
//! deleting the group takes its hooks with it.
//!
//! The tables below span what that rule generalises over — which members a request
//! carries, whether the call creates or updates, the value written and whether it
//! replaces an earlier one, each published transition and default result, both published
//! heartbeat bounds and one past each, how many hooks the group holds, which hooks a read
//! filters for, and which of the two writers (the upsert or the group create) stored the
//! hook. Every row reads the hooks back through `DescribeLifecycleHooks`, so no row can
//! pass on an accepted-but-unstored change.
//!
//! This suite OWNS the hook harness for this crate's tests — the writers, the reader and
//! the projection they are asserted through are defined here once, and
//! `describe_lifecycle_hooks_projection.rs`, which covers what the reader projects out of
//! a group's collection, drives the same helpers rather than keeping a second copy.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape,
};

/// The launch source every group here is created from — a launch TEMPLATE, because no
/// account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The zone every group here places into.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The transitions a lifecycle hook can be created for: the catalog
/// `DescribeLifecycleHookTypes` publishes, from the model's own documentation for
/// `PutLifecycleHookType$LifecycleTransition` ("To create a lifecycle hook for scale-out
/// events, specify autoscaling:EC2_INSTANCE_LAUNCHING. To create a lifecycle hook for
/// scale-in events, specify autoscaling:EC2_INSTANCE_TERMINATING").
pub(crate) const TRANSITIONS: [&str; 2] = [
    "autoscaling:EC2_INSTANCE_LAUNCHING",
    "autoscaling:EC2_INSTANCE_TERMINATING",
];

/// The results a hook may default to, from the model's own documentation for
/// `PutLifecycleHookType$DefaultResult` ("Valid values: CONTINUE | ABANDON").
pub(crate) const RESULTS: [&str; 2] = ["CONTINUE", "ABANDON"];

/// `DefaultResult` for a hook created without one, from the same documentation ("The
/// default value is ABANDON").
const DEFAULT_RESULT: &str = "ABANDON";

/// `HeartbeatTimeout` for a hook created without one, from the model's own documentation
/// for `PutLifecycleHookType$HeartbeatTimeout` ("The default value is 3600 seconds (1
/// hour)").
const DEFAULT_HEARTBEAT_TIMEOUT: i32 = 3600;

/// The published bounds of `HeartbeatTimeout`, from the same documentation ("The range is
/// from 30 to 7200 seconds").
pub(crate) const MIN_HEARTBEAT_TIMEOUT: i32 = 30;
pub(crate) const MAX_HEARTBEAT_TIMEOUT: i32 = 7200;

/// The ceiling on a hook's `GlobalTimeout`, from the model's own documentation for
/// `LifecycleHook$GlobalTimeout` ("The maximum is 172800 seconds (48 hours) or 100 times
/// HeartbeatTimeout, whichever is smaller").
const MAX_GLOBAL_TIMEOUT: i32 = 172_800;

/// The most hooks one group may hold, from the model's own documentation for
/// `PutLifecycleHook` ("your maximum limit of lifecycle hooks, which by default is 50 per
/// Auto Scaling group").
const MAX_HOOKS_PER_GROUP: usize = 50;

/// What `GlobalTimeout` a hook with this heartbeat timeout must report: the documented
/// derivation, written out here so the assertion does not take the handler's own word.
fn expected_global_timeout(heartbeat_timeout: i32) -> i32 {
    (heartbeat_timeout * 100).min(MAX_GLOBAL_TIMEOUT)
}

/// The whole message a request naming a value outside the set a lifecycle-hook member
/// admits is refused with: the member as the model names it, the admissible values in
/// the order the service publishes them, and no part of the request.
///
/// Spelled out here rather than taken from the crate, so the assertion does not take the
/// handler's own word for the sentence.
pub(crate) fn outside_value_set(member: &str, catalog: &[&str]) -> String {
    format!("'{member}' must be one of: {}", catalog.join(", "))
}

/// The whole message a call naming a lifecycle hook this service holds none of is
/// refused with: both identifiers the request addressed it by, quoted.
pub(crate) fn absent_hook_message(hook_name: &str, group_name: &str) -> String {
    format!("No Lifecycle Hook found with name '{hook_name}' for group '{group_name}'")
}

/// The whole message a `HeartbeatTimeout` outside the documented range is refused with:
/// the member and both published bounds, with no part of the request in it.
pub(crate) fn heartbeat_range_message() -> String {
    format!(
        "HeartbeatTimeout must be between {MIN_HEARTBEAT_TIMEOUT} and {MAX_HEARTBEAT_TIMEOUT} \
         seconds"
    )
}

/// The whole message `PutLifecycleHook` refuses a group the account does not hold with:
/// the absent-group prefix this service opens every such refusal with, and the detail
/// form this operation serves — that no such group exists, naming the one asked for.
pub(crate) fn absent_group_message(name: &str) -> String {
    format!("AutoScalingGroup name not found - No such Auto Scaling group: {name}")
}

/// One hook as `DescribeLifecycleHooks` reports it. Every member is kept as the wire
/// spelled it, `""` for one the answer omits, so an assertion can tell a cleared member
/// from a stored one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedHook {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) transition: String,
    pub(crate) notification_target_arn: String,
    pub(crate) role_arn: String,
    pub(crate) notification_metadata: String,
    pub(crate) heartbeat_timeout: String,
    pub(crate) global_timeout: String,
    pub(crate) default_result: String,
}

/// Create an empty group (nothing to launch, so nothing settles on the clock while the
/// hooks are written and read).
pub(crate) async fn create_group(router: &MultiServiceRouter, name: &str) {
    create_group_with(router, name, &[]).await;
}

/// Create an empty group carrying `extra` request members, and assert the create was
/// served.
async fn create_group_with(router: &MultiServiceRouter, name: &str, extra: &[(String, String)]) {
    let (status, body) = create_group_result(router, name, extra).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one `CreateAutoScalingGroup` for an empty group carrying `extra`, and answer its
/// status and body.
async fn create_group_result(
    router: &MultiServiceRouter,
    name: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p(ZONE.0, ZONE.1),
    ];
    params.extend_from_slice(extra);
    query(router, "CreateAutoScalingGroup", &borrowed(&params)).await
}

/// Post one `PutLifecycleHook` naming `members` beyond the group and hook names, and
/// answer its status, wire error code and message.
pub(crate) async fn put_hook(
    router: &MultiServiceRouter,
    group: &str,
    hook: &str,
    members: &[(String, String)],
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
    ];
    params.extend_from_slice(members);
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one `DeleteLifecycleHook`, and answer its status, wire error code and message.
pub(crate) async fn delete_hook(
    router: &MultiServiceRouter,
    group: &str,
    hook: &str,
) -> (u16, String, String) {
    let (status, body) = query(
        router,
        "DeleteLifecycleHook",
        &[("AutoScalingGroupName", group), ("LifecycleHookName", hook)],
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one `DescribeLifecycleHooks`, filtering for `names` when any are given, and
/// answer its status, wire error code and message.
pub(crate) async fn describe_result(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            format!("LifecycleHookNames.member.{}", index + 1).as_str(),
            *name,
        ));
    }
    let (status, body) = query(router, "DescribeLifecycleHooks", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The hooks the group reports, in the order the answer carries them.
pub(crate) async fn described_hooks(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> Vec<ReportedHook> {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            format!("LifecycleHookNames.member.{}", index + 1).as_str(),
            *name,
        ));
    }
    let (status, body) = query(router, "DescribeLifecycleHooks", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the read-back of {group:?} must succeed, got {status} with {body:?}"
    );
    // The hook members live inside the answer's own `LifecycleHooks` element; the
    // envelope carries other elements, so the slice is narrowed to that one first.
    let Some((_, rest)) = body.split_once("<LifecycleHooks>") else {
        return Vec::new();
    };
    let inner = rest
        .split_once("</LifecycleHooks>")
        .map_or(rest, |(inner, _)| inner);
    tag_values(inner, "member")
        .iter()
        // Every value is decoded: a member can carry the entities the envelope escapes
        // its text with (the notification metadata is free text), so the assertion
        // compares what a client parses rather than its wire spelling.
        .map(|member| {
            let text = |tag: &str| xml_unescape(&tag_value(member, tag));
            ReportedHook {
                name: text("LifecycleHookName"),
                group: text("AutoScalingGroupName"),
                transition: text("LifecycleTransition"),
                notification_target_arn: text("NotificationTargetARN"),
                role_arn: text("RoleARN"),
                notification_metadata: text("NotificationMetadata"),
                heartbeat_timeout: text("HeartbeatTimeout"),
                global_timeout: text("GlobalTimeout"),
                default_result: text("DefaultResult"),
            }
        })
        .collect()
}

/// The one hook the group reports under `hook`.
pub(crate) async fn described_hook(
    router: &MultiServiceRouter,
    group: &str,
    hook: &str,
) -> ReportedHook {
    let reported = described_hooks(router, group, &[hook]).await;
    assert_eq!(
        reported.len(),
        1,
        "the group {group:?} must report exactly the hook {hook:?}, got {reported:?}"
    );
    reported.into_iter().next().expect("one reported hook")
}

/// One create row: the case, the members the request carries beyond the names, and what
/// the group must then report for the hook.
struct CreateRow {
    case: &'static str,
    members: Vec<(String, String)>,
    expected: ReportedHook,
}

/// What a created hook must look like, filled in per row.
pub(crate) fn expect(
    group: &str,
    hook: &str,
    transition: &str,
    heartbeat_timeout: i32,
    default_result: &str,
) -> ReportedHook {
    ReportedHook {
        name: hook.to_string(),
        group: group.to_string(),
        transition: transition.to_string(),
        notification_target_arn: String::new(),
        role_arn: String::new(),
        notification_metadata: String::new(),
        heartbeat_timeout: heartbeat_timeout.to_string(),
        global_timeout: expected_global_timeout(heartbeat_timeout).to_string(),
        default_result: default_result.to_string(),
    }
}

/// A notification target of the shape a hook names: an Amazon SNS topic ARN of the
/// account, which the model documents as one of the two kinds of target.
fn topic_arn(suffix: &str) -> String {
    format!("arn:aws:sns:us-west-2:000000000000:asg-hook-{suffix}")
}

/// A role of the shape a hook names.
fn role_arn(suffix: &str) -> String {
    format!("arn:aws:iam::000000000000:role/asg-hook-{suffix}")
}

/// Every create row: the hook is stored with exactly the members the request carried and
/// the documented default for each one it omitted, read back off the group itself.
#[tokio::test]
async fn a_put_creates_the_hook_with_the_documented_defaults() {
    let group = "asg-create";
    let mut rows = vec![
        CreateRow {
            case: "the transition alone, so every other member defaults",
            members: vec![p("LifecycleTransition", TRANSITIONS[0])],
            expected: expect(
                group,
                "hook-1",
                TRANSITIONS[0],
                DEFAULT_HEARTBEAT_TIMEOUT,
                DEFAULT_RESULT,
            ),
        },
        CreateRow {
            case: "the scale-in transition, which the other rows do not use",
            members: vec![p("LifecycleTransition", TRANSITIONS[1])],
            expected: expect(
                group,
                "hook-2",
                TRANSITIONS[1],
                DEFAULT_HEARTBEAT_TIMEOUT,
                DEFAULT_RESULT,
            ),
        },
        CreateRow {
            case: "a notification target and the role that publishes to it",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("NotificationTargetARN", topic_arn("a")),
                p("RoleARN", role_arn("a")),
            ],
            expected: ReportedHook {
                notification_target_arn: topic_arn("a"),
                role_arn: role_arn("a"),
                ..expect(
                    group,
                    "hook-3",
                    TRANSITIONS[0],
                    DEFAULT_HEARTBEAT_TIMEOUT,
                    DEFAULT_RESULT,
                )
            },
        },
        CreateRow {
            case: "notification metadata and a heartbeat timeout",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[1]),
                p("NotificationMetadata", "{\"stage\":\"drain\"}"),
                p("HeartbeatTimeout", "120"),
            ],
            expected: ReportedHook {
                notification_metadata: "{\"stage\":\"drain\"}".to_string(),
                ..expect(group, "hook-4", TRANSITIONS[1], 120, DEFAULT_RESULT)
            },
        },
        CreateRow {
            case: "every member at once",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("NotificationTargetARN", topic_arn("b")),
                p("RoleARN", role_arn("b")),
                p("NotificationMetadata", "drain-then-continue"),
                p("HeartbeatTimeout", "600"),
                p("DefaultResult", RESULTS[0]),
            ],
            expected: ReportedHook {
                notification_target_arn: topic_arn("b"),
                role_arn: role_arn("b"),
                notification_metadata: "drain-then-continue".to_string(),
                ..expect(group, "hook-5", TRANSITIONS[0], 600, RESULTS[0])
            },
        },
    ];
    // Each published `DefaultResult`, so the row that writes one is not the only
    // evidence that the member is stored rather than defaulted.
    for (index, result) in RESULTS.iter().enumerate() {
        rows.push(CreateRow {
            case: "an explicit default result",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[index % TRANSITIONS.len()]),
                p("DefaultResult", *result),
            ],
            expected: expect(
                group,
                &format!("hook-result-{result}"),
                TRANSITIONS[index % TRANSITIONS.len()],
                DEFAULT_HEARTBEAT_TIMEOUT,
                result,
            ),
        });
    }
    // Each published heartbeat bound, and the derived `GlobalTimeout` that follows from
    // it: the smaller bound derives a timeout below the ceiling, the larger one is capped
    // by it.
    for timeout in [MIN_HEARTBEAT_TIMEOUT, MAX_HEARTBEAT_TIMEOUT] {
        rows.push(CreateRow {
            case: "a published heartbeat bound",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", timeout.to_string()),
            ],
            expected: expect(
                group,
                &format!("hook-heartbeat-{timeout}"),
                TRANSITIONS[0],
                timeout,
                DEFAULT_RESULT,
            ),
        });
    }

    let router = router_with(new_engine());
    create_group(&router, group).await;

    for row in &rows {
        let hook = row.expected.name.as_str();
        let (status, code, message) = put_hook(&router, group, hook, &row.members).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 for {hook:?}, got {status} with {code:?} {message:?}",
            row.case
        );
        let actual = described_hook(&router, group, hook).await;
        assert_eq!(
            actual, row.expected,
            "case {:?}: expected {:?}, got {actual:?}",
            row.case, row.expected
        );
    }

    // Every hook of the table is still there, and an unfiltered read reports all of
    // them: the collection is a collection, not one slot.
    let all = described_hooks(&router, group, &[]).await;
    assert_eq!(
        all.len(),
        rows.len(),
        "the group must report every hook the table created, got {all:?}"
    );
}

/// One update row: the case, the members the second call carries, and what the hook must
/// report afterwards given what the first call stored.
struct UpdateRow {
    case: &'static str,
    members: Vec<(String, String)>,
    expect_after: fn(ReportedHook) -> ReportedHook,
}

/// Every update row: the members the second call carries overwrite, the members it omits
/// keep what the first call stored, and an empty `NotificationTargetARN` clears the
/// stored target.
#[tokio::test]
async fn a_put_on_an_existing_hook_updates_only_the_members_it_carries() {
    let rows = [
        UpdateRow {
            case: "the heartbeat timeout alone, leaving the target and the role",
            members: vec![p("HeartbeatTimeout", "120")],
            expect_after: |before| ReportedHook {
                heartbeat_timeout: "120".to_string(),
                global_timeout: expected_global_timeout(120).to_string(),
                ..before
            },
        },
        UpdateRow {
            case: "an empty notification target, which overrides the stored one",
            members: vec![p("NotificationTargetARN", "")],
            expect_after: |before| ReportedHook {
                notification_target_arn: String::new(),
                ..before
            },
        },
        UpdateRow {
            case: "a different notification target",
            members: vec![p("NotificationTargetARN", topic_arn("moved"))],
            expect_after: |before| ReportedHook {
                notification_target_arn: topic_arn("moved"),
                ..before
            },
        },
        UpdateRow {
            case: "the other transition, which an update may change",
            members: vec![p("LifecycleTransition", TRANSITIONS[1])],
            expect_after: |before| ReportedHook {
                transition: TRANSITIONS[1].to_string(),
                ..before
            },
        },
        UpdateRow {
            case: "a new default result",
            members: vec![p("DefaultResult", RESULTS[0])],
            expect_after: |before| ReportedHook {
                default_result: RESULTS[0].to_string(),
                ..before
            },
        },
        UpdateRow {
            case: "new metadata and a new role at once",
            members: vec![
                p("NotificationMetadata", "second"),
                p("RoleARN", role_arn("second")),
            ],
            expect_after: |before| ReportedHook {
                notification_metadata: "second".to_string(),
                role_arn: role_arn("second"),
                ..before
            },
        },
        UpdateRow {
            case: "nothing beyond the names, which changes nothing",
            members: vec![],
            expect_after: |before| before,
        },
        UpdateRow {
            case: "the same values again, which changes nothing either",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("NotificationTargetARN", topic_arn("first")),
                p("RoleARN", role_arn("first")),
                p("NotificationMetadata", "first"),
                p("HeartbeatTimeout", "300"),
                p("DefaultResult", RESULTS[1]),
            ],
            expect_after: |before| before,
        },
    ];

    for (row_index, row) in rows.iter().enumerate() {
        // One emulator per row, so each row's update acts on the same starting hook.
        let router = router_with(new_engine());
        let group = format!("asg-update-{row_index}");
        create_group(&router, &group).await;

        let created = vec![
            p("LifecycleTransition", TRANSITIONS[0]),
            p("NotificationTargetARN", topic_arn("first")),
            p("RoleARN", role_arn("first")),
            p("NotificationMetadata", "first"),
            p("HeartbeatTimeout", "300"),
            p("DefaultResult", RESULTS[1]),
        ];
        let (status, code, message) = put_hook(&router, &group, "hook", &created).await;
        assert_eq!(
            status, 200,
            "case {:?}: the create must succeed, got {status} with {code:?} {message:?}",
            row.case
        );
        // A second hook of the same group, left untouched by every row: an update of one
        // hook is an update of ONE hook.
        let (status, code, message) = put_hook(
            &router,
            &group,
            "other-hook",
            &[p("LifecycleTransition", TRANSITIONS[1])],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the second hook must be created, got {status} with {code:?} {message:?}",
            row.case
        );

        let before = described_hook(&router, &group, "hook").await;
        let (status, code, message) = put_hook(&router, &group, "hook", &row.members).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} with {code:?} {message:?}",
            row.case
        );

        let expected = (row.expect_after)(before);
        let actual = described_hook(&router, &group, "hook").await;
        assert_eq!(
            actual, expected,
            "case {:?}: expected {expected:?}, got {actual:?}",
            row.case
        );

        let untouched = described_hook(&router, &group, "other-hook").await;
        assert_eq!(
            untouched,
            expect(
                &group,
                "other-hook",
                TRANSITIONS[1],
                DEFAULT_HEARTBEAT_TIMEOUT,
                DEFAULT_RESULT,
            ),
            "case {:?}: the other hook of the group must be untouched, got {untouched:?}",
            row.case
        );
    }
}

/// A create needs a transition and an update does not: the model documents
/// `LifecycleTransition` as "Required for new lifecycle hooks, but optional when updating
/// existing hooks", and nothing is stored for the refused create.
#[tokio::test]
async fn a_transition_is_required_only_to_create_a_hook() {
    let router = router_with(new_engine());
    create_group(&router, "asg-transition").await;

    let (status, code, message) = put_hook(&router, "asg-transition", "hook", &[]).await;
    assert_eq!(
        status, 400,
        "a create without a transition must be refused, got {status} with {code:?} {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code, got {code:?} {message:?}"
    );
    let expected = "The LifecycleTransition is required to create the lifecycle hook hook, and \
                    may be omitted only when updating one that already exists";
    assert_eq!(
        message, expected,
        "expected the message {expected:?}, got {message:?}"
    );
    let hooks = described_hooks(&router, "asg-transition", &[]).await;
    assert!(
        hooks.is_empty(),
        "the refused create stored nothing, got {hooks:?}"
    );

    let (status, code, message) = put_hook(
        &router,
        "asg-transition",
        "hook",
        &[p("LifecycleTransition", TRANSITIONS[0])],
    )
    .await;
    assert_eq!(
        status, 200,
        "the create naming a transition must succeed, got {status} with {code:?} {message:?}"
    );
    let (status, code, message) =
        put_hook(&router, "asg-transition", "hook", &[p("HeartbeatTimeout", "60")]).await;
    assert_eq!(
        status, 200,
        "the update may omit the transition, got {status} with {code:?} {message:?}"
    );
    let actual = described_hook(&router, "asg-transition", "hook").await;
    assert_eq!(
        actual,
        expect(
            "asg-transition",
            "hook",
            TRANSITIONS[0],
            60,
            DEFAULT_RESULT
        ),
        "the update keeps the transition the create stored, got {actual:?}"
    );
}

/// One refused row: the case, the members the request carries, and the whole message it
/// must be refused with.
struct RefusedRow {
    case: &'static str,
    members: Vec<(String, String)>,
    message: String,
}

/// The value rules: a transition outside the published catalog, a default result outside
/// the published pair and a heartbeat timeout one past either published bound are each
/// refused `ValidationError`, and nothing is written — the hook the group already holds
/// is unchanged.
#[tokio::test]
async fn a_put_carrying_a_value_outside_its_rule_is_refused_and_writes_nothing() {
    let heartbeat_range_sentence = heartbeat_range_message();
    let rows = [
        RefusedRow {
            case: "a transition of neither published kind",
            members: vec![p("LifecycleTransition", "autoscaling:EC2_INSTANCE_REBOOTING")],
            message: outside_value_set("LifecycleTransition", &TRANSITIONS),
        },
        RefusedRow {
            case: "a transition spelled without its namespace",
            members: vec![p("LifecycleTransition", "EC2_INSTANCE_LAUNCHING")],
            message: outside_value_set("LifecycleTransition", &TRANSITIONS),
        },
        RefusedRow {
            case: "a transition spelled in the wrong case",
            members: vec![p(
                "LifecycleTransition",
                "autoscaling:ec2_instance_launching",
            )],
            message: outside_value_set("LifecycleTransition", &TRANSITIONS),
        },
        RefusedRow {
            case: "a default result of neither published value",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("DefaultResult", "PROCEED"),
            ],
            message: outside_value_set("DefaultResult", &RESULTS),
        },
        RefusedRow {
            case: "a default result in the wrong case",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("DefaultResult", "continue"),
            ],
            message: outside_value_set("DefaultResult", &RESULTS),
        },
        RefusedRow {
            case: "a heartbeat timeout one second below the published minimum",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", (MIN_HEARTBEAT_TIMEOUT - 1).to_string()),
            ],
            message: heartbeat_range_sentence.clone(),
        },
        RefusedRow {
            case: "a heartbeat timeout one second above the published maximum",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", (MAX_HEARTBEAT_TIMEOUT + 1).to_string()),
            ],
            message: heartbeat_range_sentence.clone(),
        },
        RefusedRow {
            case: "a heartbeat timeout of zero",
            members: vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", "0"),
            ],
            message: heartbeat_range_sentence.clone(),
        },
    ];

    let router = router_with(new_engine());
    create_group(&router, "asg-values").await;
    let stored = vec![
        p("LifecycleTransition", TRANSITIONS[0]),
        p("HeartbeatTimeout", "300"),
    ];
    let (status, code, message) = put_hook(&router, "asg-values", "hook", &stored).await;
    assert_eq!(
        status, 200,
        "the hook the rows must leave alone has to be created, got {status} with {code:?} \
         {message:?}"
    );
    let before = described_hook(&router, "asg-values", "hook").await;

    for row in &rows {
        // The same name as the stored hook, so a row that wrote anything would be caught
        // by the read-back below — a refused UPDATE writes nothing either.
        let (status, code, message) = put_hook(&router, "asg-values", "hook", &row.members).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {code:?} {message:?}",
            row.case
        );
        assert_eq!(
            code, "ValidationError",
            "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
            row.case
        );
        assert_eq!(
            message, row.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            row.case, row.message
        );
        let actual = described_hook(&router, "asg-values", "hook").await;
        assert_eq!(
            actual, before,
            "case {:?}: a refused call leaves the stored hook as it was, got {actual:?}",
            row.case
        );
    }
}

/// The documented per-group ceiling: the group holds the maximum, and one hook more is
/// refused with the operation's modelled `LimitExceededFault` while an UPDATE of a hook
/// it already holds still succeeds at the ceiling.
#[tokio::test]
async fn a_put_past_the_documented_hook_limit_is_refused() {
    let router = router_with(new_engine());
    create_group(&router, "asg-limit").await;

    for index in 0..MAX_HOOKS_PER_GROUP {
        let hook = format!("hook-{index}");
        let (status, code, message) = put_hook(
            &router,
            "asg-limit",
            &hook,
            &[p("LifecycleTransition", TRANSITIONS[index % TRANSITIONS.len()])],
        )
        .await;
        assert_eq!(
            status, 200,
            "the hook {hook:?} is inside the limit and must be stored, got {status} with \
             {code:?} {message:?}"
        );
    }
    let held = described_hooks(&router, "asg-limit", &[]).await;
    assert_eq!(
        held.len(),
        MAX_HOOKS_PER_GROUP,
        "the group must hold the documented maximum, got {} hooks",
        held.len()
    );

    let (status, code, message) = put_hook(
        &router,
        "asg-limit",
        "hook-past-the-limit",
        &[p("LifecycleTransition", TRANSITIONS[0])],
    )
    .await;
    assert_eq!(
        status, 400,
        "one hook past the limit must be refused, got {status} with {code:?} {message:?}"
    );
    // `LimitExceededFault` carries the awsQuery error code `LimitExceeded`
    // (smithy_query(query="operation",operation="PutLifecycleHook")).
    assert_eq!(
        code, "LimitExceeded",
        "expected the LimitExceeded code past the limit, got {code:?} {message:?}"
    );
    let expected = format!(
        "The lifecycle hook hook-past-the-limit exceeds the limit of {MAX_HOOKS_PER_GROUP} \
         lifecycle hooks per Auto Scaling group"
    );
    assert_eq!(
        message, expected,
        "expected the message {expected:?}, got {message:?}"
    );
    assert_eq!(
        described_hooks(&router, "asg-limit", &[]).await.len(),
        MAX_HOOKS_PER_GROUP,
        "the refused create stored nothing"
    );

    // An UPDATE at the ceiling creates no hook, so the limit does not apply to it.
    let (status, code, message) =
        put_hook(&router, "asg-limit", "hook-0", &[p("HeartbeatTimeout", "90")]).await;
    assert_eq!(
        status, 200,
        "an update at the ceiling must succeed, got {status} with {code:?} {message:?}"
    );
    assert_eq!(
        described_hook(&router, "asg-limit", "hook-0").await.heartbeat_timeout,
        "90",
        "the update at the ceiling must be stored"
    );
}

/// What a read filters for: no names is every hook of the group, a subset is exactly
/// those hooks, and a name the group does not hold contributes nothing rather than
/// refusing the read.
#[tokio::test]
async fn a_read_reports_the_hooks_it_filters_for() {
    let router = router_with(new_engine());
    create_group(&router, "asg-read").await;
    create_group(&router, "asg-read-other").await;

    for hook in ["hook-a", "hook-b", "hook-c"] {
        let (status, code, message) = put_hook(
            &router,
            "asg-read",
            hook,
            &[p("LifecycleTransition", TRANSITIONS[0])],
        )
        .await;
        assert_eq!(
            status, 200,
            "the hook {hook:?} must be created, got {status} with {code:?} {message:?}"
        );
    }
    // A hook of ANOTHER group, to show the read is scoped to the group it names.
    let (status, code, message) = put_hook(
        &router,
        "asg-read-other",
        "hook-elsewhere",
        &[p("LifecycleTransition", TRANSITIONS[1])],
    )
    .await;
    assert_eq!(
        status, 200,
        "the other group's hook must be created, got {status} with {code:?} {message:?}"
    );

    let cases: [(&str, &[&str], Vec<&str>); 6] = [
        ("no names, so every hook", &[], vec!["hook-a", "hook-b", "hook-c"]),
        ("one name", &["hook-b"], vec!["hook-b"]),
        ("two names", &["hook-a", "hook-c"], vec!["hook-a", "hook-c"]),
        (
            "the names in the reverse of the order they were created",
            &["hook-c", "hook-a"],
            vec!["hook-a", "hook-c"],
        ),
        ("a name the group does not hold", &["hook-z"], vec![]),
        (
            "a name of ANOTHER group's hook",
            &["hook-elsewhere"],
            vec![],
        ),
    ];
    for (case, names, expected) in cases {
        let reported = described_hooks(&router, "asg-read", names).await;
        let actual: Vec<String> = reported.iter().map(|hook| hook.name.clone()).collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the hooks {expected:?}, got {reported:?}"
        );
        for hook in &reported {
            assert_eq!(
                hook.group, "asg-read",
                "case {case:?}: every reported hook belongs to the group read, got {hook:?}"
            );
        }
    }
}

/// The delete removes exactly the hook it names, is refused for a name the group does
/// not hold, and frees the name for a later create.
#[tokio::test]
async fn a_delete_removes_exactly_the_hook_it_names() {
    let router = router_with(new_engine());
    create_group(&router, "asg-delete").await;

    for hook in ["hook-a", "hook-b"] {
        let (status, code, message) = put_hook(
            &router,
            "asg-delete",
            hook,
            &[
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", "300"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "the hook {hook:?} must be created, got {status} with {code:?} {message:?}"
        );
    }

    let (status, code, message) = delete_hook(&router, "asg-delete", "hook-a").await;
    assert_eq!(
        status, 200,
        "the delete must succeed, got {status} with {code:?} {message:?}"
    );
    let remaining: Vec<String> = described_hooks(&router, "asg-delete", &[])
        .await
        .iter()
        .map(|hook| hook.name.clone())
        .collect();
    assert_eq!(
        remaining,
        vec!["hook-b".to_string()],
        "only the hook the delete named is gone, got {remaining:?}"
    );

    // Deleting it again, and deleting a name the group never held, name no hook to
    // delete: each is refused, and the hooks the group does hold are untouched.
    for hook in ["hook-a", "hook-never-stored"] {
        let (status, code, message) = delete_hook(&router, "asg-delete", hook).await;
        assert_eq!(
            status, 400,
            "case {hook:?}: deleting a hook the group does not hold must be refused, got \
             {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {hook:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        let expected = absent_hook_message(hook, "asg-delete");
        assert_eq!(
            message, expected,
            "case {hook:?}: expected the message {expected:?}, got {message:?}"
        );
    }
    assert_eq!(
        described_hooks(&router, "asg-delete", &[]).await.len(),
        1,
        "those deletes removed nothing"
    );

    // The name is free again: a put under it CREATES a hook, with the defaults rather
    // than anything the deleted hook carried.
    let (status, code, message) = put_hook(
        &router,
        "asg-delete",
        "hook-a",
        &[p("LifecycleTransition", TRANSITIONS[1])],
    )
    .await;
    assert_eq!(
        status, 200,
        "the name must be free to create again, got {status} with {code:?} {message:?}"
    );
    let actual = described_hook(&router, "asg-delete", "hook-a").await;
    assert_eq!(
        actual,
        expect(
            "asg-delete",
            "hook-a",
            TRANSITIONS[1],
            DEFAULT_HEARTBEAT_TIMEOUT,
            DEFAULT_RESULT,
        ),
        "the recreated hook carries the documented defaults, got {actual:?}"
    );
}

/// A hook belongs to its group: deleting the group takes its hooks with it, so a group
/// created again under the same name holds none.
#[tokio::test]
async fn a_deleted_group_takes_its_hooks_with_it() {
    let router = router_with(new_engine());
    create_group(&router, "asg-owned").await;
    let (status, code, message) = put_hook(
        &router,
        "asg-owned",
        "hook",
        &[p("LifecycleTransition", TRANSITIONS[0])],
    )
    .await;
    assert_eq!(
        status, 200,
        "the hook must be created, got {status} with {code:?} {message:?}"
    );

    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", "asg-owned")],
    )
    .await;
    assert_eq!(status, 200, "the group delete must succeed, got {status} with {body:?}");

    create_group(&router, "asg-owned").await;
    let hooks = described_hooks(&router, "asg-owned", &[]).await;
    assert!(
        hooks.is_empty(),
        "a group created again under the same name holds no hook of the deleted one, got \
         {hooks:?}"
    );
}

/// The other writer of the same collection: a group created WITH lifecycle hook
/// specifications holds exactly those hooks, under the same defaults, and a specification
/// the service cannot accept refuses the create whole — no group and no hook.
#[tokio::test]
async fn a_group_created_with_hook_specifications_reports_them() {
    let router = router_with(new_engine());
    let specifications = vec![
        p("LifecycleHookSpecificationList.member.1.LifecycleHookName", "spec-a"),
        p(
            "LifecycleHookSpecificationList.member.1.LifecycleTransition",
            TRANSITIONS[0],
        ),
        p(
            "LifecycleHookSpecificationList.member.1.HeartbeatTimeout",
            "120",
        ),
        p("LifecycleHookSpecificationList.member.2.LifecycleHookName", "spec-b"),
        p(
            "LifecycleHookSpecificationList.member.2.LifecycleTransition",
            TRANSITIONS[1],
        ),
        p(
            "LifecycleHookSpecificationList.member.2.NotificationTargetARN",
            topic_arn("spec"),
        ),
        p(
            "LifecycleHookSpecificationList.member.2.RoleARN",
            role_arn("spec"),
        ),
        p(
            "LifecycleHookSpecificationList.member.2.DefaultResult",
            RESULTS[0],
        ),
    ];
    create_group_with(&router, "asg-spec", &specifications).await;

    let actual = described_hooks(&router, "asg-spec", &[]).await;
    let expected = vec![
        expect("asg-spec", "spec-a", TRANSITIONS[0], 120, DEFAULT_RESULT),
        ReportedHook {
            notification_target_arn: topic_arn("spec"),
            role_arn: role_arn("spec"),
            ..expect(
                "asg-spec",
                "spec-b",
                TRANSITIONS[1],
                DEFAULT_HEARTBEAT_TIMEOUT,
                RESULTS[0],
            )
        },
    ];
    assert_eq!(
        actual, expected,
        "expected the specified hooks {expected:?}, got {actual:?}"
    );

    // A specification carrying a value outside its rule refuses the create: no group, so
    // no hook either.
    let refused = vec![
        p("LifecycleHookSpecificationList.member.1.LifecycleHookName", "spec-bad"),
        p(
            "LifecycleHookSpecificationList.member.1.LifecycleTransition",
            "autoscaling:EC2_INSTANCE_REBOOTING",
        ),
    ];
    let (status, body) = create_group_result(&router, "asg-spec-bad", &refused).await;
    assert_eq!(
        status, 400,
        "a create carrying an unknown transition must be refused, got {status} with {body:?}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "ValidationError",
        "expected the ValidationError code, got {body:?}"
    );
    let expected = outside_value_set("LifecycleTransition", &TRANSITIONS);
    assert_eq!(
        xml_unescape(&tag_value(&body, "Message")),
        expected,
        "expected the message {expected:?}, got {body:?}"
    );
    let (status, _, message) = describe_result(&router, "asg-spec-bad", &[]).await;
    assert_eq!(
        status, 200,
        "the refused create stored no group, and the read of a name no group answers to \
         reports no hooks rather than being refused, got {status} with {message:?}"
    );
}

/// A hook-scoped call naming a group the account does not hold: the PUT states that no
/// such group exists, naming the one the request asked for, the DELETE reports the hook
/// it could not find (naming the hook and the group it was addressed to), and the READ
/// reports no hooks at all.
#[tokio::test]
async fn a_call_against_an_absent_group_is_refused() {
    let router = router_with(new_engine());
    create_group(&router, "asg-present").await;

    for absent in ["asg-absent", "ASG-Present", "asg-present-but-longer"] {
        let (status, code, message) = put_hook(
            &router,
            absent,
            "hook",
            &[p("LifecycleTransition", TRANSITIONS[0])],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {absent:?}: the put must be refused, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {absent:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        let expected = absent_group_message(absent);
        assert_eq!(
            message, expected,
            "case {absent:?}: expected the message {expected:?}, got {message:?}"
        );

        let (status, code, message) = delete_hook(&router, absent, "hook").await;
        assert_eq!(
            status, 400,
            "case {absent:?}: the delete must be refused, got {status} with {code:?} {message:?}"
        );
        let expected = absent_hook_message("hook", absent);
        assert_eq!(
            message, expected,
            "case {absent:?}: expected the message {expected:?}, got {message:?}"
        );

        let (status, code, message) = describe_result(&router, absent, &[]).await;
        assert_eq!(
            status, 200,
            "case {absent:?}: the read reports no hooks rather than refusing, got {status} \
             with {code:?} {message:?}"
        );
        let hooks = described_hooks(&router, absent, &[]).await;
        assert!(
            hooks.is_empty(),
            "case {absent:?}: expected no hooks for a name no group answers to, got {hooks:?}"
        );
    }

    let hooks = described_hooks(&router, "asg-present", &[]).await;
    assert!(
        hooks.is_empty(),
        "the refusals above wrote nothing on the group that does exist, got {hooks:?}"
    );
}

// ---------------------------------------------------------------------------
// THE HOLD a stored hook puts in front of the transition it names.
//
// The rule: a hook is not a record a client reads back — when the group holds one for the
// transition an instance is taking, that instance is put into the transition's WAIT state
// and a `LifecycleActionToken` is minted for the hold; it stays there until the token (or
// the instance id) is passed back through `CompleteLifecycleAction`, with
// `RecordLifecycleActionHeartbeat` keeping it waiting, or until the hook's heartbeat runs
// out and the hook's own `DefaultResult` is applied. A launch that carries on then comes
// into service, a launch that is abandoned leaves the group, and a termination resumes
// either way.
//
// The rows span what that rule generalises over: WHICH transition is held, HOW the hold
// ends (the instance id, the token, a heartbeat and then the token, or the hook's own
// deadline), and WHICH result ends it. The announcement of the hold on the account's event
// bus — the only place the token reaches a client — is the cross-service half of the same
// rule and is covered by `core/router/tests/all/autoscaling_lifecycle_hook_actions.rs`,
// which drives the whole mounted service set.
// ---------------------------------------------------------------------------

/// The two wait states a held instance is reported in, and the state a launch that carried
/// on reaches: the model's own `LifecycleState` values
/// (`smithy_query(query="shape",shape="LifecycleState")`), written out here so a wrong
/// constant in the implementation cannot make these rows pass.
const PENDING_WAIT: &str = "Pending:Wait";
const TERMINATING_WAIT: &str = "Terminating:Wait";
const IN_SERVICE: &str = "InService";

/// How long a `LifecycleActionToken` is: `@length(min: 36, max: 36)` on the model's own
/// `LifecycleActionToken` shape.
const TOKEN_LENGTH: usize = 36;

/// The launch source the held groups launch from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const HOLD_TEMPLATE: &str = "lt-hold";

/// How long a leg waits for what it is waiting on before giving up, and how often it looks:
/// far above the work one capacity change makes, and driven off what the group REPORTS
/// rather than off a fixed sleep, so a loaded machine delays a row instead of breaking it.
const HOLD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const HOLD_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// The one instance the group holds and the `LifecycleState` it is reported in, or `None`
/// when the group holds none.
async fn held_instance(router: &MultiServiceRouter, group: &str) -> Option<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group {group:?} must be readable, got {status} with {body:?}"
    );
    let ids = tag_values(&body, "InstanceId");
    assert!(
        ids.len() <= 1,
        "every group of this table holds at most one instance, got {ids:?}"
    );
    ids.into_iter()
        .next()
        .map(|id| (id, tag_value(&body, "LifecycleState")))
}

/// Look at the group until it reports `expected` for the one instance it holds — `None`
/// meaning it holds none at all — or [`HOLD_DEADLINE`] passes; answer that instance's id.
async fn poll_state(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    expected: Option<&str>,
) -> Option<String> {
    let deadline = std::time::Instant::now() + HOLD_DEADLINE;
    loop {
        let held = held_instance(router, group).await;
        if held.as_ref().map(|(_, state)| state.as_str()) == expected {
            return held.map(|(id, _)| id);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the group must report {expected:?}, got {held:?}"
        );
        tokio::time::sleep(HOLD_POLL).await;
    }
}

/// The lifecycle actions the store is holding for the group, as
/// `(instance id, hook name, token)`.
///
/// Read off the store because the token is published to the account's event bus and
/// nowhere in this service's own API: what is under test here is the hold itself, so the
/// token is taken from where the handler wrote it and passed back over the wire, which is
/// the only way a callback can address the action at all.
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

/// Backdate every hold the group has taken, so each one's own deadline has passed.
///
/// The shortest `HeartbeatTimeout` the service accepts ([`MIN_HEARTBEAT_TIMEOUT`]) is far
/// longer than a test may wait, so a row drives the CLOCK rather than waiting for it: the
/// deadline the hold stored is moved into the past, and the next read of the store
/// materialises what that instant made due exactly as it would have on its own.
fn time_out_every_hold(engine: &std::sync::Arc<engine::InMemoryEngine>, group: &str) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let stored = store
        .groups
        .get_mut(group)
        .unwrap_or_else(|| panic!("the group {group:?} must be stored for its holds to time out"));
    for instance in &mut stored.instances {
        for action in &mut instance.pending_lifecycle_actions {
            action.heartbeat_due = std::time::SystemTime::UNIX_EPOCH;
            action.global_due = std::time::SystemTime::UNIX_EPOCH;
        }
    }
}

/// One row of the hold table.
struct HoldRow {
    case: &'static str,
    /// The transition the row's hook holds.
    transition: &'static str,
    /// The `LifecycleState` the held instance must be reported in.
    wait_state: &'static str,
    /// The hook's `DefaultResult`, which is what its own deadline applies.
    default_result: &'static str,
    /// The result a callback passes back, or `None` to leave the hold to that deadline.
    callback: Option<&'static str>,
    /// Whether the callback is addressed by the action's TOKEN (and heartbeated first)
    /// rather than by the instance id.
    by_token: bool,
    /// What the group must report for the instance once the hold has ended and settled:
    /// `None` for an instance that left the group.
    settles_at: Option<&'static str>,
}

/// THE RULE: a hook holds the transition it names until the action it minted is resolved.
#[tokio::test]
async fn a_hook_holds_the_transition_it_is_configured_for() {
    let rows: &[HoldRow] = &[
        HoldRow {
            case: "a launch continued by the instance id",
            transition: TRANSITIONS[0],
            wait_state: PENDING_WAIT,
            default_result: RESULTS[1],
            callback: Some(RESULTS[0]),
            by_token: false,
            settles_at: Some(IN_SERVICE),
        },
        HoldRow {
            case: "a launch heartbeated and then continued by its token",
            transition: TRANSITIONS[0],
            wait_state: PENDING_WAIT,
            default_result: RESULTS[1],
            callback: Some(RESULTS[0]),
            by_token: true,
            settles_at: Some(IN_SERVICE),
        },
        HoldRow {
            case: "a launch abandoned by its token",
            transition: TRANSITIONS[0],
            wait_state: PENDING_WAIT,
            default_result: RESULTS[0],
            callback: Some(RESULTS[1]),
            by_token: true,
            settles_at: None,
        },
        HoldRow {
            case: "a launch left to its own deadline, defaulting to CONTINUE",
            transition: TRANSITIONS[0],
            wait_state: PENDING_WAIT,
            default_result: RESULTS[0],
            callback: None,
            by_token: false,
            settles_at: Some(IN_SERVICE),
        },
        HoldRow {
            case: "a launch left to its own deadline, defaulting to ABANDON",
            transition: TRANSITIONS[0],
            wait_state: PENDING_WAIT,
            default_result: RESULTS[1],
            callback: None,
            by_token: false,
            settles_at: None,
        },
        HoldRow {
            case: "a termination continued by the instance id",
            transition: TRANSITIONS[1],
            wait_state: TERMINATING_WAIT,
            default_result: RESULTS[1],
            callback: Some(RESULTS[0]),
            by_token: false,
            settles_at: None,
        },
        HoldRow {
            case: "a termination abandoned by its token",
            transition: TRANSITIONS[1],
            wait_state: TERMINATING_WAIT,
            default_result: RESULTS[0],
            callback: Some(RESULTS[1]),
            by_token: true,
            settles_at: None,
        },
        HoldRow {
            case: "a termination left to its own deadline",
            transition: TRANSITIONS[1],
            wait_state: TERMINATING_WAIT,
            default_result: RESULTS[1],
            callback: None,
            by_token: false,
            settles_at: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One emulator per row, so one row's group and hook cannot decide another's.
        let (router, engine) = crate::support::emulator();
        crate::support::create_launch_template(&router, HOLD_TEMPLATE).await;
        let subnet = crate::support::seed_subnets(&engine, 1)
            .first()
            .expect("one seeded subnet")
            .id
            .clone();

        let group = format!("asg-held-{index}");
        let params = vec![
            p("AutoScalingGroupName", &group),
            p("LaunchTemplate.LaunchTemplateName", HOLD_TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", &subnet),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group must be created, got {status} with {body:?}"
        );
        let (status, code, message) = put_hook(
            &router,
            &group,
            "hold",
            &[
                p("LifecycleTransition", row.transition),
                p("DefaultResult", row.default_result),
                p("HeartbeatTimeout", "300"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the hook must be stored, got {status} with {code:?} {message:?}"
        );

        // What puts the instance into the held transition: the capacity change the row's
        // hook is configured for. A terminating hook holds the SCALE-IN, so the group's one
        // instance has to be in service before it.
        set_capacity(&router, case, &group, 1).await;
        if row.transition == TRANSITIONS[1] {
            poll_state(&router, case, &group, Some(IN_SERVICE)).await;
            set_capacity(&router, case, &group, 0).await;
        }

        let instance_id = poll_state(&router, case, &group, Some(row.wait_state))
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the held instance must be reported"));
        // The hold the hook took: one action, under that hook, carrying a token of the
        // modelled length.
        let actions = held_actions(&engine, &group);
        assert_eq!(
            actions.len(),
            1,
            "case {case:?}: the one hook holding the instance must hold one action, got \
             {actions:?}"
        );
        let (held_id, hook_name, token) = actions[0].clone();
        assert_eq!(
            (held_id.as_str(), hook_name.as_str()),
            (instance_id.as_str(), "hold"),
            "case {case:?}: the action must be held against the reported instance by the \
             group's own hook, got {actions:?}"
        );
        assert_eq!(
            token.chars().count(),
            TOKEN_LENGTH,
            "case {case:?}: the minted token must be {TOKEN_LENGTH} characters, got {token:?}"
        );

        match row.callback {
            None => {
                time_out_every_hold(&engine, &group);
                // The next read materialises what that instant made due.
                held_instance(&router, &group).await;
            }
            Some(result) if row.by_token => {
                // The heartbeat keeps the hold waiting, and the group still reports it so.
                let (status, body) = query(
                    &router,
                    "RecordLifecycleActionHeartbeat",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("LifecycleHookName", "hold"),
                        ("LifecycleActionToken", token.as_str()),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the heartbeat must be accepted, got {status} with {body:?}"
                );
                let still = held_instance(&router, &group).await;
                assert_eq!(
                    still.as_ref().map(|(_, state)| state.as_str()),
                    Some(row.wait_state),
                    "case {case:?}: a heartbeat keeps the instance waiting, got {still:?}"
                );
                complete_hold(
                    &router,
                    case,
                    &group,
                    "hold",
                    &[
                        ("LifecycleActionToken", token.as_str()),
                        ("LifecycleActionResult", result),
                    ],
                )
                .await;
            }
            Some(result) => {
                complete_hold(
                    &router,
                    case,
                    &group,
                    "hold",
                    &[
                        ("InstanceId", instance_id.as_str()),
                        ("LifecycleActionResult", result),
                    ],
                )
                .await;
            }
        }

        poll_state(&router, case, &group, row.settles_at).await;
        assert!(
            held_actions(&engine, &group).is_empty(),
            "case {case:?}: a resolved hold is no longer held, got {:?}",
            held_actions(&engine, &group)
        );
    }
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
        "case {case:?}: SetDesiredCapacity {desired} must be accepted, got {status} with \
         {body:?}"
    );
}

/// Post one `CompleteLifecycleAction` against the hook `hook`, carrying `members` beyond
/// the group and hook names, which must be accepted.
async fn complete_hold(
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

/// A hold does not outlive the move it was waiting on: taking an instance out of service
/// while a LAUNCH hook is still holding it discards that hold, so the instance leaves the
/// group, the hold's own deadline cannot bring it back, and the token it was addressed by
/// resolves nothing.
///
/// Two rows, differing along what the group holds for the termination it is now taking: no
/// hook at all (the instance goes straight out) and a termination hook (the launch hold is
/// replaced by that hook's own, under a new token). Each group's launch hook defaults to
/// CONTINUE, so a hold left behind would be the one that reports a terminated instance in
/// service again.
#[tokio::test]
async fn a_held_instance_taken_out_of_service_leaves_the_group() {
    // (case, whether the group also holds a hook for the terminating transition)
    for (index, (case, also_terminating)) in [
        ("a group holding only the launch hook", false),
        ("a group holding a termination hook as well", true),
    ]
    .into_iter()
    .enumerate()
    {
        let (router, engine) = crate::support::emulator();
        crate::support::create_launch_template(&router, HOLD_TEMPLATE).await;
        let subnet = crate::support::seed_subnets(&engine, 1)
            .first()
            .expect("one seeded subnet")
            .id
            .clone();

        let group = format!("asg-held-out-{index}");
        let params = vec![
            p("AutoScalingGroupName", &group),
            p("LaunchTemplate.LaunchTemplateName", HOLD_TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", &subnet),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the group must be created, got {status} with {body:?}"
        );
        let (status, code, message) = put_hook(
            &router,
            &group,
            "hold",
            &[
                p("LifecycleTransition", TRANSITIONS[0]),
                p("DefaultResult", RESULTS[0]),
                p("HeartbeatTimeout", "300"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the launch hook must be stored, got {status} with {code:?} \
             {message:?}"
        );
        if also_terminating {
            let (status, code, message) = put_hook(
                &router,
                &group,
                "hold-out",
                &[
                    p("LifecycleTransition", TRANSITIONS[1]),
                    p("DefaultResult", RESULTS[1]),
                    p("HeartbeatTimeout", "300"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the termination hook must be stored, got {status} with \
                 {code:?} {message:?}"
            );
        }

        set_capacity(&router, case, &group, 1).await;
        poll_state(&router, case, &group, Some(PENDING_WAIT)).await;
        let launch_hold = held_actions(&engine, &group);
        assert_eq!(
            launch_hold.len(),
            1,
            "case {case:?}: the launch hook must be holding the instance, got {launch_hold:?}"
        );
        let stale_token = launch_hold[0].2.clone();

        // The scale-in takes the instance the launch hook is still holding.
        set_capacity(&router, case, &group, 0).await;

        if also_terminating {
            // The launch hold is over, so the termination hook's own hold replaces it: a new
            // action under that hook, and the token the launch hold was addressed by
            // resolves nothing.
            poll_state(&router, case, &group, Some(TERMINATING_WAIT)).await;
            let terminate_hold = held_actions(&engine, &group);
            assert_eq!(
                terminate_hold.len(),
                1,
                "case {case:?}: exactly the termination hook's hold must remain, got \
                 {terminate_hold:?}"
            );
            assert_eq!(
                terminate_hold[0].1, "hold-out",
                "case {case:?}: the remaining hold belongs to the termination hook, got \
                 {terminate_hold:?}"
            );
            assert_ne!(
                terminate_hold[0].2, stale_token,
                "case {case:?}: the termination hold carries its own token, got \
                 {terminate_hold:?}"
            );
            let (status, body) = query(
                &router,
                "CompleteLifecycleAction",
                &[
                    ("AutoScalingGroupName", group.as_str()),
                    ("LifecycleHookName", "hold"),
                    ("LifecycleActionToken", stale_token.as_str()),
                    ("LifecycleActionResult", RESULTS[0]),
                ],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: the discarded launch token must resolve nothing, got {status} \
                 with {body:?}"
            );
            assert_eq!(
                xml_unescape(&tag_value(&body, "Message")),
                format!("No active Lifecycle Action found with token {stale_token}"),
                "case {case:?}: got {body:?}"
            );
            complete_hold(
                &router,
                case,
                &group,
                "hold-out",
                &[
                    ("InstanceId", terminate_hold[0].0.as_str()),
                    ("LifecycleActionResult", RESULTS[0]),
                ],
            )
            .await;
        } else {
            assert!(
                held_actions(&engine, &group).is_empty(),
                "case {case:?}: the launch hold must be discarded when the instance is taken \
                 out of service, got {:?}",
                held_actions(&engine, &group)
            );
        }

        // The instance leaves the group, and nothing the clock owes brings it back: the
        // group reports none from then on.
        poll_state(&router, case, &group, None).await;
        for _ in 0..3 {
            tokio::time::sleep(HOLD_POLL).await;
            let held = held_instance(&router, &group).await;
            assert!(
                held.is_none(),
                "case {case:?}: a group whose only instance was taken out of service reports \
                 none, got {held:?}"
            );
        }
    }
}
