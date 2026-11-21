// SPDX-License-Identifier: CC0-1.0

//! The per-group scheduled-action quota, and the ONE sentence both writers of a scheduled
//! action refuse a group already at it with.
//!
//! The rule under test: a group holds at most the published "Scheduled actions per Auto
//! Scaling group" quota, and a call that would CREATE one more is refused under the wire code
//! `LimitExceeded` carrying Amazon EC2 Auto Scaling's own sentence for the cap — "The maximum
//! limit of <limit> scheduled actions has been reached." — whichever of the two writers the
//! caller reached it through: `PutScheduledUpdateGroupAction` answers it as the call's own
//! HTTP 400 error, and `BatchPutScheduledUpdateGroupAction` reports it for the entry it could
//! not store. The refusal changes nothing — the group stays at the quota — the ceiling bounds
//! CREATES, so the same request against a name the group already holds is served, and freeing
//! one slot lets the refused request succeed.
//!
//! The table spans the two writers, which is what the rule generalises over; the expected
//! sentence is written out here rather than taken from the crate, and every assertion names
//! its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, list_members, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The launch source every group here is created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where the group places the instances it would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The published quota on a group's scheduled actions — "Scheduled actions per Auto Scaling
/// group" — written out here rather than taken from the crate, so the assertion does not take
/// the handler's own word for it.
const MAX_SCHEDULED_ACTIONS_PER_GROUP: usize = 125;

/// The most actions ONE call may state: "One or more scheduled actions. The maximum number
/// allowed is 50." (`BatchPutScheduledUpdateGroupActionType$ScheduledUpdateGroupActions`).
const MAX_SCHEDULED_ACTIONS_PER_CALL: usize = 50;

/// The wire error code the quota is refused under: the `awsQueryError` code of the
/// `LimitExceededFault` both writers model.
const LIMIT_EXCEEDED: &str = "LimitExceeded";

/// The whole sentence a group already at the quota is refused with, with the limit in force
/// interpolated.
fn quota_message() -> String {
    format!(
        "The maximum limit of {MAX_SCHEDULED_ACTIONS_PER_GROUP} scheduled actions has been \
         reached."
    )
}

/// One scheduled action of a call: the members every action here states beyond its name.
fn definition(min_size: &str) -> Vec<(String, String)> {
    vec![
        p("StartTime", "2030-06-01T12:00:00Z"),
        p("MinSize", min_size),
    ]
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

/// Post one `PutScheduledUpdateGroupAction` and answer its status, wire code and message.
async fn put_action(
    router: &MultiServiceRouter,
    group: &str,
    action: &str,
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("ScheduledActionName", action),
    ];
    params.extend(definition("1"));
    let (status, body) = query(
        router,
        "PutScheduledUpdateGroupAction",
        &borrowed(&params),
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one `BatchPutScheduledUpdateGroupAction` stating `actions`, and answer its status,
/// the entries it reported as failed, and its body.
async fn batch_put(
    router: &MultiServiceRouter,
    group: &str,
    actions: &[String],
) -> (u16, Vec<(String, String, String)>, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, action) in actions.iter().enumerate() {
        let prefix = format!("ScheduledUpdateGroupActions.member.{}", index + 1);
        params.push(p(&format!("{prefix}.ScheduledActionName"), action.clone()));
        for (member, value) in definition("1") {
            params.push(p(&format!("{prefix}.{member}"), value));
        }
    }
    let (status, body) = query(
        router,
        "BatchPutScheduledUpdateGroupAction",
        &borrowed(&params),
    )
    .await;
    let failed = list_members(&body, "FailedScheduledUpdateGroupActions")
        .into_iter()
        .map(|block| {
            (
                tag_value(block, "ScheduledActionName"),
                tag_value(block, "ErrorCode"),
                xml_unescape(&tag_value(block, "ErrorMessage")),
            )
        })
        .collect();
    (status, failed, body)
}

/// How many scheduled actions the group holds, walking every page the read links.
async fn held_count(router: &MultiServiceRouter, case: &str, group: &str) -> usize {
    let mut held = 0;
    let mut token = String::new();
    // Bounded: the quota is the most actions a group can hold, so at most that many pages of
    // one can exist.
    for _ in 0..=MAX_SCHEDULED_ACTIONS_PER_GROUP {
        let mut selection = vec![p("AutoScalingGroupName", group), p("MaxRecords", "100")];
        if !token.is_empty() {
            selection.push(p("NextToken", token.clone()));
        }
        let (status, body) = query(router, "DescribeScheduledActions", &borrowed(&selection)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must succeed, got {status} with {body:?}"
        );
        held += list_members(&body, "ScheduledUpdateGroupActions").len();
        token = tag_value(&body, "NextToken");
        if token.is_empty() {
            break;
        }
    }
    held
}

/// Fill `group` to the quota, in calls of at most the per-call maximum.
async fn fill_to_the_quota(router: &MultiServiceRouter, group: &str) {
    let names: Vec<String> = (0..MAX_SCHEDULED_ACTIONS_PER_GROUP)
        .map(|index| format!("held-{index:03}"))
        .collect();
    for batch in names.chunks(MAX_SCHEDULED_ACTIONS_PER_CALL) {
        let (status, failed, body) = batch_put(router, group, batch).await;
        assert_eq!(
            (status, failed.as_slice()),
            (200, [].as_slice()),
            "filling the quota must store every action, got {status} with {body:?}"
        );
    }
    let held = held_count(router, "filling the quota", group).await;
    assert_eq!(
        held, MAX_SCHEDULED_ACTIONS_PER_GROUP,
        "expected the group to hold exactly the quota, got {held}"
    );
}

/// Both writers refuse a group at the quota with the one sentence for it, change nothing, and
/// serve the same request once a slot is free.
#[tokio::test]
async fn the_scheduled_action_quota_is_refused_with_one_sentence() {
    let group = "quota-group";
    let router = given_group(group).await;
    fill_to_the_quota(&router, group).await;

    // ROW 1 — the single put: the quota is the call's own modelled failure.
    let (status, code, message) = put_action(&router, group, "one-too-many").await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, LIMIT_EXCEEDED, quota_message().as_str()),
        "case \"single put\": expected the quota sentence, got {status} {code:?} {message:?}"
    );

    // ROW 2 — the batch: the same quota, reported for the entry it could not store.
    let (status, failed, body) = batch_put(&router, group, &["batched-too-many".to_string()]).await;
    assert_eq!(
        (status, failed.as_slice()),
        (
            200,
            [(
                "batched-too-many".to_string(),
                LIMIT_EXCEEDED.to_string(),
                quota_message()
            )]
            .as_slice()
        ),
        "case \"batch put\": expected the quota reported for the entry, got {status} with \
         {body:?}"
    );

    // Neither refusal changed anything.
    let held = held_count(&router, "after both refusals", group).await;
    assert_eq!(
        held, MAX_SCHEDULED_ACTIONS_PER_GROUP,
        "expected a refused call to store nothing, got {held}"
    );

    // The ceiling bounds CREATES: the same request against a name the group already holds is
    // served at the quota, so what the refusal above is about is the new action and not the
    // request's own members.
    let (status, code, message) = put_action(&router, group, "held-000").await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (200, "", ""),
        "expected a put of a held name to be served at the quota, got {status} {code:?} \
         {message:?}"
    );
    let held = held_count(&router, "after the put of a held name", group).await;
    assert_eq!(
        held, MAX_SCHEDULED_ACTIONS_PER_GROUP,
        "expected the group to stay at the quota, got {held}"
    );

    // Freeing one slot lets the same request succeed, so what the refusal was about is the
    // quota and not the request.
    let (status, body) = query(
        &router,
        "BatchDeleteScheduledAction",
        &[
            ("AutoScalingGroupName", group),
            ("ScheduledActionNames.member.1", "held-000"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting one action must succeed, got {status} with {body:?}"
    );
    let (status, code, message) = put_action(&router, group, "one-too-many").await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (200, "", ""),
        "expected the freed slot to serve the same request, got {status} {code:?} {message:?}"
    );
    let held = held_count(&router, "after the freed slot was taken", group).await;
    assert_eq!(
        held, MAX_SCHEDULED_ACTIONS_PER_GROUP,
        "expected the group to be back at the quota, got {held}"
    );
}
