// SPDX-License-Identifier: CC0-1.0

//! What a client WATCHING its queue sees when a cache cluster's events are published to the
//! cluster's notification topic: the queue's own depth.
//!
//! The RULE under test: **while a cache cluster's `NotificationConfiguration` names an Amazon
//! SNS topic and its `TopicStatus` is `active`, every cluster event ElastiCache records for
//! that cluster is also published to that topic, so the queue subscribed to it GAINS a
//! message — its `ApproximateNumberOfMessages` reaches at least one — without any further
//! ElastiCache call, and the message it gained is the SNS envelope of that event
//! (`Type` `Notification`, `TopicArn` the configured topic, `Message` the published
//! `ElastiCache:<Event>` name keyed to the cluster it happened to); a configuration moved to
//! `inactive` keeps its `TopicArn` and leaves the queue at zero while the events keep being
//! recorded, and moving it back to `active` makes the depth rise again on the same topic.**
//!
//! The sibling suite [`crate::elasticache_cluster_event_reaches_its_subscribed_queue`] reads
//! the deliveries by RECEIVING them. What only this suite pins is the depth a client polls
//! instead: a publish nothing ever made leaves `ApproximateNumberOfMessages` at zero for
//! ever, which is what a client watching the queue — rather than draining it — observes, and
//! the depth is read here BEFORE anything is received, so a delivery that only materialises
//! when somebody reads cannot satisfy it.
//!
//! The rows differ along what the rule generalises over: which operation makes the event
//! happen — the completion of the provisioning, a reboot of every node, a snapshot of the
//! cluster, the cluster's deletion — so the depth is asserted for a completion the CLOCK owes
//! and for one the call itself records.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::autoscaling_notification_reaches_its_subscribed_queue::sqs;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_topic_with_subscriber, router, Grant,
};
use crate::elasticache_cluster_event_reaches_its_subscribed_queue::{
    cluster_named, drain, elasticache, events_reach, given_network, given_subnet_group,
    names_event, notification_status_reaches, reboot, settled,
};
use crate::query_wire::elements;

/// The cache node type every cluster below runs on, out of the catalogue
/// `ListAllowedNodeTypeModifications` publishes, and the engine and port it serves.
const NODE_TYPE: &str = "cache.t3.micro";
/// See [`NODE_TYPE`].
const ENGINE: &str = "redis";
/// See [`NODE_TYPE`].
const PORT: &str = "6379";

/// The two depth attributes `GetQueueAttributes` reports for a queue: what is waiting to be
/// read, and what has been read and not yet deleted.
const WAITING: &str = "ApproximateNumberOfMessages";
/// See [`WAITING`].
const IN_FLIGHT: &str = "ApproximateNumberOfMessagesNotVisible";

/// The published event names the completions below raise. Spelled out here rather than
/// imported from the service crate, so a wrong constant in the implementation cannot make
/// this suite pass.
const PROVISIONING_COMPLETE: &str = "ElastiCache:CacheClusterProvisioningComplete";
/// See [`PROVISIONING_COMPLETE`].
const NODES_REBOOTED: &str = "ElastiCache:CacheNodesRebooted";
/// See [`PROVISIONING_COMPLETE`].
const SNAPSHOT_COMPLETE: &str = "ElastiCache:SnapshotComplete";
/// See [`PROVISIONING_COMPLETE`].
const DELETE_COMPLETE: &str = "ElastiCache:DeleteCacheClusterComplete";

/// The two documented values of `NotificationTopicStatus`: "notifications are sent only if
/// the status is `active`. Valid values: active | inactive".
const ACTIVE: &str = "active";
/// See [`ACTIVE`].
const INACTIVE: &str = "inactive";

/// How long any one condition here is waited for, and how often it is looked at again.
/// Generous next to the spans this emulator's own transitions and in-process deliveries take:
/// what is asserted is the CONDITION, never how fast the host reached it.
const DEADLINE: Duration = Duration::from_secs(90);
/// See [`DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// What `GetQueueAttributes` reports for `attribute` on the queue at `url`.
///
/// A READ of the queue's own counters: nothing is received, so the depth it answers is the
/// depth a client polling the queue sees.
async fn reported_depth(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
    attribute: &str,
) -> u64 {
    let reported = sqs(
        router,
        case,
        region,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": [attribute]}),
    )
    .await;
    let text = reported["Attributes"][attribute]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected {attribute} in {reported}"))
        .to_string();
    text.parse::<u64>().unwrap_or_else(|error| {
        panic!("case {case:?}: expected {attribute} as a count, got {text:?}: {error}")
    })
}

/// Wait until at least one message is waiting on the queue at `url`, reading nothing off it.
async fn depth_reaches_one(router: &MultiServiceRouter, case: &str, region: &str, url: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let waiting = reported_depth(router, case, region, url, WAITING).await;
        if waiting >= 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {WAITING} to reach at least 1 on the subscribed queue, \
             got {waiting}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Assert both depth attributes of the queue at `url` report nothing held.
async fn assert_depth_is_zero(router: &MultiServiceRouter, case: &str, region: &str, url: &str) {
    for attribute in [WAITING, IN_FLIGHT] {
        let held = reported_depth(router, case, region, url, attribute).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected {attribute} 0 on the subscribed queue, got {held}"
        );
    }
}

/// Take what the queue at `url` holds and assert one of the envelopes is the SNS notification
/// of `event` happening to `cluster`, delivered on `topic`.
async fn assert_notified_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
    topic: &str,
    cluster: &str,
    event: &str,
) {
    let arrived = drain(router, case, region, url).await;
    let envelope = arrived
        .iter()
        .find(|envelope| names_event(envelope, event))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a notification naming {event}, got {arrived:?}")
        });
    assert_eq!(
        envelope["Type"].as_str(),
        Some("Notification"),
        "case {case:?}: expected an SNS notification, got {envelope}"
    );
    assert_eq!(
        envelope["TopicArn"].as_str(),
        Some(topic),
        "case {case:?}: expected the notification on the configured topic, got {envelope}"
    );
    assert_eq!(
        cluster_named(envelope, event).as_deref(),
        Some(cluster),
        "case {case:?}: expected the notification to name the cluster it happened to, got \
         {envelope}"
    );
}

/// The network and the cache subnet group a cluster of this suite is placed in, built through
/// Amazon EC2's own operations, and then the cluster itself, wired to `topic` at creation.
async fn given_cluster_wired_to(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    label: &str,
    zones: &[&str],
    topic: &str,
) -> String {
    let network = given_network(router, case, region, label, zones).await;
    let group = format!("{label}-subnets");
    given_subnet_group(router, case, region, &group, &network).await;
    let cluster = format!("{label}-cluster");
    elasticache(
        router,
        case,
        region,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", cluster.as_str()),
            ("Engine", ENGINE),
            ("CacheNodeType", NODE_TYPE),
            ("NumCacheNodes", "1"),
            ("Port", PORT),
            ("CacheSubnetGroupName", group.as_str()),
            ("SecurityGroupIds.SecurityGroupId.1", &network.security_group),
            ("NotificationTopicArn", topic),
        ],
    )
    .await;
    cluster
}

/// Move the cluster's notification configuration to `status`, and assert the topic it was
/// bound to is still the one it reports.
async fn set_notification_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
    topic: &str,
    status: &str,
) {
    elasticache(
        router,
        case,
        region,
        "ModifyCacheCluster",
        &[
            ("CacheClusterId", cluster),
            ("ApplyImmediately", "true"),
            ("NotificationTopicStatus", status),
        ],
    )
    .await;
    let bound = notification_status_reaches(router, case, region, cluster, status).await;
    assert_eq!(
        bound.as_deref(),
        Some(topic),
        "case {case:?}: expected the same topic bound at TopicStatus {status:?}, got {bound:?}"
    );
}

/// What a row of the table does to the cluster before the queue's depth is read.
#[derive(Debug, Clone, Copy)]
enum Work {
    /// Nothing: the completion of the cluster's own provisioning is the event.
    None,
    /// Reboot every cache node the cluster reports.
    Reboot,
    /// Take a snapshot of the cluster.
    Snapshot,
    /// Delete the cluster.
    Delete,
}

/// One row: the work that makes an event happen, and the published event it owes the topic.
struct Row {
    case: &'static str,
    work: Work,
    expected: &'static str,
}

/// The rows span the operations a cluster event comes out of: a completion the CLOCK owes
/// (the provisioning, a reboot, a snapshot) and one the call itself records (the delete).
const ROWS: &[Row] = &[
    Row {
        case: "the provisioning completing",
        work: Work::None,
        expected: PROVISIONING_COMPLETE,
    },
    Row {
        case: "every cache node rebooted",
        work: Work::Reboot,
        expected: NODES_REBOOTED,
    },
    Row {
        case: "a snapshot of the cluster",
        work: Work::Snapshot,
        expected: SNAPSHOT_COMPLETE,
    },
    Row {
        case: "the cluster deleted",
        work: Work::Delete,
        expected: DELETE_COMPLETE,
    },
];

/// RULE: every cluster event of a cluster whose notification configuration is active raises
/// the depth of the queue subscribed to its topic, and the message the queue gained is that
/// event's SNS notification.
#[tokio::test]
async fn the_subscribed_queues_depth_rises_for_every_cluster_event() {
    let region = "us-east-1";
    let router = router();
    let (topic, url) = given_topic_with_subscriber(
        &router,
        "the queue subscribed to the cluster's topic",
        region,
        "cache-cluster-event-depth",
        Grant::ScopedToTopic,
    )
    .await;
    let cluster = given_cluster_wired_to(
        &router,
        "the cluster wired to the topic",
        region,
        "cache-depth",
        &["us-east-1a", "us-east-1b"],
        &topic,
    )
    .await;

    for row in ROWS {
        let case = row.case;
        // The work each row does is done on an available cluster, and the describe that waits
        // for it is the LAST ElastiCache call before the depth is read: what raises the depth
        // below is the event, not a call that reports on it.
        if !matches!(row.work, Work::None) {
            let body = settled(&router, case, region, &cluster).await;
            let nodes = elements(&body, "CacheNodeId");
            assert!(
                !nodes.is_empty(),
                "case {case:?}: expected the cluster to report its cache nodes, got {body}"
            );
            match row.work {
                Work::None => {}
                Work::Reboot => reboot(&router, case, region, &cluster, &nodes).await,
                Work::Snapshot => {
                    let snapshot = format!("{cluster}-snapshot");
                    elasticache(
                        &router,
                        case,
                        region,
                        "CreateSnapshot",
                        &[
                            ("CacheClusterId", cluster.as_str()),
                            ("SnapshotName", snapshot.as_str()),
                        ],
                    )
                    .await;
                }
                Work::Delete => {
                    elasticache(
                        &router,
                        case,
                        region,
                        "DeleteCacheCluster",
                        &[("CacheClusterId", cluster.as_str())],
                    )
                    .await;
                }
            }
        }

        // The depth is read first, with nothing received: a client watching its queue sees the
        // event arrive without reading, and without calling ElastiCache again.
        depth_reaches_one(&router, case, region, &url).await;
        assert_notified_of(
            &router, case, region, &url, &topic, &cluster, row.expected,
        )
        .await;
    }
}

/// RULE: a notification configuration moved to `inactive` leaves the subscribed queue's depth
/// at zero while the cluster's events keep being recorded, and moving it back to `active`
/// makes the depth rise again on the same topic with no ARN re-supplied.
#[tokio::test]
async fn an_inactive_configuration_leaves_the_queues_depth_at_zero() {
    let region = "eu-west-1";
    let case = "the deactivated notification configuration";
    let router = router();
    let (topic, url) = given_topic_with_subscriber(
        &router,
        case,
        region,
        "cache-cluster-event-depth-gate",
        Grant::Unscoped,
    )
    .await;
    let cluster = given_cluster_wired_to(
        &router,
        case,
        region,
        "cache-depth-gate",
        &["eu-west-1a"],
        &topic,
    )
    .await;

    // The provisioning's own notification is taken off the queue, so what is read below is
    // what arrived after the configuration was deactivated.
    depth_reaches_one(&router, case, region, &url).await;
    assert_notified_of(
        &router,
        case,
        region,
        &url,
        &topic,
        &cluster,
        PROVISIONING_COMPLETE,
    )
    .await;

    set_notification_status(&router, case, region, &cluster, &topic, INACTIVE).await;
    let body = settled(&router, case, region, &cluster).await;
    let nodes = elements(&body, "CacheNodeId");
    let recorded = events_reach(&router, case, region, &cluster, 1).await;
    reboot(&router, case, region, &cluster, &nodes).await;
    // The absence is read once the reboot's completion has been MATERIALISED and recorded —
    // the instant a publish would have been made — rather than after a fixed wait.
    settled(&router, case, region, &cluster).await;
    let after = events_reach(&router, case, region, &cluster, recorded.len() + 1).await;
    assert!(
        after.len() > recorded.len(),
        "case {case:?}: expected the reboot's completion in the cluster's own event log, got \
         {after:?}"
    );
    assert_depth_is_zero(&router, case, region, &url).await;

    // Reactivated, with no ARN re-supplied: the depth rises again on the same topic.
    let resumed = "the reactivated notification configuration";
    set_notification_status(&router, resumed, region, &cluster, &topic, ACTIVE).await;
    settled(&router, resumed, region, &cluster).await;
    reboot(&router, resumed, region, &cluster, &nodes).await;
    depth_reaches_one(&router, resumed, region, &url).await;
    assert_notified_of(
        &router,
        resumed,
        region,
        &url,
        &topic,
        &cluster,
        NODES_REBOOTED,
    )
    .await;
}
