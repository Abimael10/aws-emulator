// SPDX-License-Identifier: CC0-1.0

//! A cache cluster's events reach the queue subscribed to its notification topic, driven
//! against the emulator a customer STARTS: the built binary, on its own port, with request
//! authentication on, reached over TLS with SigV4-signed calls.
//!
//! RULE: while a cache cluster's `NotificationConfiguration` names an Amazon SNS topic and
//! its `TopicStatus` is `active`, every cluster event ElastiCache records for that cluster is
//! ALSO published to that topic, so a queue subscribed to it receives one SNS notification
//! naming the `ElastiCache:<Event>` that happened and the cluster it happened to; a
//! configuration moved to `inactive` keeps its `TopicArn`, reports that status and is
//! published nothing while the events keep being recorded, and moving it back to `active`
//! resumes delivery onto the same topic with no ARN re-supplied.
//!
//! The sibling suite
//! [`crate::elasticache_cluster_event_reaches_its_subscribed_queue`] pins that rule on a
//! router assembled inside the test process, over a table of Regions, engines, node counts
//! and queue grants. This suite is the one that pins it on the emulator a client RUNS: the
//! credentials are verified, each call arrives over its own TLS connection, the publish
//! ElastiCache makes through the emulator's own callback is served by the binary's
//! multi-thread runtime, and one process serves every call — so a chain that holds in process
//! and not on the started emulator is a failure a test reports rather than one only a client
//! sees.
//!
//! ONE start is paid for here and drives the whole scenario: the network the cache is placed
//! in, the topic and its subscribed queue, the cluster, the reboot that raises an event, the
//! deactivation that withholds the next one, and the reactivation that resumes delivery.
//! Every wait is on a CONDITION, under a generous deadline, and the absence leg is read only
//! once the cluster's own event log shows the event that would have been published.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{blocks_of, element, elements, opt_element};

/// The API version an ElastiCache awsQuery client sends beside `Action`, and the one an
/// ec2Query client sends.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
/// See [`ELASTICACHE_API_VERSION`].
const EC2_API_VERSION: &str = "2016-11-15";

/// The service principal Amazon SNS delivers to a subscribed queue as, which is what that
/// queue's own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The first word of every published ElastiCache event name — the names are spelled
/// `ElastiCache:<Event>` — and the two completions this scenario raises. Spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const EVENT_PREFIX: &str = "ElastiCache:";
/// See [`EVENT_PREFIX`].
const PROVISIONING_COMPLETE: &str = "ElastiCache:CacheClusterProvisioningComplete";
/// See [`EVENT_PREFIX`].
const NODES_REBOOTED: &str = "ElastiCache:CacheNodesRebooted";

/// The two documented values of `NotificationTopicStatus`: "notifications are sent only if
/// the status is `active`. Valid values: active | inactive".
const ACTIVE: &str = "active";
/// See [`ACTIVE`].
const INACTIVE: &str = "inactive";

/// The cluster this scenario provisions, the engine it runs, the node type it runs on and the
/// port it serves — the node type out of the catalogue `ListAllowedNodeTypeModifications`
/// publishes.
const CLUSTER: &str = "emu-started-cache-events";
/// See [`CLUSTER`].
const ENGINE: &str = "redis";
/// See [`CLUSTER`].
const NODE_TYPE: &str = "cache.t3.micro";
/// See [`CLUSTER`].
const PORT: &str = "6379";

/// How long any one condition is waited for, and how often it is looked at. Generous next to
/// the spans this emulator's own transitions and in-process deliveries take: the convergence
/// SPEED is not what this suite asserts.
const DEADLINE: Duration = Duration::from_secs(90);
/// See [`DEADLINE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// One Amazon ElastiCache call over its awsQuery wire, which must answer 200.
fn elasticache(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, "elasticache", ELASTICACHE_API_VERSION, action, params)
}

/// One Amazon EC2 call over its ec2Query wire, which must answer 200.
fn ec2(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, "ec2", EC2_API_VERSION, action, params)
}

/// One Amazon SNS call over its awsQuery wire, which must answer 200.
fn sns(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    form_call(emulator, "sns", "2010-03-31", action, params)
}

/// One form-wire call that must answer 200, quoting the emulator's own log when it did not.
fn form_call(
    emulator: &Started,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = emulator.form_call(signing_name, version, action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    body
}

/// One Amazon SQS call, as the JSON it answered.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    let (status, answered) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    assert_eq!(
        status, 200,
        "case {operation:?}: the call must be served, got HTTP {status} with {answered}"
    );
    serde_json::from_str(&answered)
        .unwrap_or_else(|error| panic!("case {operation:?}: {answered} must be JSON: {error}"))
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// A topic of this emulator with a queue of this emulator subscribed to it, the queue
/// granting the SNS service principal the send AWS documents for a subscribed queue. Answers
/// the topic's ARN and the queue's url.
fn given_topic_with_subscriber(emulator: &Started, name: &str) -> (String, String) {
    let created = sqs(emulator, "CreateQueue", &json!({ "QueueName": name }));
    let url = member(name, &created, "/QueueUrl");
    let attributes = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(name, &attributes, "/Attributes/QueueArn");
    let topic = element(&sns(emulator, "CreateTopic", &[("Name", name)]), "TopicArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic } },
        }],
    });
    sqs(
        emulator,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy.to_string() } }),
    );
    sns(
        emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    );
    (topic, url)
}

/// `DescribeCacheClusters` for the one cluster, with its nodes.
fn describe(emulator: &Started) -> String {
    elasticache(
        emulator,
        "DescribeCacheClusters",
        &[("CacheClusterId", CLUSTER), ("ShowCacheNodeInfo", "true")],
    )
}

/// The cluster's notification configuration as `DescribeCacheClusters` projects it: its
/// `TopicArn` and its `TopicStatus`, each `None` when the answer carries no such member.
fn notification_configuration(body: &str) -> (Option<String>, Option<String>) {
    let Some(block) = blocks_of(body, "NotificationConfiguration")
        .into_iter()
        .next()
    else {
        return (None, None);
    };
    (
        opt_element(block, "TopicArn"),
        opt_element(block, "TopicStatus"),
    )
}

/// Wait until the cluster reports `available`, and answer that describe's body.
fn settled(emulator: &Started, case: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = describe(emulator);
        if opt_element(&body, "CacheClusterStatus").as_deref() == Some("available") {
            return body;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {CLUSTER} never settled at available inside {DEADLINE:?}; the last \
             describe answered {body}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Wait until the cluster's notification configuration reports `status`, and answer the
/// `TopicArn` reported beside it.
fn notification_status_reaches(emulator: &Started, case: &str, status: &str) -> Option<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = describe(emulator);
        let (arn, reported) = notification_configuration(&body);
        if reported.as_deref() == Some(status) {
            return arn;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {CLUSTER} never reported TopicStatus {status:?} inside {DEADLINE:?}; \
             the last describe answered {body}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Every message waiting on the queue at `url`, as the SNS envelope the queue carries it in,
/// deleting what it hands over so a later leg reads only what arrived since.
fn drain(emulator: &Started, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let raw = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or_else(|error| {
            panic!("the body of a delivered message is not the SNS envelope ({error}): {raw}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    envelopes
}

/// Wait until a notification naming `event` has arrived on the queue at `url`, and answer the
/// envelope it arrived in.
fn await_notification(emulator: &Started, case: &str, url: &str, event: &str) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(emulator, url));
        if let Some(envelope) = arrived
            .iter()
            .find(|envelope| names_event(envelope, event))
            .cloned()
        {
            return envelope;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: no notification naming {event} reached the subscribed queue inside \
             {DEADLINE:?}; what arrived was {arrived:?}; the emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Whether `envelope`'s `Message` is a cluster notification naming `event`.
fn names_event(envelope: &Value, event: &str) -> bool {
    document_of(envelope).is_some_and(|document| document.get(event).is_some())
}

/// The notification document `envelope` carries, or `None` when it carries no JSON one.
fn document_of(envelope: &Value) -> Option<Value> {
    let message = envelope["Message"].as_str()?;
    serde_json::from_str(message).ok()
}

/// Assert `envelope` is the SNS notification of `event` happening to this cluster, delivered
/// on `topic`.
fn assert_notification_of(case: &str, envelope: &Value, topic: &str, event: &str) {
    assert_eq!(
        envelope["Type"].as_str(),
        Some("Notification"),
        "case {case:?}: the delivery must be an SNS notification, got {envelope}"
    );
    assert_eq!(
        envelope["TopicArn"].as_str(),
        Some(topic),
        "case {case:?}: the notification must name the configured topic, got {envelope}"
    );
    let document = document_of(envelope)
        .unwrap_or_else(|| panic!("case {case:?}: the message must be a JSON document, got {envelope}"));
    assert!(
        event.starts_with(EVENT_PREFIX),
        "case {case:?}: a published event name is spelled {EVENT_PREFIX}<Event>, got {event}"
    );
    assert_eq!(
        document.get(event).and_then(Value::as_str),
        Some(CLUSTER),
        "case {case:?}: the notification must name the cluster the event happened to, got \
         {document}"
    );
}

/// The messages `DescribeEvents` reports for this cache cluster.
fn recorded_events(emulator: &Started) -> Vec<String> {
    let body = elasticache(
        emulator,
        "DescribeEvents",
        &[
            ("SourceIdentifier", CLUSTER),
            ("SourceType", "cache-cluster"),
        ],
    );
    elements(&body, "Message")
}

/// Wait until `DescribeEvents` reports at least `wanted` events for the cluster, and answer
/// them.
fn events_reach(emulator: &Started, case: &str, wanted: usize) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let recorded = recorded_events(emulator);
        if recorded.len() >= wanted {
            return recorded;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {CLUSTER} never recorded {wanted} events inside {DEADLINE:?}; it \
             reports {recorded:?}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// `RebootCacheCluster` naming every node the cluster reports.
fn reboot(emulator: &Started, nodes: &[String]) {
    let mut params: Vec<(String, String)> =
        vec![("CacheClusterId".to_string(), CLUSTER.to_string())];
    for (index, node) in nodes.iter().enumerate() {
        params.push((
            format!("CacheNodeIdsToReboot.CacheNodeId.{}", index + 1),
            node.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(emulator, "RebootCacheCluster", &borrowed);
}

/// Assert the queue holds nothing: both depth attributes report zero, and a read hands over
/// no message at all.
fn assert_holds_nothing(emulator: &Started, case: &str, url: &str) {
    let reported = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    );
    for attribute in [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ] {
        assert_eq!(
            reported["Attributes"][attribute],
            json!("0"),
            "case {case:?}: expected {attribute} \"0\", got {reported}"
        );
    }
    let held = drain(emulator, url);
    assert!(
        held.is_empty(),
        "case {case:?}: expected no message on the queue, got {held:?}"
    );
}

/// RULE: on the emulator a client starts, a cluster event reaches the queue subscribed to the
/// cluster's notification topic while that configuration is active, nothing reaches it while
/// the configuration is inactive, and delivery resumes on the same topic when it is activated
/// again.
#[test]
fn a_cluster_event_reaches_its_subscribed_queue_on_a_started_emulator() {
    let emulator = Started::start();

    // ---- the topic and the queue subscribed to it ----
    let (topic, url) = given_topic_with_subscriber(&emulator, "cache-cluster-events");

    // ---- the network the cache is placed in: one subnet per Availability Zone EC2 reports,
    //      which is the placement a client builds for a cache subnet group ----
    let vpc = element(
        &ec2(&emulator, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")]),
        "vpcId",
    );
    let zones = elements(
        &ec2(&emulator, "DescribeAvailabilityZones", &[]),
        "zoneName",
    );
    assert!(
        !zones.is_empty(),
        "the Region must publish at least one Availability Zone to place a cache in"
    );
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let created = ec2(
            &emulator,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", &format!("10.91.{index}.0/24")),
                ("AvailabilityZone", zone),
            ],
        );
        subnets.push(element(&created, "subnetId"));
    }
    let security_group = element(
        &ec2(
            &emulator,
            "CreateSecurityGroup",
            &[
                ("GroupName", "emu-started-cache-events"),
                ("GroupDescription", "the group guarding the cache of this suite"),
                ("VpcId", &vpc),
            ],
        ),
        "groupId",
    );
    let group = "emu-started-cache-subnets";
    let mut group_params: Vec<(String, String)> = vec![
        ("CacheSubnetGroupName".to_string(), group.to_string()),
        (
            "CacheSubnetGroupDescription".to_string(),
            "the placement of this scenario's cache".to_string(),
        ),
    ];
    for (index, subnet) in subnets.iter().enumerate() {
        group_params.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.clone(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = group_params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(&emulator, "CreateCacheSubnetGroup", &borrowed);

    // ---- the cluster, wired to the topic at creation ----
    elasticache(
        &emulator,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", CLUSTER),
            ("Engine", ENGINE),
            ("CacheNodeType", NODE_TYPE),
            ("NumCacheNodes", "1"),
            ("Port", PORT),
            ("CacheSubnetGroupName", group),
            ("SecurityGroupIds.SecurityGroupId.1", &security_group),
            ("NotificationTopicArn", &topic),
        ],
    );
    let body = settled(&emulator, "the created cluster");
    let (reported_arn, reported_status) = notification_configuration(&body);
    assert_eq!(
        reported_arn.as_deref(),
        Some(topic.as_str()),
        "the cluster must report the topic it was created with, got {body}"
    );
    assert_eq!(
        reported_status.as_deref(),
        Some(ACTIVE),
        "a configured topic starts active, got {body}"
    );

    // The provisioning's completion is the first event the topic is owed.
    let envelope = await_notification(&emulator, "the provisioned cluster", &url, PROVISIONING_COMPLETE);
    assert_notification_of(
        "the provisioned cluster",
        &envelope,
        &topic,
        PROVISIONING_COMPLETE,
    );

    // ---- the reboot, whose completion is recorded AND published ----
    let nodes = elements(&body, "CacheNodeId");
    assert_eq!(
        nodes.len(),
        1,
        "the cluster must report one id per node, got {nodes:?}"
    );
    reboot(&emulator, &nodes);
    let envelope = await_notification(&emulator, "the rebooted cluster", &url, NODES_REBOOTED);
    assert_notification_of("the rebooted cluster", &envelope, &topic, NODES_REBOOTED);
    settled(&emulator, "the rebooted cluster");
    let recorded = events_reach(&emulator, "the rebooted cluster", 2);
    assert!(
        recorded.iter().any(|message| message.contains("reboot")),
        "the reboot must also be in the cluster's own event log, got {recorded:?}"
    );

    // ---- deactivated: the binding is kept, the status reads back, nothing is delivered ----
    elasticache(
        &emulator,
        "ModifyCacheCluster",
        &[
            ("CacheClusterId", CLUSTER),
            ("ApplyImmediately", "true"),
            ("NotificationTopicStatus", INACTIVE),
        ],
    );
    let held = notification_status_reaches(&emulator, "the deactivated configuration", INACTIVE);
    assert_eq!(
        held.as_deref(),
        Some(topic.as_str()),
        "deactivating must keep the same topic bound, got {held:?}"
    );
    settled(&emulator, "the deactivated cluster");
    drain(&emulator, &url);
    reboot(&emulator, &nodes);
    // The absence is read once the cluster's OWN event log shows the reboot that followed, so
    // it is read after the publish point was reached rather than after a fixed wait.
    events_reach(&emulator, "the withheld reboot", recorded.len() + 2);
    assert_holds_nothing(&emulator, "an inactive notification configuration", &url);

    // ---- reactivated, with no ARN re-supplied: the same topic is published to again ----
    settled(&emulator, "the withheld reboot");
    elasticache(
        &emulator,
        "ModifyCacheCluster",
        &[
            ("CacheClusterId", CLUSTER),
            ("ApplyImmediately", "true"),
            ("NotificationTopicStatus", ACTIVE),
        ],
    );
    let restored = notification_status_reaches(&emulator, "the reactivated configuration", ACTIVE);
    assert_eq!(
        restored.as_deref(),
        Some(topic.as_str()),
        "reactivating must keep the same topic bound, got {restored:?}"
    );
    settled(&emulator, "the reactivated cluster");
    reboot(&emulator, &nodes);
    let envelope = await_notification(&emulator, "the resumed delivery", &url, NODES_REBOOTED);
    assert_notification_of("the resumed delivery", &envelope, &topic, NODES_REBOOTED);
}
