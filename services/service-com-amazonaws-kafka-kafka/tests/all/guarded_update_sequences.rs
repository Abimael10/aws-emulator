// SPDX-License-Identifier: CC0-1.0

//! The two guarded MSK sequences a client walks when it grows a cluster's storage and
//! builds a replicator, and when it configures a topic across a broker reboot.
//!
//! RULE: every asynchronous state these sequences walk reaches its TERMINAL value
//! within ONE published span of the call that started it — so the FIRST read a client
//! makes after that span reports the terminal value — and until then the call that
//! CHANGES the resource is refused in Amazon MSK's fixed sentence for a resource that
//! is not in a state the call is accepted from, with nothing of the resource changed.
//!
//! The suites are the sequences themselves, step by step: after every write the
//! transient value is read back, the span is waited out ONCE, and the terminal value is
//! asserted on the next read. Nothing here polls, so a step that needed a second span
//! would fail — which is what makes these suites evidence about the CADENCE a client
//! observes, not only about the values.
//!
//! The refusal sentences are written out as Amazon MSK words them rather than built
//! from the service's own constants, so this suite pins the wording instead of
//! agreeing with whatever the code says.

use serde_json::json;

use crate::kafka_harness::{
    pct, sleep_past_activation, sleep_past_broker_reboot, sleep_past_update, ClusterRequest,
    Emulator, Reply,
};

/// How Amazon MSK refuses a call that changes a cluster which is not `ACTIVE`: fixed
/// text, with only the list of states the call IS accepted from determined by the
/// operation.
const CLUSTER_STATE_REFUSAL: &str =
    "This operation is only valid for resources that are in one of the following states :[ACTIVE]";

/// The same refusal for a replicator that is not `RUNNING`: fixed text apart from the
/// member name and the state list.
const REPLICATOR_STATE_REFUSAL: &str = "Invalid parameter replicatorArn: This operation is only \
                                        valid for resources that are in one of the following \
                                        states: [RUNNING]";

/// A role ARN the service stores and reports back for the replicator.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-sequences";

/// Sleep past the span a replicator takes to leave `CREATING`/`UPDATING`, and the one a
/// topic takes to become `ACTIVE`: the service's own published constants, so a suite
/// cannot drift from the service about how long a client waits.
fn sleep_past_replicator_span() {
    std::thread::sleep(
        ::service_com_amazonaws_kafka_kafka::state::REPLICATOR_ACTIVATION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// See [`sleep_past_replicator_span`].
fn sleep_past_topic_span() {
    std::thread::sleep(
        ::service_com_amazonaws_kafka_kafka::state::TOPIC_ACTIVATION_DELAY
            + std::time::Duration::from_millis(250),
    );
}

/// `DescribeClusterV2` for one cluster.
async fn described(emulator: &Emulator, cluster: &str) -> Reply {
    emulator.describe_v2(cluster).await
}

/// The cluster's `State` and `CurrentVersion` as the V2 reader reports them.
async fn cluster_state(emulator: &Emulator, cluster: &str, case: &str) -> (String, String) {
    let reply = described(emulator, cluster).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: DescribeClusterV2 must answer, got {} {}",
        reply.status, reply.body
    );
    (
        reply
            .at("/clusterInfo/state")
            .as_str()
            .unwrap_or_default()
            .to_string(),
        reply
            .at("/clusterInfo/currentVersion")
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// `DescribeClusterOperationV2`'s `OperationState` for one operation.
async fn operation_state(emulator: &Emulator, operation: &str, case: &str) -> String {
    let reply = emulator
        .call("GET", &format!("/api/v2/operations/{}", pct(operation)), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: DescribeClusterOperationV2 must answer, got {} {}",
        reply.status, reply.body
    );
    reply
        .at("/clusterOperationInfo/operationState")
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `UpdateStorage` with `volume_size_gb`, quoting `version`.
async fn update_storage(
    emulator: &Emulator,
    cluster: &str,
    version: &str,
    volume_size_gb: i64,
) -> Reply {
    emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/storage", pct(cluster)),
            Some(json!({ "currentVersion": version, "volumeSizeGB": volume_size_gb })),
        )
        .await
}

/// `UpdateRebalancing`, quoting `version`.
async fn update_rebalancing(emulator: &Emulator, cluster: &str, version: &str) -> Reply {
    emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/rebalancing", pct(cluster)),
            Some(json!({
                "currentVersion": version,
                "rebalancing": { "status": "ACTIVE" },
            })),
        )
        .await
}

/// Assert one reply is the cluster-state refusal, in Amazon MSK's own words.
fn assert_cluster_state_refusal(reply: &Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", CLUSTER_STATE_REFUSAL),
        "case {case:?}: expected the cluster-state refusal, got {} {}",
        reply.status,
        reply.body
    );
}

/// RULE, for the guarded cluster-update and replicator sequence: a cluster that is
/// still `CREATING` refuses `UpdateStorage` and `UpdateRebalancing` in Amazon MSK's
/// fixed sentence and keeps its `CurrentVersion`; one activation span later the FIRST
/// read reports `ACTIVE`, the same `UpdateStorage` is accepted and its cluster
/// operation reaches `UPDATE_COMPLETE` one update span later; a replicator created over
/// the two clusters reports `CREATING`, refuses `UpdateReplicationInfo` in the
/// replicator's own fixed sentence, and reports `RUNNING` on the FIRST read one
/// replicator span later, when the same update is accepted and bumps its version.
#[tokio::test]
async fn the_storage_and_rebalancing_guards_then_the_replicator_sequence_settle() {
    let case = "two clusters, a storage update and a replicator";
    let emulator = Emulator::new();
    let source = emulator
        .create_ok(&ClusterRequest {
            sasl_iam: Some(true),
            ..ClusterRequest::named("sequence-source")
        })
        .await;
    let target = emulator
        .create_ok(&ClusterRequest {
            sasl_iam: Some(true),
            ..ClusterRequest::named("sequence-target")
        })
        .await;

    // Still CREATING: both changes are refused, and neither moves the version.
    let (state, version) = cluster_state(&emulator, &source, case).await;
    assert_eq!(
        state, "CREATING",
        "case {case:?}: a just-created cluster is reported CREATING, got {state}"
    );
    let refused_storage = update_storage(&emulator, &source, &version, 11).await;
    assert_cluster_state_refusal(&refused_storage, "UpdateStorage while CREATING");
    let refused_rebalancing = update_rebalancing(&emulator, &source, &version).await;
    assert_cluster_state_refusal(&refused_rebalancing, "UpdateRebalancing while CREATING");
    let (_, after_refusal) = cluster_state(&emulator, &source, case).await;
    assert_eq!(
        after_refusal, version,
        "case {case:?}: a refused update must leave the cluster's version alone"
    );

    // One activation span, and the FIRST read of each cluster reports ACTIVE.
    sleep_past_activation();
    for cluster in [&source, &target] {
        let (state, _) = cluster_state(&emulator, cluster, case).await;
        assert_eq!(
            state, "ACTIVE",
            "case {case:?}: the first read after the activation span must report ACTIVE, got \
             {state}"
        );
    }

    // The same update is now accepted, and its operation finishes within one span.
    let (_, version) = cluster_state(&emulator, &source, case).await;
    let accepted = update_storage(&emulator, &source, &version, 11).await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: UpdateStorage on an ACTIVE cluster must be accepted, got {} {}",
        accepted.status, accepted.body
    );
    let operation = accepted
        .at("/clusterOperationArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: UpdateStorage reported no clusterOperationArn"))
        .to_string();
    assert_eq!(
        operation_state(&emulator, &operation, case).await,
        "UPDATE_IN_PROGRESS",
        "case {case:?}: an accepted update is in progress until its span elapses"
    );
    sleep_past_update();
    assert_eq!(
        operation_state(&emulator, &operation, case).await,
        "UPDATE_COMPLETE",
        "case {case:?}: the first read after the update span must report UPDATE_COMPLETE"
    );
    let (state, _) = cluster_state(&emulator, &source, case).await;
    assert_eq!(
        state, "ACTIVE",
        "case {case:?}: a completed update leaves the cluster ACTIVE, got {state}"
    );

    // The replicator over the two clusters: CREATING, refusing a change, then RUNNING.
    let created = emulator
        .call(
            "POST",
            "/replication/v1/replicators",
            Some(json!({
                "replicatorName": "sequence-replicator",
                "serviceExecutionRoleArn": ROLE_ARN,
                "kafkaClusters": [
                    { "amazonMskCluster": { "mskClusterArn": source },
                      "vpcConfig": { "subnetIds": ["subnet-a", "subnet-b", "subnet-c"] } },
                    { "amazonMskCluster": { "mskClusterArn": target },
                      "vpcConfig": { "subnetIds": ["subnet-a", "subnet-b", "subnet-c"] } },
                ],
                "replicationInfoList": [{
                    "sourceKafkaClusterArn": source,
                    "targetKafkaClusterArn": target,
                    "targetCompressionType": "NONE",
                    "topicReplication": { "topicsToReplicate": ["sim2real.*"] },
                    "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                }],
            })),
        )
        .await;
    assert_eq!(
        (created.status, created.at("/replicatorState").as_str()),
        (200, Some("CREATING")),
        "case {case:?}: CreateReplicator reports CREATING, got {} {}",
        created.status,
        created.body
    );
    let replicator = created
        .at("/replicatorArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateReplicator reported no replicatorArn"))
        .to_string();
    let replicator_uri = format!("/replication/v1/replicators/{}", pct(&replicator));
    let described = emulator.call("GET", &replicator_uri, None).await;
    assert_eq!(
        described.at("/replicatorState").as_str(),
        Some("CREATING"),
        "case {case:?}: a just-created replicator reads back CREATING, got {}",
        described.body
    );
    let replicator_version = described
        .at("/currentVersion")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeReplicator reported no currentVersion"))
        .to_string();
    let update_body = |version: &str, pattern: &str| {
        json!({
            "currentVersion": version,
            "sourceKafkaClusterArn": source,
            "targetKafkaClusterArn": target,
            "topicReplication": {
                "copyAccessControlListsForTopics": false,
                "copyTopicConfigurations": false,
                "detectAndCopyNewTopics": false,
                "topicsToExclude": [],
                "topicsToReplicate": [pattern],
            },
        })
    };
    let refused = emulator
        .call(
            "PUT",
            &format!("{replicator_uri}/replication-info"),
            Some(update_body(&replicator_version, "payments.*")),
        )
        .await;
    assert_eq!(
        (refused.status, refused.error_code(), refused.error_message()),
        (400, "BadRequestException", REPLICATOR_STATE_REFUSAL),
        "case {case:?}: expected the replicator-state refusal, got {} {}",
        refused.status,
        refused.body
    );

    sleep_past_replicator_span();
    let settled = emulator.call("GET", &replicator_uri, None).await;
    assert_eq!(
        settled.at("/replicatorState").as_str(),
        Some("RUNNING"),
        "case {case:?}: the first read after the replicator span must report RUNNING, got {}",
        settled.body
    );
    let accepted = emulator
        .call(
            "PUT",
            &format!("{replicator_uri}/replication-info"),
            Some(update_body(&replicator_version, "payments.*")),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: the same update on a RUNNING replicator must be accepted, got {} {}",
        accepted.status, accepted.body
    );
    sleep_past_replicator_span();
    let after = emulator.call("GET", &replicator_uri, None).await;
    assert_eq!(
        after.at("/replicatorState").as_str(),
        Some("RUNNING"),
        "case {case:?}: an applied update leaves the replicator RUNNING, got {}",
        after.body
    );
    assert_ne!(
        after.at("/currentVersion").as_str(),
        Some(replicator_version.as_str()),
        "case {case:?}: an accepted update must change the replicator's version"
    );
    assert_eq!(
        after.at("/replicationInfoList/0/topicReplication/topicsToReplicate"),
        &json!(["payments.*"]),
        "case {case:?}: the accepted update must be readable from the replicator, got {}",
        after.body
    );
}

/// The base64 topic-configuration string a client sets `retention.ms` with.
fn retention_configs(retention_ms: &str) -> String {
    ::aws_smithy_types::base64::encode(json!({ "retention.ms": retention_ms }).to_string())
}

/// RULE, for the topic and broker-reboot sequence: a created topic reports `CREATING`
/// and is `ACTIVE` on the FIRST read one topic span later; an accepted `UpdateTopic`
/// reports `UPDATING` and its configuration is readable one span later; a `RebootBroker`
/// reports a cluster operation that is `REBOOT_IN_PROGRESS` and `REBOOT_COMPLETE` one
/// reboot span later with the cluster back to `ACTIVE`; and the topic's configuration
/// survives the reboot, with a further `UpdateTopic` readable after it.
#[tokio::test]
async fn the_topic_and_reboot_sequence_settles_and_a_topic_config_survives_a_reboot() {
    let case = "a topic configured across a broker reboot";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest::named("sequence-topic"))
        .await;
    sleep_past_activation();

    let created = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(&cluster)),
            Some(json!({ "topicName": "ttl", "partitionCount": 1, "replicationFactor": 1 })),
        )
        .await;
    assert_eq!(
        (created.status, created.at("/status").as_str()),
        (200, Some("CREATING")),
        "case {case:?}: CreateTopic reports CREATING, got {} {}",
        created.status,
        created.body
    );
    let topic_uri = format!("/v1/clusters/{}/topics/ttl", pct(&cluster));
    let describe_topic = || emulator.call("GET", &topic_uri, None);
    assert_eq!(
        describe_topic().await.at("/status").as_str(),
        Some("CREATING"),
        "case {case:?}: a just-created topic reads back CREATING"
    );
    sleep_past_topic_span();
    assert_eq!(
        describe_topic().await.at("/status").as_str(),
        Some("ACTIVE"),
        "case {case:?}: the first read after the topic span must report ACTIVE"
    );

    // The configuration a client sets is readable once the change has rolled out.
    for retention in ["3600000", "7200000"] {
        let updated = emulator
            .call(
                "PUT",
                &topic_uri,
                Some(json!({ "configs": retention_configs(retention) })),
            )
            .await;
        assert_eq!(
            (updated.status, updated.at("/status").as_str()),
            (200, Some("UPDATING")),
            "case {case:?}: an accepted UpdateTopic reports UPDATING, got {} {}",
            updated.status,
            updated.body
        );
        sleep_past_topic_span();
        let described = describe_topic().await;
        assert_eq!(
            (
                described.at("/status").as_str(),
                described.at("/configs").as_str()
            ),
            (Some("ACTIVE"), Some(retention_configs(retention).as_str())),
            "case {case:?}: the first read after the update span must echo retention.ms \
             {retention}, got {}",
            described.body
        );

        // And the reboot of every broker in turn, after which the configuration the
        // topic carries is still the one the client set.
        for broker in ["1", "2", "3"] {
            let reboot = emulator
                .call(
                    "PUT",
                    &format!("/v1/clusters/{}/reboot-broker", pct(&cluster)),
                    Some(json!({ "brokerIds": [broker] })),
                )
                .await;
            assert_eq!(
                reboot.status, 200,
                "case {case:?}: RebootBroker({broker}) must be accepted, got {} {}",
                reboot.status, reboot.body
            );
            let operation = reboot
                .at("/clusterOperationArn")
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: RebootBroker reported no clusterOperationArn")
                })
                .to_string();
            assert_eq!(
                operation_state(&emulator, &operation, case).await,
                "REBOOT_IN_PROGRESS",
                "case {case:?}: a reboot is in progress until its span elapses"
            );
            let (state, _) = cluster_state(&emulator, &cluster, case).await;
            assert_eq!(
                state, "REBOOTING_BROKER",
                "case {case:?}: a rebooting cluster reports REBOOTING_BROKER, got {state}"
            );
            sleep_past_broker_reboot();
            assert_eq!(
                operation_state(&emulator, &operation, case).await,
                "REBOOT_COMPLETE",
                "case {case:?}: the first read after the reboot span must report REBOOT_COMPLETE"
            );
            let (state, _) = cluster_state(&emulator, &cluster, case).await;
            assert_eq!(
                state, "ACTIVE",
                "case {case:?}: a completed reboot leaves the cluster ACTIVE, got {state}"
            );
        }
        let described = describe_topic().await;
        assert_eq!(
            described.at("/configs").as_str(),
            Some(retention_configs(retention).as_str()),
            "case {case:?}: the topic's configuration must survive the reboot, got {}",
            described.body
        );
        assert_eq!(
            described.at("/status").as_str(),
            Some("ACTIVE"),
            "case {case:?}: the topic is usable again after the reboot, got {}",
            described.body
        );
    }
}
