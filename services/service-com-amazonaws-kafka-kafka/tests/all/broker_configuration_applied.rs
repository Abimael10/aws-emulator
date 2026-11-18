// SPDX-License-Identifier: CC0-1.0

//! The BROKER CONFIGURATION an Amazon MSK cluster runs with: the `server.properties`
//! bundle of the MSK configuration revision the cluster carries, over Apache Kafka's
//! documented broker defaults.
//!
//! RULE under test: wherever the protocol lets a client leave a decision to the broker,
//! the broker decides it from the configuration it runs with — a `CreateTopics` request
//! whose `NumPartitions` or `ReplicationFactor` is `-1` is placed with the
//! `num.partitions` and `default.replication.factor` in force — and a client reads that
//! configuration back from `DescribeConfigs` on the broker resource and the placement
//! back from `Metadata`.
//!
//! The rows span what the rule generalises over: which properties the cluster's
//! configuration states (both, one, neither), the VALUES it states them at, and whether
//! the request names a count of its own instead of leaving it to the broker.

use ::kafka_protocol::messages::{
    describe_configs_request::DescribeConfigsResource, ApiKey, DescribeConfigsRequest,
    DescribeConfigsResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use serde_json::json;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The resource type a `DescribeConfigs` request names a BROKER by
/// (`ConfigResource.Type`, Apache Kafka protocol).
const RESOURCE_TYPE_BROKER: i8 = 4;

/// The `NumPartitions`/`ReplicationFactor` a request sends to leave the decision to the
/// broker (Apache Kafka protocol, `CreateTopics`).
const USE_BROKER_DEFAULT: i32 = -1;

/// The broker properties this suite states and reads back, and Apache Kafka's own
/// defaults for them ("Broker Configs", Apache Kafka documentation) — what a cluster
/// whose configuration states neither runs with.
const NUM_PARTITIONS: &str = "num.partitions";
const KAFKA_DEFAULT_PARTITIONS: i32 = 1;
const KAFKA_DEFAULT_REPLICATION: i32 = 1;

/// One row: the case, the `server.properties` the cluster's configuration states, the
/// partition count and replication factor the request names, and the placement the broker
/// must answer with.
type ConfiguredRow<'a> = (&'a str, &'a str, i32, i16, i32, i32);

/// RULE: a topic the request leaves to the broker is placed with the `num.partitions` and
/// `default.replication.factor` the cluster's own MSK configuration puts in force, and a
/// request that names a count of its own keeps it.
#[tokio::test]
async fn the_clusters_configuration_decides_what_the_broker_places() {
    let rows: &[ConfiguredRow] = &[
        (
            "a configuration stating both properties",
            "num.partitions=4\ndefault.replication.factor=2\n",
            USE_BROKER_DEFAULT,
            -1,
            4,
            2,
        ),
        (
            "a configuration stating only the partition count",
            "num.partitions=2\n",
            USE_BROKER_DEFAULT,
            -1,
            2,
            KAFKA_DEFAULT_REPLICATION,
        ),
        (
            "a configuration stating neither property",
            "auto.create.topics.enable=false\n",
            USE_BROKER_DEFAULT,
            -1,
            KAFKA_DEFAULT_PARTITIONS,
            KAFKA_DEFAULT_REPLICATION,
        ),
        (
            "a request that names its own placement",
            "num.partitions=4\ndefault.replication.factor=2\n",
            3,
            1,
            3,
            1,
        ),
    ];
    for (index, (case, properties, named_partitions, named_replication, partitions, replication)) in
        rows.iter().enumerate()
    {
        let emulator = Emulator::new();
        let cluster = given_configured_cluster(
            &emulator,
            &format!("broker-config-{index}"),
            properties.as_bytes(),
        )
        .await;
        let authority = plaintext_address(&emulator, &cluster, case).await;
        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;

        // What the broker places for the topic.
        let topic = format!("configured-{index}");
        let created = client
            .create_topics(
                case,
                &[(topic.as_str(), *named_partitions, *named_replication, &[])],
                false,
            )
            .await;
        let placed = created
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported no result"));
        assert_eq!(
            (placed.1, placed.3, i32::from(placed.4)),
            (0, *partitions, *replication),
            "case {case:?}: expected the broker to place {partitions} partitions with \
             replication {replication}, got {placed:?}"
        );

        // What a client reads back about that placement: one entry per partition, each
        // replicated on as many brokers as the factor in force.
        let metadata = client.metadata(case).await;
        let reported = metadata
            .topics
            .iter()
            .find(|held| held.name.as_ref().is_some_and(|name| name.0.as_str() == topic))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: Metadata reported no topic {topic}"));
        let replica_counts: Vec<usize> = reported
            .partitions
            .iter()
            .map(|partition| partition.replica_nodes.len())
            .collect();
        assert_eq!(
            replica_counts,
            vec![usize::try_from(*replication).unwrap_or(1); usize::try_from(*partitions).unwrap_or(1)],
            "case {case:?}: expected {partitions} partitions each on {replication} brokers, \
             got {replica_counts:?}"
        );
        for partition in reported.partitions.iter() {
            assert_eq!(
                partition.isr_nodes, partition.replica_nodes,
                "case {case:?}: partition {} must report every replica in sync, got {:?} of {:?}",
                partition.partition_index, partition.isr_nodes, partition.replica_nodes
            );
            assert_eq!(
                partition.replica_nodes.first().copied(),
                Some(partition.leader_id),
                "case {case:?}: partition {} must be led by the first broker it is placed \
                 on, got leader {:?} of {:?}",
                partition.partition_index,
                partition.leader_id,
                partition.replica_nodes
            );
        }

        // And what the broker reports as its own configuration: the value in force,
        // whichever of the two states it.
        let in_force = broker_configuration(&mut client, case).await;
        let expected_partitions = partitions_in_force(properties).unwrap_or(KAFKA_DEFAULT_PARTITIONS);
        assert_eq!(
            in_force.get(NUM_PARTITIONS).map(String::as_str),
            Some(expected_partitions.to_string().as_str()),
            "case {case:?}: expected the broker to report {NUM_PARTITIONS}={expected_partitions}, \
             got {in_force:?}"
        );
    }
}

/// The `num.partitions` a `server.properties` body states, read as a client of the
/// configuration reads it.
fn partitions_in_force(properties: &str) -> Option<i32> {
    properties
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == NUM_PARTITIONS)
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// The configuration the broker reports for itself, as `DescribeConfigs` on the broker
/// resource answers it.
async fn broker_configuration(
    client: &mut Client,
    case: &str,
) -> std::collections::BTreeMap<String, String> {
    let answered: DescribeConfigsResponse = client
        .call(
            case,
            ApiKey::DescribeConfigs,
            4,
            &DescribeConfigsRequest::default().with_resources(vec![
                DescribeConfigsResource::default()
                    .with_resource_type(RESOURCE_TYPE_BROKER)
                    .with_resource_name(StrBytes::from_static_str("1"))
                    // A null key list asks about every entry, which is what a client
                    // reading a broker's whole configuration sends.
                    .with_configuration_keys(None),
            ]),
        )
        .await;
    answered
        .results
        .iter()
        .flat_map(|result| result.configs.iter())
        .map(|entry| {
            (
                entry.name.to_string(),
                entry
                    .value
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// A cluster whose brokers are reached in the clear and which carries the MSK
/// configuration `properties` states, settled.
async fn given_configured_cluster(emulator: &Emulator, name: &str, properties: &[u8]) -> String {
    let configuration = emulator
        .create_configuration_ok(&format!("{name}-configuration"), None, properties)
        .await;
    let mut body = ClusterRequest {
        client_broker: Some("PLAINTEXT"),
        ..ClusterRequest::named(name)
    }
    .to_json();
    body["configurationInfo"] = json!({ "arn": configuration, "revision": 1 });
    let reply = emulator.call("POST", "/v1/clusters", Some(body)).await;
    assert_eq!(
        reply.status, 200,
        "setup: CreateCluster({name}) must be accepted, got {} {}",
        reply.status, reply.body
    );
    reply
        .at("/clusterArn")
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateCluster({name}) answered no ARN"))
        .to_string()
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}
