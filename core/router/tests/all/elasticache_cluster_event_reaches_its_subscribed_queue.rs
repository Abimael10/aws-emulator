// SPDX-License-Identifier: CC0-1.0

//! A cache cluster's events reach the queue subscribed to the topic its notification
//! configuration names — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **while a cache cluster's `NotificationConfiguration` names a topic
//! and its `TopicStatus` is `active`, every cluster event ElastiCache records for that
//! cluster is also published to that topic, so a queue subscribed to it (and granting the
//! SNS service principal the send) receives one SNS notification per event naming the
//! `ElastiCache:<Event>` that happened and the cluster it happened to; a configuration moved
//! to `inactive` KEEPS its `TopicArn`, reports that status, and is published nothing while
//! the events keep being recorded, and moving it back to `active` resumes delivery onto the
//! same topic with no ARN re-supplied.**
//!
//! Five services have to agree for that to hold — ElastiCache publishes, Amazon EC2 holds
//! the VPC the cluster is placed in, SNS fans out, SQS holds what arrived — and the publish
//! ElastiCache makes is an in-process call addressed the way a client's own `sns`-scoped call
//! is addressed, so WHICH mounted model claims it is only decidable with every service
//! mounted. The crate-local suite
//! `services/service-com-amazonaws-elasticache-amazonelasticachev9/tests/all/cluster_event_notifications.rs`
//! pins the publish and its status gate over tables of events and statuses; what only this
//! suite can reach is the same path over the production service set and out through a real
//! subscribed queue.
//!
//! The rows differ along what the rule generalises over: which operation makes the event
//! happen (the completion of a provisioning, a reboot of every node, a reboot of one node of
//! several), the engine the cluster runs and how many nodes it has, how the subscribed queue
//! scopes its grant to SNS, and the Region the whole wiring lives in.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::autoscaling_notification_reaches_its_subscribed_queue::{form_call, sqs};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{blocks_of, element, elements, opt_element};

/// The API version an ElastiCache awsQuery client sends beside `Action`.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The cache node type every cluster below runs on, out of the catalogue
/// `ListAllowedNodeTypeModifications` publishes.
const NODE_TYPE: &str = "cache.t3.micro";

/// The published event name the completion of a cluster's provisioning raises, and the one
/// the completion of a reboot raises. Spelled out here rather than imported from the service
/// crate, so a wrong constant in the implementation cannot make this suite pass.
const PROVISIONING_COMPLETE: &str = "ElastiCache:CacheClusterProvisioningComplete";
/// See [`PROVISIONING_COMPLETE`].
const NODES_REBOOTED: &str = "ElastiCache:CacheNodesRebooted";

/// The two documented values of `NotificationTopicStatus`: "notifications are sent only if
/// the status is `active`. Valid values: active | inactive".
const ACTIVE: &str = "active";
/// See [`ACTIVE`].
const INACTIVE: &str = "inactive";

/// How long this suite waits for something the CLOCK owes — a cluster settling, a
/// notification arriving — and how often it looks again while it waits. Generous on purpose:
/// every wait is for the CONDITION, so what is asserted does not depend on how fast or how
/// loaded the host is.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// One ElastiCache call that must answer 200, as its awsQuery XML body.
///
/// `pub(crate)` from here down to [`given_subnet_group`], and again on the readers below,
/// because the sibling suite
/// [`crate::elasticache_cluster_event_notification_raises_the_queues_depth`] drives the same
/// wiring over the same four services: a second copy of this plumbing would drift from this
/// one.
pub(crate) async fn elasticache(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    form_call(
        router,
        case,
        region,
        "elasticache",
        ELASTICACHE_API_VERSION,
        action,
        members,
    )
    .await
}

/// One Amazon EC2 call that must answer 200.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    form_call(router, case, region, "ec2", EC2_API_VERSION, action, members).await
}

/// The network a cluster of this suite is placed in: one VPC, one subnet per zone, one
/// security group — all minted through Amazon EC2's own operations, so every identifier
/// handed to ElastiCache is one the owner of subnets handed out.
pub(crate) struct Network {
    pub(crate) subnets: Vec<String>,
    pub(crate) security_group: String,
}

/// Build that network in `region`, spanning `zones`.
pub(crate) async fn given_network(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    label: &str,
    zones: &[&str],
) -> Network {
    let vpc = element(
        &ec2(router, case, region, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.90.{}.0/24", index + 1);
        let body = ec2(
            router,
            case,
            region,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr.as_str()),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&body, "subnetId"));
    }
    let security_group = element(
        &ec2(
            router,
            case,
            region,
            "CreateSecurityGroup",
            &[
                ("GroupName", label),
                ("GroupDescription", "the group guarding a cache of this suite"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnets,
        security_group,
    }
}

/// `CreateCacheSubnetGroup` over `network`'s subnets.
pub(crate) async fn given_subnet_group(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    network: &Network,
) {
    let mut members: Vec<(String, String)> = vec![
        ("CacheSubnetGroupName".to_string(), name.to_string()),
        (
            "CacheSubnetGroupDescription".to_string(),
            format!("the placement of {name}"),
        ),
    ];
    for (index, subnet) in network.subnets.iter().enumerate() {
        members.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(router, case, region, "CreateCacheSubnetGroup", &borrowed).await;
}

/// `DescribeCacheClusters` for one cluster, with its nodes.
async fn describe(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
) -> String {
    elasticache(
        router,
        case,
        region,
        "DescribeCacheClusters",
        &[("CacheClusterId", cluster), ("ShowCacheNodeInfo", "true")],
    )
    .await
}

/// The cluster's notification configuration as `DescribeCacheClusters` projects it: the ARN
/// and the status, each `None` when the answer carries no such member.
fn notification_configuration(body: &str) -> (Option<String>, Option<String>) {
    let Some(block) = blocks_of(body, "NotificationConfiguration").into_iter().next() else {
        return (None, None);
    };
    (
        opt_element(block, "TopicArn"),
        opt_element(block, "TopicStatus"),
    )
}

/// Wait until the cluster `cluster` reports `available`, and answer that describe's body.
pub(crate) async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, case, region, cluster).await;
        if opt_element(&body, "CacheClusterStatus").as_deref() == Some("available") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {cluster} never settled at available; the last describe answered \
             {body}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Wait until the cluster's notification configuration reports `status`, and answer the
/// `TopicArn` it reports beside it.
pub(crate) async fn notification_status_reaches(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
    status: &str,
) -> Option<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, case, region, cluster).await;
        let (arn, reported) = notification_configuration(&body);
        if reported.as_deref() == Some(status) {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {cluster} never reported TopicStatus {status:?}; the last describe \
             answered {body}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Every message waiting on the queue at `url`, as the SNS envelope the queue carries it in,
/// deleting what it hands over so a later leg reads only what arrived since.
pub(crate) async fn drain(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        region,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                region,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    envelopes
}

/// Drain until one notification naming `event` has arrived, or the deadline passes; answers
/// every envelope that arrived while waiting.
async fn drain_until(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
    event: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, region, url).await);
        if arrived.iter().any(|envelope| names_event(envelope, event)) {
            return arrived;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no notification naming {event} reached the subscribed queue; what \
             arrived was {arrived:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Whether `envelope`'s `Message` is a cluster notification naming `event`.
pub(crate) fn names_event(envelope: &Value, event: &str) -> bool {
    envelope["Message"]
        .as_str()
        .and_then(|message| serde_json::from_str::<Value>(message).ok())
        .is_some_and(|document| document.get(event).is_some())
}

/// The cluster the notification `envelope` names for `event`.
pub(crate) fn cluster_named(envelope: &Value, event: &str) -> Option<String> {
    let message = envelope["Message"].as_str()?;
    let document: Value = serde_json::from_str(message).ok()?;
    document
        .get(event)?
        .as_str()
        .map(::std::string::ToString::to_string)
}

/// The messages `DescribeEvents` reports for one cache cluster.
async fn recorded_events(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
) -> Vec<String> {
    let body = elasticache(
        router,
        case,
        region,
        "DescribeEvents",
        &[
            ("SourceIdentifier", cluster),
            ("SourceType", "cache-cluster"),
        ],
    )
    .await;
    elements(&body, "Message")
}

/// Wait until `DescribeEvents` reports at least `wanted` events for the cluster, and answer
/// them.
pub(crate) async fn events_reach(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let recorded = recorded_events(router, case, region, cluster).await;
        if recorded.len() >= wanted {
            return recorded;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {cluster} never recorded {wanted} events; it reports {recorded:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// One row: the wiring the rule is driven over.
struct Row {
    case: &'static str,
    /// The Region the whole wiring lives in.
    region: &'static str,
    /// How the subscribed queue grants SNS the send.
    grant: Grant,
    /// The zones the cache subnet group spans, one subnet each.
    zones: &'static [&'static str],
    /// The engine the cluster runs.
    engine: &'static str,
    /// How many cache nodes the cluster holds.
    nodes: usize,
    /// How many of those nodes the reboot names: every one, or just the first.
    reboot_all: bool,
}

/// The rows span what the rule generalises over: the Region, the grant's scoping, the engine
/// and the number of nodes the cluster holds, and how many of them a reboot names.
///
/// A `redis` cluster holds exactly one cache node, so the many-node row runs the engine that
/// takes several.
const ROWS: &[Row] = &[
    Row {
        case: "the one node of a redis cluster rebooted, with the grant scoped to the topic",
        region: "us-west-2",
        grant: Grant::ScopedToTopic,
        zones: &["us-west-2a"],
        engine: "redis",
        nodes: 1,
        reboot_all: true,
    },
    Row {
        case: "one node of a three-node memcached cluster, with an unscoped grant elsewhere",
        region: "eu-west-1",
        grant: Grant::Unscoped,
        zones: &["eu-west-1a", "eu-west-1b"],
        engine: "memcached",
        nodes: 3,
        reboot_all: false,
    },
];

#[tokio::test]
async fn a_cluster_event_reaches_the_queue_subscribed_to_its_notification_topic() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let router = router();
        let (topic, url) =
            given_topic_with_subscriber(&router, case, region, "cache-cluster-events", row.grant)
                .await;

        let label = format!("cache-events-{index}");
        let network = given_network(&router, case, region, &label, row.zones).await;
        let group = format!("cache-events-group-{index}");
        given_subnet_group(&router, case, region, &group, &network).await;

        // The cluster, wired to the topic at creation: "the ARN of the SNS topic to which
        // notifications are sent".
        let cluster = format!("cache-events-cluster-{index}");
        let nodes = row.nodes.to_string();
        elasticache(
            &router,
            case,
            region,
            "CreateCacheCluster",
            &[
                ("CacheClusterId", cluster.as_str()),
                ("Engine", row.engine),
                ("CacheNodeType", NODE_TYPE),
                ("NumCacheNodes", nodes.as_str()),
                ("Port", "6379"),
                ("CacheSubnetGroupName", group.as_str()),
                ("SecurityGroupIds.SecurityGroupId.1", &network.security_group),
                ("NotificationTopicArn", topic.as_str()),
            ],
        )
        .await;

        // The configuration reads back as the topic that was named, active.
        let body = settled(&router, case, region, &cluster).await;
        let (reported_arn, reported_status) = notification_configuration(&body);
        assert_eq!(
            reported_arn.as_deref(),
            Some(topic.as_str()),
            "case {case:?}: the cluster must report the topic it was created with, got {body}"
        );
        assert_eq!(
            reported_status.as_deref(),
            Some(ACTIVE),
            "case {case:?}: a configured topic starts active, got {body}"
        );

        // The provisioning's completion is the first event the topic is owed, and it reaches
        // the queue subscribed to it inside the SNS envelope.
        let delivered = drain_until(&router, case, region, &url, PROVISIONING_COMPLETE).await;
        let envelope = delivered
            .iter()
            .find(|envelope| names_event(envelope, PROVISIONING_COMPLETE))
            .unwrap_or_else(|| panic!("case {case:?}: checked just above"));
        assert_eq!(
            envelope["Type"].as_str(),
            Some("Notification"),
            "case {case:?}: the delivery must be an SNS notification, got {envelope}"
        );
        assert_eq!(
            envelope["TopicArn"].as_str(),
            Some(topic.as_str()),
            "case {case:?}: the notification must name the configured topic, got {envelope}"
        );
        assert_eq!(
            cluster_named(envelope, PROVISIONING_COMPLETE).as_deref(),
            Some(cluster.as_str()),
            "case {case:?}: the notification must name the cluster it happened to, got \
             {envelope}"
        );

        // A reboot of the nodes the cluster reports: its completion is published too.
        let node_ids = elements(&body, "CacheNodeId");
        assert_eq!(
            node_ids.len(),
            row.nodes,
            "case {case:?}: the cluster must report one id per node, got {node_ids:?}"
        );
        let rebooted: Vec<String> = if row.reboot_all {
            node_ids.clone()
        } else {
            node_ids[..1].to_vec()
        };
        reboot(&router, case, region, &cluster, &rebooted).await;
        let delivered = drain_until(&router, case, region, &url, NODES_REBOOTED).await;
        let envelope = delivered
            .iter()
            .find(|envelope| names_event(envelope, NODES_REBOOTED))
            .unwrap_or_else(|| panic!("case {case:?}: checked just above"));
        assert_eq!(
            cluster_named(envelope, NODES_REBOOTED).as_deref(),
            Some(cluster.as_str()),
            "case {case:?}: the reboot's notification must name the cluster, got {envelope}"
        );
        assert_eq!(
            envelope["TopicArn"].as_str(),
            Some(topic.as_str()),
            "case {case:?}: the reboot's notification must go to the configured topic, got \
             {envelope}"
        );
        settled(&router, case, region, &cluster).await;
        let recorded = events_reach(&router, case, region, &cluster, 2).await;
        assert!(
            recorded.iter().any(|message| message.contains("reboot")),
            "case {case:?}: the reboot must also be in the cluster's own event log, got \
             {recorded:?}"
        );

        // Deactivated: the binding is KEPT, the status reads back inactive, and the events of
        // a second reboot are recorded and published to nothing.
        elasticache(
            &router,
            case,
            region,
            "ModifyCacheCluster",
            &[
                ("CacheClusterId", cluster.as_str()),
                ("ApplyImmediately", "true"),
                ("NotificationTopicStatus", INACTIVE),
            ],
        )
        .await;
        let held = notification_status_reaches(&router, case, region, &cluster, INACTIVE).await;
        assert_eq!(
            held.as_deref(),
            Some(topic.as_str()),
            "case {case:?}: deactivating must keep the same topic bound, got {held:?}"
        );
        settled(&router, case, region, &cluster).await;
        drain(&router, case, region, &url).await;
        reboot(&router, case, region, &cluster, &rebooted).await;
        // The completion is WAITED for on the cluster's own event log, so the absence below
        // is read after the publish point was reached rather than after a fixed time.
        let before = recorded.len();
        events_reach(&router, case, region, &cluster, before + 2).await;
        let withheld = drain(&router, case, region, &url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: an inactive configuration must be published nothing, got {withheld:?}"
        );

        // Reactivated, with no ARN re-supplied: the same topic is published to again.
        elasticache(
            &router,
            case,
            region,
            "ModifyCacheCluster",
            &[
                ("CacheClusterId", cluster.as_str()),
                ("ApplyImmediately", "true"),
                ("NotificationTopicStatus", ACTIVE),
            ],
        )
        .await;
        let restored = notification_status_reaches(&router, case, region, &cluster, ACTIVE).await;
        assert_eq!(
            restored.as_deref(),
            Some(topic.as_str()),
            "case {case:?}: reactivating must keep the same topic bound, got {restored:?}"
        );
        settled(&router, case, region, &cluster).await;
        reboot(&router, case, region, &cluster, &rebooted).await;
        let resumed = drain_until(&router, case, region, &url, NODES_REBOOTED).await;
        let envelope = resumed
            .iter()
            .find(|envelope| names_event(envelope, NODES_REBOOTED))
            .unwrap_or_else(|| panic!("case {case:?}: checked just above"));
        assert_eq!(
            cluster_named(envelope, NODES_REBOOTED).as_deref(),
            Some(cluster.as_str()),
            "case {case:?}: delivery must resume on the same topic, got {envelope}"
        );
    }
}

/// `RebootCacheCluster` naming `nodes` of `cluster`.
pub(crate) async fn reboot(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    cluster: &str,
    nodes: &[String],
) {
    let mut members: Vec<(String, String)> =
        vec![("CacheClusterId".to_string(), cluster.to_string())];
    for (index, node) in nodes.iter().enumerate() {
        members.push((
            format!("CacheNodeIdsToReboot.CacheNodeId.{}", index + 1),
            node.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(router, case, region, "RebootCacheCluster", &borrowed).await;
}
