// SPDX-License-Identifier: CC0-1.0

//! What a client that TEARS DOWN a cross-cluster replication reads, driven end to
//! end over the same `MultiServiceRouter` the emulator serves from (the shared
//! `kafka_harness`).
//!
//! RULE under test: the asynchronous states this sequence walks each reach their
//! TERMINAL value within ONE clock-owed span of the call that started it — the
//! clusters to `ACTIVE`, the replicator to `RUNNING`, a source topic onto the
//! target cluster's `ListTopics` — and a DELETE converges the same way: once its
//! own span is out, every later reader is answered the service's not-found refusal,
//! `NotFoundException` whose Message is the service's one fixed sentence, carrying
//! the member at fault where the operation's reference is a parameter of its own
//! (`Invalid parameter <member>: <sentence>`) and that sentence alone where the
//! resource is the call's subject. So the FIRST read a client makes after each span
//! reports the terminal answer, whatever cadence it polls at, and a client that
//! waits for absence to confirm a teardown converges instead of running to its own
//! deadline.
//!
//! The rows differ along what the rule generalises over: which resource the delete
//! removes (a topic of a cluster, the replicator over two of them) and therefore
//! which of the two refusal shapes marks it gone, and — for the propagation and the
//! topic teardown — the topic's own name and partition count, so the sequence is
//! asserted over a set of topics rather than for one recorded request.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, sleep_past_activation, ClusterRequest, Emulator, Reply};

/// A role ARN passed to the service, which stores it and reports it back.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-replicator";

/// Amazon MSK's one fixed sentence for a reference that resolves to no resource of
/// this endpoint. Written out here rather than imported from the service, so this
/// assertion cannot agree with a constant that is wrong — the apostrophe is the
/// TYPOGRAPHIC one (U+2019), not ASCII.
const NOT_FOUND_SENTENCE: &str = "The requested resource doesn\u{2019}t exist.";

/// Sleep past the span a replicator takes to leave `CREATING`, the service's own
/// published constant plus a margin, so the read after it is the FIRST read of a
/// settled replicator.
fn sleep_past_replicator_span() {
    std::thread::sleep(
        service_com_amazonaws_kafka_kafka::state::REPLICATOR_ACTIVATION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// Sleep past the span a topic takes to become `ACTIVE`.
fn sleep_past_topic_span() {
    std::thread::sleep(
        service_com_amazonaws_kafka_kafka::state::TOPIC_ACTIVATION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// Sleep past the span a replicated topic takes to appear on its target cluster.
fn sleep_past_replication_span() {
    std::thread::sleep(
        service_com_amazonaws_kafka_kafka::state::TOPIC_REPLICATION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// Sleep past the span a deleted topic takes to leave the cluster.
fn sleep_past_topic_deletion_span() {
    std::thread::sleep(
        service_com_amazonaws_kafka_kafka::state::TOPIC_DELETION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// A `CreateReplicator` body over one flow between `source` and `target`.
fn replicator_body(name: &str, source: &str, target: &str, topics: &[&str]) -> Value {
    json!({
        "replicatorName": name,
        "serviceExecutionRoleArn": ROLE_ARN,
        "kafkaClusters": [
            {
                "amazonMskCluster": { "mskClusterArn": source },
                "vpcConfig": { "subnetIds": ["subnet-a", "subnet-b", "subnet-c"] },
            },
            {
                "amazonMskCluster": { "mskClusterArn": target },
                "vpcConfig": { "subnetIds": ["subnet-a", "subnet-b", "subnet-c"] },
            },
        ],
        "replicationInfoList": [{
            "sourceKafkaClusterArn": source,
            "targetKafkaClusterArn": target,
            "targetCompressionType": "NONE",
            "topicReplication": { "topicsToReplicate": topics },
            "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
        }],
    })
}

/// `DescribeReplicator` for `arn`.
async fn describe_replicator(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/replication/v1/replicators/{}", pct(arn)),
            None,
        )
        .await
}

/// `DescribeTopic` for one topic of `cluster`.
async fn describe_topic(emulator: &Emulator, cluster: &str, topic: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(topic)),
            None,
        )
        .await
}

/// Every topic name `ListTopics` reports for `cluster`.
async fn listed_topic_names(emulator: &Emulator, cluster: &str) -> Vec<String> {
    let listed = emulator
        .call("GET", &format!("/v1/clusters/{}/topics", pct(cluster)), None)
        .await;
    assert_eq!(
        listed.status, 200,
        "expected ListTopics to be served, got {} {}",
        listed.status, listed.body
    );
    listed
        .at("/topics")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.get("topicName").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Assert `reply` is the not-found refusal that marks a resource gone: HTTP 404
/// `NotFoundException` whose whole message is `expected`.
fn assert_absent(reply: &Reply, case: &str, expected: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (404, "NotFoundException", expected),
        "case {case}: expected the not-found refusal {expected:?}, got {} {}",
        reply.status,
        reply.body
    );
}

/// RULE: the whole cross-cluster sequence settles step by step — both clusters
/// `ACTIVE`, the replicator `RUNNING`, each source topic visible on the target, and
/// then each delete converging to the refusal that marks it done — with every
/// terminal value observed on the FIRST read after that step's own span.
#[tokio::test]
async fn the_cross_cluster_topic_sync_and_teardown_converges_at_every_step() {
    let emulator = Emulator::new();
    let source = emulator.create_ok(&ClusterRequest::named("sync-source")).await;
    let target = emulator.create_ok(&ClusterRequest::named("sync-target")).await;

    // One span for both clusters: they were created independently, so neither waits
    // for the other.
    sleep_past_activation();
    for (case, cluster) in [("the source", &source), ("the target", &target)] {
        let described = emulator.describe(cluster).await;
        assert_eq!(
            described.at("/clusterInfo/state").as_str(),
            Some("ACTIVE"),
            "case {case}: the first read after the activation span must report ACTIVE, got {}",
            described.body
        );
    }

    let created = emulator
        .call(
            "POST",
            "/replication/v1/replicators",
            Some(replicator_body("sync", &source, &target, &[".*"])),
        )
        .await;
    assert_eq!(
        (created.status, created.at("/replicatorState").as_str()),
        (200, Some("CREATING")),
        "expected CreateReplicator to report CREATING, got {} {}",
        created.status,
        created.body
    );
    let replicator = created
        .at("/replicatorArn")
        .as_str()
        .expect("CreateReplicator returned no replicatorArn")
        .to_string();

    sleep_past_replicator_span();
    let described = describe_replicator(&emulator, &replicator).await;
    assert_eq!(
        described.at("/replicatorState").as_str(),
        Some("RUNNING"),
        "the first read after the replicator span must report RUNNING, got {}",
        described.body
    );

    // The rows: the topics the flow carries, differing in name (an ASCII one, a
    // dotted one, one whose name a prefixing flow must not mangle) and in partition
    // count, so the propagation and the teardown are asserted over a set.
    let rows: Vec<(&str, i64)> = vec![("orders", 1), ("orders.eu.west", 3), ("Orders-2024_v2", 2)];
    for (topic, partitions) in rows {
        let accepted = emulator
            .call(
                "POST",
                &format!("/v1/clusters/{}/topics", pct(&source)),
                Some(json!({
                    "topicName": topic,
                    "partitionCount": partitions,
                    "replicationFactor": 2,
                })),
            )
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {topic:?}: expected CreateTopic to be accepted, got {} {}",
            accepted.status, accepted.body
        );

        // One span for the topic itself, then one for the flow to carry it: the
        // first read after each reports the terminal value.
        sleep_past_topic_span();
        let on_source = describe_topic(&emulator, &source, topic).await;
        assert_eq!(
            (on_source.status, on_source.at("/status").as_str()),
            (200, Some("ACTIVE")),
            "case {topic:?}: the first read after the topic span must report ACTIVE, got {} {}",
            on_source.status,
            on_source.body
        );
        assert_eq!(
            on_source.at("/partitionCount").as_i64(),
            Some(partitions),
            "case {topic:?}: expected the submitted partition count back, got {}",
            on_source.body
        );

        sleep_past_replication_span();
        let carried = listed_topic_names(&emulator, &target).await;
        assert!(
            carried.iter().any(|name| name.ends_with(&format!(".{topic}"))),
            "case {topic:?}: the first read after the replication span must list the replicated \
             topic under the source cluster's alias, got {carried:?}"
        );

        // The teardown: the source topic is answered as absent once its own span is
        // out, in the sentence that marks it gone.
        let deleted = emulator
            .call(
                "DELETE",
                &format!("/v1/clusters/{}/topics/{}", pct(&source), pct(topic)),
                None,
            )
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {topic:?}: expected DeleteTopic to be accepted, got {} {}",
            deleted.status, deleted.body
        );
        sleep_past_topic_deletion_span();
        assert_absent(
            &describe_topic(&emulator, &source, topic).await,
            topic,
            NOT_FOUND_SENTENCE,
        );
        let left = listed_topic_names(&emulator, &source).await;
        assert!(
            !left.iter().any(|name| name == topic),
            "case {topic:?}: a deleted topic must leave the listing, got {left:?}"
        );
    }

    // And the replicator itself: `ReplicatorArn` is a parameter of every replicator
    // call, so its absence is reported as that parameter's.
    let deleted = emulator
        .call(
            "DELETE",
            &format!("/replication/v1/replicators/{}", pct(&replicator)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "expected DeleteReplicator to be accepted, got {} {}",
        deleted.status, deleted.body
    );
    assert_absent(
        &describe_replicator(&emulator, &replicator).await,
        "the deleted replicator",
        &format!("Invalid parameter replicatorArn: {NOT_FOUND_SENTENCE}"),
    );
    let listed = emulator
        .call("GET", "/replication/v1/replicators", None)
        .await;
    let names: Vec<String> = listed
        .at("/replicators")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.get("replicatorArn").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        !names.contains(&replicator),
        "a deleted replicator must leave the listing, got {names:?}"
    );
}
