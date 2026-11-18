// SPDX-License-Identifier: CC0-1.0

//! An EXPLICIT empty `NotificationTargetARN` on a lifecycle hook, over the awsQuery wire.
//!
//! The RULE under test: `PutLifecycleHook` creates OR updates one hook of a group as a
//! PER-MEMBER update, so a member the request omits keeps the value the stored hook
//! carries while a member it carries overwrites that value — and a member the model admits
//! the empty string for carries that empty value as a VALUE, which clears the stored one
//! ("If you specify an empty string, this overrides the current ARN", the model's own
//! documentation for `PutLifecycleHookType$NotificationTargetARN`, whose target
//! `NotificationTargetResourceName` is `length 0..255`). `DescribeLifecycleHooks` then
//! reports whichever of the two states the hook is in, so a cleared target is never read
//! back as the ARN it replaced and an untouched one is never lost to an update that does
//! not name it.
//!
//! The tables here span what that rule generalises over: which WRITER stored the hook (the
//! upsert or the hook specifications of the group create), whether the empty value arrives
//! on the call that CREATES the hook or on a later one, which single member a narrow update
//! carries (each published heartbeat bound, each published default result, the other
//! transition, the metadata, the role, and nothing at all), how many calls follow a clear,
//! and whether the hook read is the one that was written.
//!
//! `lifecycle_hooks.rs` OWNS the harness these rows drive — the group create, the upsert
//! and the projection of an answered body are its helpers, used here rather than copied.

use super::{
    create_group, create_group_with, described_hook, expect, expected_global_timeout, put_hook,
    role_arn, topic_arn, ReportedHook, MAX_HEARTBEAT_TIMEOUT, MIN_HEARTBEAT_TIMEOUT, RESULTS,
    TRANSITIONS,
};
use crate::support::{new_engine, p, query, router_with};

/// The heartbeat timeout every hook below is created with: inside the published range, and
/// not the documented default, so a member that was never named cannot be mistaken for one
/// that was.
const CREATED_HEARTBEAT_TIMEOUT: i32 = 300;

/// The metadata every hook below is created with, for the same reason.
const CREATED_METADATA: &str = "created";

/// Which writer stored the hook whose target is read back.
///
/// Both writers of a group's hook collection take the same members, so the empty value is
/// followed through each of them rather than through the upsert alone.
#[derive(Debug, Clone, Copy)]
enum Writer {
    /// `PutLifecycleHook`, carrying the empty target on the call that CREATES the hook.
    UpsertOnCreate,
    /// `CreateAutoScalingGroup`, carrying the empty target in the group's own
    /// `LifecycleHookSpecificationList`.
    GroupCreateSpecification,
    /// `PutLifecycleHook` twice: a create that stores a target, then a call that carries
    /// the empty value for it.
    UpsertThenClear,
}

/// The members a hook is written with, beyond the names and the target: the same set for
/// every row, so what differs between rows is the target alone.
fn created_members() -> Vec<(String, String)> {
    vec![
        p("LifecycleTransition", TRANSITIONS[0]),
        p("RoleARN", role_arn("override")),
        p("NotificationMetadata", CREATED_METADATA),
        p("HeartbeatTimeout", CREATED_HEARTBEAT_TIMEOUT.to_string()),
        p("DefaultResult", RESULTS[1]),
    ]
}

/// What the group must report for a hook written with [`created_members`] holding `target`
/// (`""` for a hook holding none).
fn expected_hook(group: &str, hook: &str, target: &str) -> ReportedHook {
    ReportedHook {
        notification_target_arn: target.to_string(),
        role_arn: role_arn("override"),
        notification_metadata: CREATED_METADATA.to_string(),
        ..expect(
            group,
            hook,
            TRANSITIONS[0],
            CREATED_HEARTBEAT_TIMEOUT,
            RESULTS[1],
        )
    }
}

/// The `LifecycleHookSpecificationList` members of a group create that writes `hook` with
/// [`created_members`] and the target `target`.
fn hook_specification(hook: &str, target: &str) -> Vec<(String, String)> {
    let mut params = vec![p("LifecycleHookSpecificationList.member.1.LifecycleHookName", hook)];
    params.extend(created_members().into_iter().map(|(member, value)| {
        p(
            format!("LifecycleHookSpecificationList.member.1.{member}").as_str(),
            value,
        )
    }));
    params.push(p(
        "LifecycleHookSpecificationList.member.1.NotificationTargetARN",
        target,
    ));
    params
}

/// An empty `NotificationTargetARN` stores NO target, whichever writer carried it and
/// whether it arrived on the call that created the hook or on a later one — and the hook's
/// other members, and the OTHER hooks of the group, are untouched by it.
#[tokio::test]
async fn an_empty_notification_target_stores_no_target_whichever_writer_carried_it() {
    for writer in [
        Writer::UpsertOnCreate,
        Writer::GroupCreateSpecification,
        Writer::UpsertThenClear,
    ] {
        let case = format!("{writer:?}");
        let router = router_with(new_engine());
        let group = format!("asg-cleared-{case}");
        let hook = "hook-cleared";

        match writer {
            Writer::UpsertOnCreate => {
                create_group(&router, &group).await;
                let mut members = created_members();
                members.push(p("NotificationTargetARN", ""));
                let (status, code, message) = put_hook(&router, &group, hook, &members).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 for the create, got {status} with {code:?} \
                     {message:?}"
                );
            }
            Writer::GroupCreateSpecification => {
                create_group_with(&router, &group, &hook_specification(hook, "")).await;
            }
            Writer::UpsertThenClear => {
                create_group(&router, &group).await;
                let mut members = created_members();
                members.push(p("NotificationTargetARN", topic_arn("stored")));
                let (status, code, message) = put_hook(&router, &group, hook, &members).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 for the create, got {status} with {code:?} \
                     {message:?}"
                );
                let stored = described_hook(&router, &group, hook).await;
                assert_eq!(
                    stored,
                    expected_hook(&group, hook, &topic_arn("stored")),
                    "case {case:?}: the create must store the target it named, got {stored:?}"
                );
                let (status, code, message) =
                    put_hook(&router, &group, hook, &[p("NotificationTargetARN", "")]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 for the clearing call, got {status} with \
                     {code:?} {message:?}"
                );
            }
        }

        // A second hook of the same group, carrying a target of its own: clearing one
        // hook's target is clearing ONE hook's target.
        let mut other = created_members();
        other.push(p("NotificationTargetARN", topic_arn("other")));
        let (status, code, message) = put_hook(&router, &group, "hook-other", &other).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second hook must be created, got {status} with {code:?} \
             {message:?}"
        );

        let actual = described_hook(&router, &group, hook).await;
        assert_eq!(
            actual,
            expected_hook(&group, hook, ""),
            "case {case:?}: the hook must report no notification target, got {actual:?}"
        );
        let untouched = described_hook(&router, &group, "hook-other").await;
        assert_eq!(
            untouched,
            expected_hook(&group, "hook-other", &topic_arn("other")),
            "case {case:?}: the group's other hook must keep its own target, got {untouched:?}"
        );

        // The ANSWER itself, not only its projection: a hook holding no target is reported
        // with no `NotificationTargetARN` member at all — and the member IS carried for the
        // hook that holds one, so that absence is the hook's state rather than a reader
        // that never reports the member.
        let member_open = "<NotificationTargetARN";
        let (status, body) = query(
            &router,
            "DescribeLifecycleHooks",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("LifecycleHookNames.member.1", hook),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back must succeed, got {status} with {body:?}"
        );
        assert!(
            !body.contains(member_open),
            "case {case:?}: a hook holding no target must be reported without the member, got \
             {body:?}"
        );
        let (status, body) = query(
            &router,
            "DescribeLifecycleHooks",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("LifecycleHookNames.member.1", "hook-other"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back of the other hook must succeed, got {status} with \
             {body:?}"
        );
        assert!(
            body.contains(member_open) && body.contains(&topic_arn("other")),
            "case {case:?}: the hook that holds a target must be reported with it, got {body:?}"
        );
    }
}

/// One narrow-update row: the case, the single member a later call carries, and what the
/// hook must report afterwards given what it reported before — the named member
/// overwritten, every other member, the notification target included, kept.
struct NarrowUpdate {
    case: String,
    members: Vec<(String, String)>,
    expect_after: Box<dyn Fn(ReportedHook) -> ReportedHook>,
}

/// A target the request does not name is KEPT and a target it cleared STAYS cleared: over
/// every member a narrow update can carry, the stored target survives an update that does
/// not name it, the empty value clears it, the clear survives the same narrow update and
/// repeating it, and a later call that names a target stores that one.
#[tokio::test]
async fn a_narrow_update_keeps_the_target_it_does_not_name_and_a_cleared_one_stays_cleared() {
    let mut rows = vec![
        NarrowUpdate {
            case: "the other transition".to_string(),
            members: vec![p("LifecycleTransition", TRANSITIONS[1])],
            expect_after: Box::new(|before| ReportedHook {
                transition: TRANSITIONS[1].to_string(),
                ..before
            }),
        },
        NarrowUpdate {
            case: "new metadata".to_string(),
            members: vec![p("NotificationMetadata", "updated")],
            expect_after: Box::new(|before| ReportedHook {
                notification_metadata: "updated".to_string(),
                ..before
            }),
        },
        NarrowUpdate {
            case: "a new role".to_string(),
            members: vec![p("RoleARN", role_arn("updated"))],
            expect_after: Box::new(|before| ReportedHook {
                role_arn: role_arn("updated"),
                ..before
            }),
        },
        NarrowUpdate {
            case: "nothing beyond the names".to_string(),
            members: Vec::new(),
            expect_after: Box::new(|before| before),
        },
    ];
    // Each published heartbeat bound and each published default result, so the rows span
    // the value sets those members admit rather than one value of each.
    for timeout in [MIN_HEARTBEAT_TIMEOUT, MAX_HEARTBEAT_TIMEOUT] {
        rows.push(NarrowUpdate {
            case: format!("the published heartbeat bound {timeout}"),
            members: vec![p("HeartbeatTimeout", timeout.to_string())],
            expect_after: Box::new(move |before| ReportedHook {
                heartbeat_timeout: timeout.to_string(),
                global_timeout: expected_global_timeout(timeout).to_string(),
                ..before
            }),
        });
    }
    for result in RESULTS {
        rows.push(NarrowUpdate {
            case: format!("the published default result {result}"),
            members: vec![p("DefaultResult", result)],
            expect_after: Box::new(move |before| ReportedHook {
                default_result: result.to_string(),
                ..before
            }),
        });
    }

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row, so each row acts on the same starting hook.
        let router = router_with(new_engine());
        let group = format!("asg-narrow-{index:02}");
        let hook = "hook-narrow";
        create_group(&router, &group).await;

        let mut created = created_members();
        created.push(p("NotificationTargetARN", topic_arn("first")));
        let (status, code, message) = put_hook(&router, &group, hook, &created).await;
        assert_eq!(
            status, 200,
            "case {:?}: the create must succeed, got {status} with {code:?} {message:?}",
            row.case
        );

        // The member the row names overwrites; the target it does not name is kept.
        let before = described_hook(&router, &group, hook).await;
        let (status, code, message) = put_hook(&router, &group, hook, &row.members).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 for the narrow update, got {status} with {code:?} \
             {message:?}",
            row.case
        );
        let actual = described_hook(&router, &group, hook).await;
        assert_eq!(
            actual.notification_target_arn,
            topic_arn("first"),
            "case {:?}: an update that does not name the target must keep it, got {actual:?}",
            row.case
        );
        let expected = (row.expect_after)(before.clone());
        assert_eq!(
            actual, expected,
            "case {:?}: expected {expected:?} from {before:?}, got {actual:?}",
            row.case
        );

        // The empty value clears the stored target, and leaves every other member as the
        // narrow update left it.
        let cleared_from = described_hook(&router, &group, hook).await;
        let (status, code, message) =
            put_hook(&router, &group, hook, &[p("NotificationTargetARN", "")]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 for the clearing call, got {status} with {code:?} \
             {message:?}",
            row.case
        );
        let cleared = described_hook(&router, &group, hook).await;
        assert_eq!(
            cleared,
            ReportedHook {
                notification_target_arn: String::new(),
                ..cleared_from.clone()
            },
            "case {:?}: the empty value must clear the target and nothing else, got {cleared:?}",
            row.case
        );

        // The clear holds: the same narrow update again, and the empty value again, leave
        // the hook holding no target.
        for (round, members) in [
            ("the narrow update again", row.members.clone()),
            (
                "the empty value again",
                vec![p("NotificationTargetARN", "")],
            ),
        ] {
            let (status, code, message) = put_hook(&router, &group, hook, &members).await;
            assert_eq!(
                status, 200,
                "case {:?} / {round:?}: expected HTTP 200, got {status} with {code:?} {message:?}",
                row.case
            );
            let actual = described_hook(&router, &group, hook).await;
            assert_eq!(
                actual, cleared,
                "case {:?} / {round:?}: the hook must still hold no target, got {actual:?}",
                row.case
            );
        }

        // And a cleared target is not a closed door: a later call that names one stores it.
        let (status, code, message) = put_hook(
            &router,
            &group,
            hook,
            &[p("NotificationTargetARN", topic_arn("second"))],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 for the call that names a target again, got {status} \
             with {code:?} {message:?}",
            row.case
        );
        let restored = described_hook(&router, &group, hook).await;
        assert_eq!(
            restored,
            ReportedHook {
                notification_target_arn: topic_arn("second"),
                ..cleared
            },
            "case {:?}: the target named after a clear must be stored, got {restored:?}",
            row.case
        );
    }
}
