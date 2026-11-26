// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a request about a scheduled scaling action its Auto Scaling group holds no
//! record of is answered with, over the awsQuery wire.
//!
//! The rule under test: that answer states the subject that was not found in FIXED text and
//! takes nothing from the request — the same sentence whatever name was asked for, and
//! whichever of the two deletes asked for it (as this operation's own `ValidationError`
//! message, or as one `FailedScheduledActions` entry's `ErrorMessage`, where the name the
//! entry is about is carried by its sibling `ScheduledActionName` member).
//!
//! The table spans what the rule generalises over — the shape, the length and the case of the
//! name asked for, whether it was never created or deleted a moment ago, how many such names
//! one call states, and which operation states them — rather than one recorded row. The
//! expected sentence is written out here rather than taken from the crate, and every assertion
//! names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape,
};

/// The launch source every group in this suite is created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where every group here places the instances it would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The instant every action of this suite is scheduled for: one far enough ahead that no case
/// depends on when it would run.
const START_TIME: &str = "2035-06-01T00:00:00Z";

/// The wire error code this service answers a request about a resource it has no record of
/// with: Amazon EC2 Auto Scaling's single generic client-error code.
const VALIDATION_ERROR: &str = "ValidationError";

/// The WHOLE sentence such a request is answered with: fixed text naming the subject that was
/// not found, with no part of the request in it. Written out here so the assertion does not
/// take the handler's own word for it.
const ABSENT_ACTION_MESSAGE: &str = "Scheduled action name not found";

/// The longest name a request may state the action by: `ScheduledActionName` targets
/// `XmlStringMaxLen255` (length 1..255) on both `DeleteScheduledActionType` and
/// `BatchDeleteScheduledActionType$ScheduledActionNames`
/// (smithy_query(query="operation",operation="DeleteScheduledAction,BatchDeleteScheduledAction")).
const MAX_ACTION_NAME_LENGTH: usize = 255;

/// Which operation a case asks through: the two that take a named action out of a group's
/// collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// `DeleteScheduledAction`, which names the one action it deletes and reports a name it
    /// could not delete as its own refusal.
    One,
    /// `BatchDeleteScheduledAction`, which reports such a name per entry.
    Batch,
}

/// A router carrying Amazon EC2 Auto Scaling on one engine, with the group `name` created on
/// it and nothing to launch.
async fn given_group(name: &str) -> MultiServiceRouter {
    let router = router_with(new_engine());
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ZONE,
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
    router
}

/// Put the scheduled action `action` on `group`, timed and setting one size.
async fn given_action(router: &MultiServiceRouter, group: &str, action: &str) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("ScheduledActionName", action),
        p("StartTime", START_TIME),
        p("MinSize", "1"),
    ];
    let (status, body) = query(router, "PutScheduledUpdateGroupAction", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "putting {action:?} on {group:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one `DeleteScheduledAction` and answer its status, wire error code and message.
async fn delete_one(
    router: &MultiServiceRouter,
    group: &str,
    action: &str,
) -> (u16, String, String) {
    let (status, body) = query(
        router,
        "DeleteScheduledAction",
        &[
            ("AutoScalingGroupName", group),
            ("ScheduledActionName", action),
        ],
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one `BatchDeleteScheduledAction` over `names` and answer its status, the entries it
/// reported as failed (name, error code, message) in the order it reported them, and its body.
async fn delete_batch(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> (u16, Vec<(String, String, String)>, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            &format!("ScheduledActionNames.member.{}", index + 1),
            *name,
        ));
    }
    let (status, body) = query(router, "BatchDeleteScheduledAction", &borrowed(&params)).await;
    let failed = list_members(&body, "FailedScheduledActions")
        .into_iter()
        .map(|entry| {
            (
                xml_unescape(&tag_value(entry, "ScheduledActionName")),
                tag_value(entry, "ErrorCode"),
                xml_unescape(&tag_value(entry, "ErrorMessage")),
            )
        })
        .collect();
    (status, failed, body)
}

/// The names of the actions `group` still holds, as its own read reports them.
async fn held(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must succeed, got {status} with {body:?}"
    );
    tag_values(&body, "ScheduledActionName")
        .into_iter()
        .map(|name| xml_unescape(&name))
        .collect()
}

/// The names this suite asks about, one per thing the rule generalises over: the name's shape,
/// its length, its case against a held one, and whether it collides with the group's own name.
/// `held` is the one action every group in the table is given, so a case can differ from it
/// only in case.
fn absent_names(held_action: &str, group: &str) -> Vec<(String, String)> {
    vec![
        (
            "a short name the group was never given".to_string(),
            "ghost".to_string(),
        ),
        (
            "a name differing from a held one only in case".to_string(),
            held_action.to_uppercase(),
        ),
        (
            "a name carrying spaces and punctuation".to_string(),
            "scale out (peak hours) — tier 1".to_string(),
        ),
        (
            "a name as long as the member allows".to_string(),
            "g".repeat(MAX_ACTION_NAME_LENGTH),
        ),
        (
            "a name as short as the member allows".to_string(),
            "x".to_string(),
        ),
        (
            "a name equal to the group's own".to_string(),
            group.to_string(),
        ),
    ]
}

/// RULE: a delete naming an action the group holds no record of is refused with the fixed
/// sentence — the same text whatever name it stated, and never echoing that name.
#[tokio::test]
async fn one_delete_states_the_absent_action_in_fixed_text() {
    let group = "asg-absent-action-one";
    let held_action = "nightly";
    let router = given_group(group).await;
    given_action(&router, group, held_action).await;

    for (case, name) in absent_names(held_action, group) {
        let (status, code, message) = delete_one(&router, group, &name).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, VALIDATION_ERROR, ABSENT_ACTION_MESSAGE),
            "case {case:?} ({name:?}): expected the refusal AWS states, got {status} {code:?} \
             {message:?}"
        );
        assert!(
            !message.contains(&name),
            "case {case:?}: the name asked for must not appear in {message:?}"
        );
        assert_eq!(
            held(&router, &case, group).await,
            vec![held_action.to_string()],
            "case {case:?}: a refused delete must delete nothing"
        );
    }
}

/// RULE, continued: the same fixed sentence is what the batch form reports such a name under,
/// per entry, beside the name itself — so one entry says nothing the other members do not, and
/// several absent names in one call are reported with one identical message.
#[tokio::test]
async fn the_batch_form_reports_the_same_fixed_text_per_entry() {
    let group = "asg-absent-action-batch";
    let held_action = "nightly";
    let router = given_group(group).await;
    given_action(&router, group, held_action).await;

    for (case, name) in absent_names(held_action, group) {
        let (status, failed, body) = delete_batch(&router, group, &[&name]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-entry failure is an answer, not a refusal, got {status} with \
             {body}"
        );
        assert_eq!(
            failed,
            vec![(
                name.clone(),
                VALIDATION_ERROR.to_string(),
                ABSENT_ACTION_MESSAGE.to_string(),
            )],
            "case {case:?}: expected one entry naming {name:?} and the fixed sentence, got \
             {failed:?} in {body}"
        );
        assert!(
            !failed[0].2.contains(&name),
            "case {case:?}: the entry's message must leave the name to its own member, got {:?}",
            failed[0].2
        );
    }

    // SEVERAL absent names in one call, stated beside a name the group does hold: each name is
    // reported on its own and all of them under the one sentence, while the held name is
    // deleted.
    let absent: Vec<String> = absent_names(held_action, group)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let mut requested: Vec<&str> = vec![held_action];
    requested.extend(absent.iter().map(String::as_str));
    let (status, failed, body) = delete_batch(&router, group, &requested).await;
    assert_eq!(
        status, 200,
        "the mixed call must be served, got {status} with {body}"
    );
    let expected: Vec<(String, String, String)> = absent
        .iter()
        .map(|name| {
            (
                name.clone(),
                VALIDATION_ERROR.to_string(),
                ABSENT_ACTION_MESSAGE.to_string(),
            )
        })
        .collect();
    assert_eq!(
        failed, expected,
        "expected one entry per absent name, each under the one sentence, got {failed:?} in \
         {body}"
    );
    assert!(
        held(&router, "after the mixed call", group).await.is_empty(),
        "expected the held name to be deleted beside the absent ones, got {:?}",
        held(&router, "after the mixed call", group).await
    );
}

/// RULE, continued: a name is absent as soon as the action it named is gone, so the SECOND
/// delete of an action a first delete took out reads exactly like a delete of a name the group
/// was never given — through either operation.
#[tokio::test]
async fn a_name_just_deleted_reads_as_absent_through_either_operation() {
    for via in [Via::One, Via::Batch] {
        let group = format!("asg-redelete-{via:?}");
        let action = "weekend";
        let router = given_group(&group).await;
        given_action(&router, &group, action).await;
        assert_eq!(
            held(&router, &format!("{via:?}: before the delete"), &group).await,
            vec![action.to_string()],
            "case {via:?}: the action must be held before the delete"
        );

        // The positive control: an action the group DOES hold deletes cleanly and stops being
        // reported, so what the second delete answers is about the action being gone.
        match via {
            Via::One => {
                let (status, code, message) = delete_one(&router, &group, action).await;
                assert_eq!(
                    status, 200,
                    "case {via:?}: deleting a held action must be served, got {status} {code:?} \
                     {message:?}"
                );
            }
            Via::Batch => {
                let (status, failed, body) = delete_batch(&router, &group, &[action]).await;
                assert_eq!(
                    (status, failed.as_slice()),
                    (200, [].as_slice()),
                    "case {via:?}: deleting a held action must report no failure, got {status} \
                     {failed:?} in {body}"
                );
            }
        }
        assert!(
            held(&router, &format!("{via:?}: after the delete"), &group)
                .await
                .is_empty(),
            "case {via:?}: the deleted action must stop being reported"
        );

        match via {
            Via::One => {
                let (status, code, message) = delete_one(&router, &group, action).await;
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (400, VALIDATION_ERROR, ABSENT_ACTION_MESSAGE),
                    "case {via:?}: re-deleting a gone action must read as absent, got {status} \
                     {code:?} {message:?}"
                );
            }
            Via::Batch => {
                let (status, failed, body) = delete_batch(&router, &group, &[action]).await;
                assert_eq!(
                    status, 200,
                    "case {via:?}: the second call must be served, got {status} with {body}"
                );
                assert_eq!(
                    failed,
                    vec![(
                        action.to_string(),
                        VALIDATION_ERROR.to_string(),
                        ABSENT_ACTION_MESSAGE.to_string(),
                    )],
                    "case {via:?}: expected the gone action reported under the fixed sentence, \
                     got {failed:?} in {body}"
                );
            }
        }
    }
}
