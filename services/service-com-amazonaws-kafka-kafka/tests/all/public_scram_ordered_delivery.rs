// SPDX-License-Identifier: CC0-1.0

//! The WHOLE chain a client drives to produce over a PROVISIONED cluster's PUBLIC
//! SASL/SCRAM endpoint, and the order the records come back in.
//!
//! RULE under test: every leg of that chain is served, and each clock-owed transition in
//! it reaches its terminal value where the leg before it left the resource — a
//! `CreateClusterV2` that answers `CREATING` settles to `ACTIVE`, an `UpdateConnectivity`
//! that turns the public access on returns the cluster to `ACTIVE`, and a `CreateTopic`
//! that answers `CREATING` settles to `ACTIVE` — so a client that LOOKS AGAIN reads the
//! terminal value rather than polling a resource that never settles. The public
//! SASL/SCRAM address `GetBootstrapBrokers` then publishes completes the SCRAM exchange
//! and serves the data plane, where Apache Kafka's per-partition guarantee holds: the
//! records of one partition are handed back in exactly the order they were produced, at
//! consecutive offsets from the start of the log.
//!
//! The rows span what the rule generalises over: HOW MANY records cross the endpoint and
//! whether they arrive as one batch or as a request each — the per-partition order holds
//! across both, and across the batch boundaries a producer's own flushing decides.
//!
//! The cluster is the shape a client that centralises its broker settings runs: an MSK
//! `Configuration` the brokers take their `server.properties` from, two broker nodes, both
//! SASL schemes, TLS between client and broker, and a topic replicated onto both nodes.
//! The control-plane legs go over the same router the emulator serves from, and every
//! data-plane assertion goes over a real socket the core opened for the cluster, dialled
//! at the address the control plane published.

use ::serde_json::{json, Value};

use crate::broker_data_plane::{
    epoch_millis, given_secret_associated_as, Client, SCRAM_SHA_512,
};
use crate::kafka_harness::{pct, poll_until_active, v2_provisioned_body, Emulator};

/// The Apache Kafka version and broker type the cluster runs: the pair a client of a
/// standard provisioned cluster asks for.
const KAFKA_VERSION: &str = "3.9.x";
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// How many broker nodes the cluster holds, which is also the replication factor its
/// topics are created with: a topic replicated onto every node of the cluster.
const BROKER_NODES: i64 = 2;

/// The SASL/SCRAM user and password the cluster's associated secret carries.
const SCRAM_USER: &str = "public-scram-user";
const SCRAM_PASSWORD: &str = "public-scram-Secret-77";

/// How long a row waits for the state the clock owes a topic before it calls the chain
/// broken, and the cadence it re-reads at: far longer than the span the service owes that
/// transition, so the deadline says nothing about how fast the host runs.
const TOPIC_ACTIVE_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);
/// See [`TOPIC_ACTIVE_DEADLINE`].
const TOPIC_POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(100);

/// RULE: the chain settles leg by leg and the public SASL/SCRAM endpoint serves the
/// produced records back in the produced order.
#[tokio::test]
async fn the_public_scram_chain_settles_and_keeps_the_per_partition_order() {
    let case = "a provisioned SCRAM and IAM cluster made publicly reachable";
    let emulator = Emulator::with_secrets_manager();

    // The configuration the cluster's brokers run with: an ACL-enforcing one, which is
    // what a client that manages its own ACLs over the wire protocol configures.
    let configuration = emulator
        .create_configuration_ok(
            "public-scram-acl-enforced",
            None,
            b"allow.everyone.if.no.acl.found=false",
        )
        .await;

    let mut request = v2_provisioned_body("public-scram-ordering", KAFKA_VERSION, INSTANCE_TYPE);
    request["provisioned"]["numberOfBrokerNodes"] = json!(BROKER_NODES);
    request["provisioned"]["clientAuthentication"] = json!({
        "sasl": { "scram": { "enabled": true }, "iam": { "enabled": true } },
    });
    request["provisioned"]["encryptionInfo"] =
        json!({ "encryptionInTransit": { "clientBroker": "TLS" } });
    request["provisioned"]["configurationInfo"] = json!({ "arn": configuration, "revision": 1 });
    let created = emulator.create_cluster_v2(request).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateClusterV2 must be accepted, got {} {}",
        created.status, created.body
    );
    assert_eq!(
        created.at("/state").as_str(),
        Some("CREATING"),
        "case {case:?}: the create must answer the state it provisions from, got {}",
        created.body
    );
    let cluster = created
        .at("/clusterArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateClusterV2 returned no clusterArn"))
        .to_string();

    // LOOKS AGAIN until each leg's own transition is reported, rather than waiting out a
    // span: every one of them is a transition the clock owes the resource.
    let version = poll_until_active(&emulator, &cluster, case).await;
    given_secret_associated_as(
        &emulator,
        &cluster,
        "AmazonMSK_public_scram_ordering",
        SCRAM_USER,
        SCRAM_PASSWORD,
        case,
    )
    .await;
    update_connectivity(
        &emulator,
        &cluster,
        &version,
        &json!({ "publicAccess": { "type": "SERVICE_PROVIDED_EIPS" } }),
        case,
    )
    .await;
    poll_until_active(&emulator, &cluster, case).await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringPublicSaslScram", case)
        .await;

    // (how many records cross the endpoint, whether they arrive as one batch)
    let rows: &[(&str, &str, usize, bool)] = &[
        ("one record", "ordering-single", 1, true),
        ("two records in one batch", "ordering-pair", 2, true),
        ("a batch of many records", "ordering-batch", 25, true),
        ("a request per record", "ordering-per-request", 9, false),
    ];
    for (row, topic, count, one_batch) in rows {
        let case = &format!("{case} / {row}");
        given_active_topic(&emulator, &cluster, topic, case).await;
        let mut client = Client::tls(case, &authority).await;
        client.api_versions(case).await;
        let code = client
            .authenticate_as(case, SCRAM_SHA_512, SCRAM_USER, SCRAM_PASSWORD)
            .await;
        assert_eq!(
            code, 0,
            "case {case:?}: the SCRAM exchange on the public endpoint must authenticate \
             the connection, got code {code}"
        );

        let produced: Vec<String> = (0..*count).map(|index| format!("msg-{index:03}")).collect();
        let timestamp = epoch_millis();
        match one_batch {
            true => {
                let batch: Vec<(i64, Option<&str>, &str)> = produced
                    .iter()
                    .map(|value| (timestamp, None, value.as_str()))
                    .collect();
                let base = client.produce(case, topic, 0, &batch).await;
                assert_eq!(
                    base, 0,
                    "case {case:?}: the batch must land at the start of the log, got {base}"
                );
            }
            false => {
                for (index, value) in produced.iter().enumerate() {
                    let base = client
                        .produce(case, topic, 0, &[(timestamp, None, value.as_str())])
                        .await;
                    assert_eq!(
                        base,
                        index as i64,
                        "case {case:?}: record {index} must land at offset {index}, got {base}"
                    );
                }
            }
        }

        let read = client.fetch(case, topic, 0, 0).await;
        let consumed: Vec<String> = read.iter().map(|(_, _, value)| value.clone()).collect();
        assert_eq!(
            consumed, produced,
            "case {case:?}: expected the consumed sequence to equal the produced one \
             {produced:?}, got {consumed:?}"
        );
        let offsets: Vec<i64> = read.iter().map(|(offset, _, _)| *offset).collect();
        let expected: Vec<i64> = (0..*count as i64).collect();
        assert_eq!(
            offsets, expected,
            "case {case:?}: expected consecutive offsets {expected:?}, got {offsets:?}"
        );
    }
}

/// `PUT /v1/clusters/{ClusterArn}/connectivity` at the version the cluster is at.
async fn update_connectivity(
    emulator: &Emulator,
    arn: &str,
    version: &str,
    body: &Value,
    case: &str,
) {
    let reply = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/connectivity", pct(arn)),
            Some(json!({ "currentVersion": version, "connectivityInfo": body })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: UpdateConnectivity({body}) must be accepted, got {} {}",
        reply.status, reply.body
    );
}

/// A topic of the cluster, created through the control plane and READ BACK until it
/// reports the terminal state: `CreateTopic` answers the state it creates from, and the
/// move out of it is a transition the clock owes the topic.
///
/// The topic is replicated onto every broker node the cluster holds, which is what a
/// client of a two-node cluster asks for, and keeps its records for as long as the suite
/// runs (`retention.ms` -1 is Apache Kafka's "no limit") so what a row asserts about the
/// log does not depend on when it produced.
async fn given_active_topic(emulator: &Emulator, cluster: &str, name: &str, case: &str) {
    let configs = json!({ "retention.ms": "-1" });
    let created = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(json!({
                "topicName": name,
                "partitionCount": 1,
                "replicationFactor": BROKER_NODES,
                "configs": ::aws_smithy_types::base64::encode(configs.to_string()),
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateTopic({name}) must be accepted, got {} {}",
        created.status,
        created.error_message()
    );
    assert_eq!(
        created.at("/status").as_str(),
        Some("CREATING"),
        "case {case:?}: the create must answer the state it creates from, got {}",
        created.body
    );
    // LOOKS AGAIN until the topic reports the terminal state rather than waiting out a
    // span, with a deadline far above the one the service owes that transition — so the
    // row says nothing about how fast the host runs.
    let deadline = ::std::time::Instant::now() + TOPIC_ACTIVE_DEADLINE;
    loop {
        let described = emulator
            .call(
                "GET",
                &format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(name)),
                None,
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeTopic({name}) must answer, got {} {}",
            described.status,
            described.error_message()
        );
        if described.at("/status").as_str() == Some("ACTIVE") {
            assert_eq!(
                described.at("/replicationFactor").as_i64(),
                Some(BROKER_NODES),
                "case {case:?}: the topic must be replicated onto every broker node, got {}",
                described.body
            );
            return;
        }
        assert!(
            ::std::time::Instant::now() < deadline,
            "case {case:?}: the topic {name} never reported ACTIVE, last read {}",
            described.body
        );
        ::std::thread::sleep(TOPIC_POLL_INTERVAL);
    }
}
