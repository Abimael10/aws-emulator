// SPDX-License-Identifier: CC0-1.0

//! The per-group Amazon SNS topic quota on a notification configuration, and the ONE
//! sentence a group already at it is refused with.
//!
//! The RULE under test: an Auto Scaling group may be configured to notify at most the
//! documented number of distinct Amazon SNS topics — "If you exceed your maximum limit of
//! SNS topics, which is 10 per Auto Scaling group, the call fails"
//! (`smithy_query(query="operation", operation="PutNotificationConfiguration")`) — and a
//! `PutNotificationConfiguration` naming one more topic is refused HTTP 400 under the wire
//! code `LimitExceeded` (the `awsQueryError` code of the operation's modelled
//! `LimitExceededFault`) carrying Amazon EC2 Auto Scaling's own sentence for that quota:
//! `Your AutoScaling Group:<group> can't have more than <quota> topics`, whose only
//! variable part is the group the request named. The refusal writes nothing, the ceiling
//! bounds NEW topics only (re-configuring a topic the group already holds is served at the
//! quota), and freeing one slot lets the refused request through.
//!
//! The rows span what the rule generalises over: WHICH group hit the quota — the one piece
//! of the sentence that varies — so several groups at the quota in ONE emulator are each
//! refused naming themselves, with the group names differing in form (plain, punctuated,
//! and a long name), which is what a caller configuring groups in a loop reads to tell its
//! failures apart.

use smithy_server_runtime::MultiServiceRouter;

use crate::delete_notification_configuration::{
    configured, delete_configuration, topic_arn, TEST_NOTIFICATION,
};
use crate::lifecycle_hooks::create_group;
use crate::put_notification_configuration_event_types::put_configuration_result;
use crate::support::{new_engine, router_with};

/// The documented ceiling on the topics ONE group may be configured to notify, written out
/// here rather than taken from the crate so the assertion does not take the handler's own
/// word for it: "your maximum limit of SNS topics, which is 10 per Auto Scaling group"
/// (`PutNotificationConfiguration`).
const MAX_TOPICS_PER_GROUP: usize = 10;

/// The wire error code the quota is refused under: the `awsQueryError` code of the
/// `LimitExceededFault` `PutNotificationConfiguration` models.
const LIMIT_EXCEEDED: &str = "LimitExceeded";

/// The whole sentence a group already at the quota is refused with, spelled out here rather
/// than taken from the service's own builder: fixed text apart from the group name — no
/// space after the colon, no trailing period.
fn quota_message(group: &str) -> String {
    format!("Your AutoScaling Group:{group} can't have more than {MAX_TOPICS_PER_GROUP} topics")
}

/// The group names the rows run, differing in the FORM of the one variable part of the
/// sentence.
fn group_names() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name", "topic-quota-group".to_string()),
        (
            "a punctuated name",
            "Topic.Quota_Group-2024".to_string(),
        ),
        ("a long name", format!("quota-{}", "g".repeat(80))),
    ]
}

/// Configure `group` with exactly the quota of distinct topics, asserting every one of them
/// is served, and answer the topic ARNs stored.
async fn fill_to_the_quota(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<String> {
    let mut stored = Vec::new();
    for index in 0..MAX_TOPICS_PER_GROUP {
        let topic = topic_arn(&format!("quota-topic-{index}"));
        let (status, code, message) =
            put_configuration_result(router, group, &topic, &[TEST_NOTIFICATION.to_string()]).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (200, "", ""),
            "case {case:?}: topic {index} of {MAX_TOPICS_PER_GROUP} is inside the quota, got \
             {status} {code:?} {message:?}"
        );
        stored.push(topic);
    }
    let held = configured(router, case, group).await;
    assert_eq!(
        held.len(),
        MAX_TOPICS_PER_GROUP,
        "case {case:?}: the group must hold exactly the quota, got {held:?}"
    );
    stored
}

/// Every group at the quota is refused the next topic with the quota sentence NAMING THAT
/// GROUP, under the modelled limit code, and writes nothing.
#[tokio::test]
async fn the_topic_quota_refusal_names_the_group_that_hit_it() {
    // ONE emulator for every row, so the refusals a caller reads in a loop over several
    // groups are told apart by the sentence alone.
    let router = router_with(new_engine());
    let rows = group_names();
    for (case, group) in &rows {
        create_group(&router, group).await;
        fill_to_the_quota(&router, case, group).await;
    }

    for (case, group) in &rows {
        let one_too_many = topic_arn("quota-topic-one-too-many");
        let (status, code, message) = put_configuration_result(
            &router,
            group,
            &one_too_many,
            &[TEST_NOTIFICATION.to_string()],
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, LIMIT_EXCEEDED, quota_message(group).as_str()),
            "case {case:?}: expected the quota sentence naming {group:?}, got {status} {code:?} \
             {message:?}"
        );
        let held = configured(&router, case, group).await;
        assert_eq!(
            held.len(),
            MAX_TOPICS_PER_GROUP,
            "case {case:?}: the refused configuration stores nothing, got {held:?}"
        );
        assert!(
            !held.iter().any(|(topic, _)| topic == &one_too_many),
            "case {case:?}: the refused topic is not stored, got {held:?}"
        );
    }
}

/// The ceiling bounds NEW topics: a group at the quota still serves a re-configuration of a
/// topic it already holds, and freeing one slot serves the request that was refused.
#[tokio::test]
async fn the_topic_quota_bounds_new_topics_only() {
    let router = router_with(new_engine());
    let case = "the quota boundary";
    let group = "topic-quota-boundary-group";
    create_group(&router, group).await;
    let stored = fill_to_the_quota(&router, case, group).await;
    let held_topic = stored.first().expect("the quota is not zero").clone();
    let one_too_many = topic_arn("boundary-topic-one-too-many");

    // Re-configuring a held topic is no new topic, so it is served AT the quota — with
    // another event type, so the call is a real write rather than a repeat of the first.
    let instance_launch = ["autoscaling:EC2_INSTANCE_LAUNCH".to_string()];
    let (status, code, message) =
        put_configuration_result(&router, group, &held_topic, &instance_launch).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (200, "", ""),
        "case {case:?}: a re-configuration of a held topic is served at the quota, got {status} \
         {code:?} {message:?}"
    );

    // One more topic is refused with the quota sentence for this group.
    let (status, code, message) = put_configuration_result(
        &router,
        group,
        &one_too_many,
        &[TEST_NOTIFICATION.to_string()],
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, LIMIT_EXCEEDED, quota_message(group).as_str()),
        "case {case:?}: expected the quota sentence, got {status} {code:?} {message:?}"
    );

    // Freeing one slot serves the very same request, so the refusal was about the quota and
    // not about what the request carried.
    let (status, code, message) = delete_configuration(&router, group, &held_topic).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (200, "", ""),
        "case {case:?}: deleting one configuration must succeed, got {status} {code:?} {message:?}"
    );
    let (status, code, message) = put_configuration_result(
        &router,
        group,
        &one_too_many,
        &[TEST_NOTIFICATION.to_string()],
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (200, "", ""),
        "case {case:?}: the freed slot serves the same request, got {status} {code:?} {message:?}"
    );
    let held = configured(&router, case, group).await;
    assert_eq!(
        held.len(),
        MAX_TOPICS_PER_GROUP,
        "case {case:?}: the group is back at the quota, got {held:?}"
    );
    assert!(
        held.iter().any(|(topic, _)| topic == &one_too_many),
        "case {case:?}: the topic that took the freed slot reads back, got {held:?}"
    );
}
