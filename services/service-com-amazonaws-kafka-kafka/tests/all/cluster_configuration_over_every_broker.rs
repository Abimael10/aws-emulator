// SPDX-License-Identifier: CC0-1.0

//! A cluster's BROKERS, one per node, reached at the addresses `GetBootstrapBrokers`
//! publishes — and the MSK configuration every one of them runs with.
//!
//! RULE under test: a bootstrap string carries ONE `hostname:port` pair per broker node of
//! the cluster, every pair being an address THIS emulator answers the Apache Kafka wire
//! protocol on for that access method — so a client that splits the string reaches a broker
//! of the cluster at each entry, each of them reports the cluster's whole broker list at
//! the same addresses, and each of them runs the `server.properties` of the configuration
//! revision the cluster carries: a `CreateTopics` that leaves the placement to the broker
//! (`num_partitions`/`replication_factor` of `-1`) is placed with the `num.partitions` and
//! `default.replication.factor` in force, whichever broker the client asked, while a
//! request naming its own count keeps it.
//!
//! The rows differ along what the rule generalises over: how many broker nodes the cluster
//! holds (one, two, three), and the partition count and replication factor its
//! configuration declares — so what is asserted is the configuration in force per node and
//! not the one value a single row happens to carry.

use std::collections::BTreeSet;

use ::kafka_protocol::messages::{
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, ProduceRequest, ProduceResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use serde_json::json;

use crate::bootstrap_broker_cardinality::{published, split};
use crate::broker_data_plane::{Client, RESOURCE_TYPE_BROKER};
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The `NumPartitions`/`ReplicationFactor` a request sends to leave the decision to the
/// broker (Apache Kafka protocol, `CreateTopics`).
const USE_BROKER_DEFAULT: i32 = -1;
/// See [`USE_BROKER_DEFAULT`]: the same sentinel, as the `ReplicationFactor` member
/// carries it.
const USE_BROKER_DEFAULT_REPLICATION: i16 = -1;

/// The two broker properties a cluster's MSK configuration declares below, as Apache
/// Kafka's "Broker Configs" name them.
const NUM_PARTITIONS: &str = "num.partitions";
const DEFAULT_REPLICATION_FACTOR: &str = "default.replication.factor";

/// The error a broker answers a produce to a topic it does not hold with (Apache Kafka's
/// `UNKNOWN_TOPIC_OR_PARTITION`), which is what a cluster whose configuration turns
/// `auto.create.topics.enable` off answers rather than creating the topic.
const UNKNOWN_TOPIC_OR_PARTITION: i16 = 3;

/// One row: the broker nodes the cluster holds and the placement its configuration
/// declares.
struct Row {
    case: &'static str,
    brokers: i64,
    subnets: Vec<&'static str>,
    partitions: i32,
    replication: i16,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // `BrokerNodeGroupInfo$NumberOfBrokerNodes` is `__integerMin1Max15`, so one is
            // the published minimum: a one-broker cluster publishes one pair, and its
            // configuration cannot ask for more replicas than it has nodes.
            case: "one broker node, four partitions on one replica",
            brokers: 1,
            subnets: vec!["subnet-a"],
            partitions: 4,
            replication: 1,
        },
        Row {
            case: "two broker nodes, five partitions on two replicas",
            brokers: 2,
            subnets: vec!["subnet-a", "subnet-b"],
            partitions: 5,
            replication: 2,
        },
        Row {
            case: "three broker nodes, two partitions on three replicas",
            brokers: 3,
            subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
            partitions: 2,
            replication: 3,
        },
    ]
}

/// The configuration the broker reports for ITSELF, as `DescribeConfigs` on the broker
/// resource `node` answers it — read through the harness's one reading of that answer
/// (`broker_data_plane::Client::configs`).
async fn broker_configuration(
    client: &mut Client,
    node: i32,
    case: &str,
) -> std::collections::BTreeMap<String, String> {
    // A null key list asks about every entry, which is what a client reading a broker's
    // whole configuration sends.
    client
        .configs(case, RESOURCE_TYPE_BROKER, &node.to_string(), None)
        .await
}

/// A cluster reached in the clear by unauthenticated clients, carrying an MSK
/// configuration that declares `partitions`, `replication` and no topic auto-creation —
/// the way a customer centralises topic defaults fleet-wide.
async fn given_configured_cluster(emulator: &Emulator, row: &Row, name: &str) -> String {
    let properties = format!(
        "auto.create.topics.enable=false\ndefault.replication.factor={}\nnum.partitions={}\n",
        row.replication, row.partitions
    );
    let configuration = emulator
        .create_configuration_ok(&format!("{name}-configuration"), None, properties.as_bytes())
        .await;
    let revision = emulator
        .describe_configuration_revision(&configuration, 1)
        .await;
    assert_eq!(
        revision.status, 200,
        "setup: DescribeConfigurationRevision must answer, got {} {}",
        revision.status, revision.body
    );
    let echoed = revision.at("/serverProperties").as_str().unwrap_or_default();
    let decoded = ::aws_smithy_types::base64::decode(echoed).unwrap_or_default();
    assert_eq!(
        String::from_utf8_lossy(&decoded),
        properties,
        "setup: the revision must echo the server properties it stored"
    );

    let mut body = ClusterRequest {
        client_broker: Some("PLAINTEXT"),
        unauthenticated: Some(true),
        brokers: row.brokers,
        subnets: row.subnets.clone(),
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
    let arn = reply
        .at("/clusterArn")
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateCluster({name}) answered no ARN"))
        .to_string();
    // The cluster reports the configuration it carries once it has settled, which is what a
    // client checks before it goes to the brokers. LOOKS AGAIN rather than waiting out a
    // span: the move out of `CREATING` is one the clock owes the cluster.
    crate::kafka_harness::poll_until_active(emulator, &arn, name).await;
    let described = emulator.describe_v2(&arn).await;
    assert_eq!(
        described
            .at("/clusterInfo/provisioned/currentBrokerSoftwareInfo/configurationArn")
            .as_str(),
        Some(configuration.as_str()),
        "setup: the cluster must report the configuration it was created with, got {}",
        described.body
    );
    assert_eq!(
        described
            .at("/clusterInfo/provisioned/currentBrokerSoftwareInfo/configurationRevision")
            .as_i64(),
        Some(1),
        "setup: the cluster must report the revision it was created with, got {}",
        described.body
    );
    arn
}

#[tokio::test]
async fn every_published_broker_serves_the_clusters_configuration() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let cluster = given_configured_cluster(
            &emulator,
            &row,
            &format!("configured-brokers-{}", row.brokers),
        )
        .await;
        // The plaintext string a client hands its Kafka client as `bootstrap.servers`: one
        // pair per broker node the cluster holds.
        emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;
        let pairs = published(&emulator, &cluster, case)
            .await
            .into_iter()
            .find(|(member, _)| member == "bootstrapBrokerString")
            .map(|(_, pairs)| pairs)
            .unwrap_or_else(|| panic!("case {case:?}: the cluster published no plaintext string"));
        assert_eq!(
            pairs.len(),
            usize::try_from(row.brokers).unwrap_or_default(),
            "case {case:?}: expected one hostname:port pair per broker node, got {pairs:?}"
        );
        let distinct: BTreeSet<&String> = pairs.iter().collect();
        assert_eq!(
            distinct.len(),
            1,
            "case {case:?}: every pair of one string names the access method's one address, \
             got {pairs:?}"
        );

        // EVERY published pair is dialled: each answers the protocol, reports the whole
        // broker list at the published addresses, and runs the cluster's configuration.
        for (index, pair) in pairs.iter().enumerate() {
            let probe = format!("{case} / {pair}");
            let mut client = Client::plaintext(&probe, pair).await;
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: every published pair must answer ApiVersions, got error {}",
                negotiated.error_code
            );
            // WHICH brokers the cluster holds: every node of it, reported at the address
            // this access method is published at, so a client that re-resolves the cluster
            // from its metadata keeps reaching this emulator's own socket.
            let metadata = client.metadata(&probe).await;
            let reported: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            assert_eq!(
                reported.iter().collect::<BTreeSet<&String>>(),
                distinct,
                "case {probe:?}: expected Metadata to report the published addresses \
                 {pairs:?}, got {reported:?}"
            );
            let reached: Vec<i32> = metadata
                .brokers
                .iter()
                .map(|broker| broker.node_id.0)
                .collect();
            assert_eq!(
                reached,
                (1..=i32::try_from(row.brokers).unwrap_or(1)).collect::<Vec<i32>>(),
                "case {probe:?}: expected the cluster's whole broker membership, got \
                 {reached:?}"
            );

            // The configuration EVERY broker of the cluster runs with: the cluster's own
            // server properties, read back from the broker the client reached.
            for answered_as in &reached {
                let in_force = broker_configuration(&mut client, *answered_as, &probe).await;
                assert_eq!(
                    in_force.get(NUM_PARTITIONS).map(String::as_str),
                    Some(row.partitions.to_string().as_str()),
                    "case {probe:?}: expected broker {answered_as} to report \
                     {NUM_PARTITIONS}={}, got {in_force:?}",
                    row.partitions
                );
                assert_eq!(
                    in_force
                        .get(DEFAULT_REPLICATION_FACTOR)
                        .map(String::as_str),
                    Some(row.replication.to_string().as_str()),
                    "case {probe:?}: expected broker {answered_as} to report \
                     {DEFAULT_REPLICATION_FACTOR}={}, got {in_force:?}",
                    row.replication
                );
            }

            // And a topic that leaves the placement to the broker is placed with that
            // configuration, whichever broker of the cluster the client asked.
            let topic = format!("left-to-broker-{}-{index}", row.brokers);
            let created = client
                .create_topics(
                    &probe,
                    &[(
                        topic.as_str(),
                        USE_BROKER_DEFAULT,
                        USE_BROKER_DEFAULT_REPLICATION,
                        &[],
                    )],
                    false,
                )
                .await;
            let placed = created
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("case {probe:?}: CreateTopics reported no result"));
            assert_eq!(
                (placed.1, placed.3, placed.4),
                (0, row.partitions, row.replication),
                "case {probe:?}: expected the broker to place {} partitions on {} replicas, \
                 got {placed:?}",
                row.partitions,
                row.replication
            );
            // The partition ids are 0..N-1, each on as many brokers as the factor in force.
            let reported = client.metadata(&probe).await;
            let held = reported
                .topics
                .iter()
                .find(|held| {
                    held.name
                        .as_ref()
                        .is_some_and(|name| name.0.as_str() == topic)
                })
                .cloned()
                .unwrap_or_else(|| panic!("case {probe:?}: Metadata reported no topic {topic}"));
            let ids: Vec<i32> = held
                .partitions
                .iter()
                .map(|partition| partition.partition_index)
                .collect();
            assert_eq!(
                ids,
                (0..row.partitions).collect::<Vec<i32>>(),
                "case {probe:?}: expected the partition ids 0..{}, got {ids:?}",
                row.partitions - 1
            );
            for partition in held.partitions.iter() {
                assert_eq!(
                    partition.replica_nodes.len(),
                    usize::try_from(row.replication).unwrap_or(1),
                    "case {probe:?}: partition {} must be placed on {} brokers, got {:?}",
                    partition.partition_index,
                    row.replication,
                    partition.replica_nodes
                );
            }

            // A request that names its OWN count keeps it: the configuration decides only
            // what the request left to the broker.
            let control = format!("explicit-{}-{index}", row.brokers);
            let explicit = client
                .create_topics(&probe, &[(control.as_str(), 1, 1, &[])], false)
                .await;
            let kept = explicit
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("case {probe:?}: CreateTopics reported no result"));
            assert_eq!(
                (kept.1, kept.3, kept.4),
                (0, 1, 1),
                "case {probe:?}: a request naming its own placement must keep it, got {kept:?}"
            );

            // And a produce to a topic nobody created is refused rather than creating it:
            // the cluster's configuration turns `auto.create.topics.enable` off.
            let never = format!("never-created-{}-{index}", row.brokers);
            let refused: ProduceResponse = client
                .call(
                    &probe,
                    ApiKey::Produce,
                    9,
                    &ProduceRequest::default()
                        .with_acks(1)
                        .with_timeout_ms(1_000)
                        .with_topic_data(vec![TopicProduceData::default()
                            .with_name(TopicName(StrBytes::from_string(never.clone())))
                            .with_partition_data(vec![PartitionProduceData::default()
                                .with_index(0)
                                .with_records(None)])]),
                )
                .await;
            let answered = refused
                .responses
                .first()
                .and_then(|topic| topic.partition_responses.first())
                .cloned()
                .unwrap_or_else(|| panic!("case {probe:?}: Produce reported no partition"));
            assert_eq!(
                answered.error_code, UNKNOWN_TOPIC_OR_PARTITION,
                "case {probe:?}: a produce to a topic nobody created must be refused with \
                 {UNKNOWN_TOPIC_OR_PARTITION}, got {}",
                answered.error_code
            );
            // And the topic is absent afterwards, so nothing was created by the attempt.
            let after = client.metadata(&probe).await;
            let created_anyway: Vec<String> = after
                .topics
                .iter()
                .filter_map(|held| held.name.as_ref().map(|name| name.0.to_string()))
                .filter(|held| *held == never)
                .collect();
            assert!(
                created_anyway.is_empty(),
                "case {probe:?}: the refused produce must leave no topic behind, got \
                 {created_anyway:?}"
            );
        }

        // Each published pair carries the port the core assigned that access method's
        // socket, on the one interface this emulator answers at.
        let mut ports: BTreeSet<u16> = BTreeSet::new();
        for pair in &pairs {
            let (host, port) = split(pair, case);
            assert!(
                ::service_common::data_plane_addressing::names_the_loopback_interface(&host),
                "case {case:?}: every published host must name this emulator's own \
                 interface, got {pair:?}"
            );
            ports.insert(port);
        }
        assert_eq!(
            ports.len(),
            1,
            "case {case:?}: every broker node of the method answers on its one port, got \
             {ports:?} in {pairs:?}"
        );
    }
}
