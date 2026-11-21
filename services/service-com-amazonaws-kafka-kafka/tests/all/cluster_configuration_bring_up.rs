// SPDX-License-Identifier: CC0-1.0

//! The BRING-UP a client of a configured provisioned cluster drives, end to end: an MSK
//! configuration, a cluster that carries it, the address that cluster publishes, and the
//! Apache Kafka protocol answered there.
//!
//! RULE under test: every leg of that sequence completes without the caller waiting on a
//! value that never arrives — `CreateConfiguration` stores the `server.properties` an
//! independent `DescribeConfigurationRevision` reads back, the cluster settles out of
//! `CREATING` on a clock, `GetBootstrapBrokers` then publishes an address THIS emulator
//! answers the Apache Kafka wire protocol on, and a client that dials it negotiates
//! `ApiVersions`, reads `Metadata` and has its `CreateTopics` answered. The protocol
//! answer is what makes the configuration observable: a request that leaves the partition
//! count and the replication factor to the broker is placed with the `num.partitions` and
//! `default.replication.factor` the cluster's configuration put in force.
//!
//! The rows differ along what the rule generalises over: the partition count the
//! configuration states (one, a count above the cluster's broker count, and a count a
//! request may not name itself), whether it states a replication factor at all, and how
//! many broker nodes the cluster holds.
//!
//! Every assertion goes over the published address, dialled on a real socket the core
//! opened for the cluster; nothing here reaches into the service's state.

use ::kafka_protocol::messages::{
    describe_configs_request::DescribeConfigsResource, ApiKey, DescribeConfigsRequest,
    DescribeConfigsResponse,
};
use ::kafka_protocol::protocol::StrBytes;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{poll_until_active, ClusterRequest, Emulator};

/// The `NumPartitions`/`ReplicationFactor` a request sends to leave the decision to the
/// broker (Apache Kafka protocol, `CreateTopics`).
const USE_BROKER_DEFAULT: i32 = -1;

/// The resource type a `DescribeConfigs` request names a BROKER by
/// (`ConfigResource.Type`, Apache Kafka protocol).
const RESOURCE_TYPE_BROKER: i8 = 4;

/// What the broker `node_id` reports as its own configuration, entry by entry.
async fn broker_configuration(
    client: &mut Client,
    case: &str,
    node_id: i32,
) -> std::collections::BTreeMap<String, String> {
    let answered: DescribeConfigsResponse = client
        .call(
            case,
            ApiKey::DescribeConfigs,
            4,
            &DescribeConfigsRequest::default().with_resources(vec![
                DescribeConfigsResource::default()
                    .with_resource_type(RESOURCE_TYPE_BROKER)
                    .with_resource_name(StrBytes::from_string(node_id.to_string()))
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

/// The api keys a client needs from a broker before it can administer and use a topic,
/// as their numbers travel in `ApiVersions`: `Produce` (0), `Metadata` (3),
/// `CreateTopics` (19) and `DescribeConfigs` (32) — the four a client of this sequence
/// negotiates (Apache Kafka protocol api keys).
const NEEDED_API_KEYS: &[i16] = &[0, 3, 19, 32];

/// One row: the case, the `server.properties` the cluster's configuration states, the
/// broker nodes the cluster holds, and the placement the broker must then answer a
/// request that leaves the decision to it with.
struct Row {
    case: &'static str,
    properties: &'static str,
    brokers: i64,
    partitions: i32,
    replication: i16,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: the three properties a fleet-wide MSK configuration
            // states, on a two-broker cluster.
            case: "a configuration stating the partition count and the replication factor",
            properties: "auto.create.topics.enable=false\ndefault.replication.factor=2\nnum.partitions=4\n",
            brokers: 2,
            partitions: 4,
            replication: 2,
        },
        Row {
            // One partition is Apache Kafka's own `num.partitions` default, so a
            // configuration that states it states the value the broker would have used.
            case: "a configuration stating one partition and no replication factor",
            properties: "num.partitions=1\n",
            brokers: 2,
            partitions: 1,
            replication: 1,
        },
        Row {
            // More partitions than the cluster has brokers: the count is the
            // configuration's, and the partitions are spread over the nodes there are.
            case: "a configuration stating more partitions than the cluster has brokers",
            properties: "num.partitions=6\ndefault.replication.factor=3\n",
            brokers: 3,
            partitions: 6,
            replication: 3,
        },
    ]
}

/// The `server.properties` body `revision` of the configuration `arn` stored, as an
/// independent read of the control plane reports it.
async fn stored_properties(emulator: &Emulator, arn: &str, revision: i64, case: &str) -> String {
    let reply = emulator.describe_configuration_revision(arn, revision).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected DescribeConfigurationRevision to return 200, got {} {}",
        reply.status, reply.body
    );
    let encoded = reply
        .at("/serverProperties")
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the revision reported no serverProperties: {}",
                reply.body
            )
        })
        .to_string();
    let decoded = ::aws_smithy_types::base64::decode(&encoded).unwrap_or_else(|error| {
        panic!("case {case:?}: the stored serverProperties must be base64: {error}")
    });
    String::from_utf8(decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the stored body must be text: {error}"))
}

#[tokio::test]
async fn a_configured_cluster_brings_its_data_plane_up_and_applies_the_configuration() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();

        // The configuration, and an independent read of what it stored.
        let configuration = emulator
            .create_configuration_ok(
                &format!("bring-up-{index}"),
                None,
                row.properties.as_bytes(),
            )
            .await;
        let stored = stored_properties(&emulator, &configuration, 1, case).await;
        assert_eq!(
            stored, row.properties,
            "case {case:?}: expected the revision to store the submitted properties, got \
             {stored:?}"
        );

        // The cluster that carries it, created the way a client creates one.
        let mut body = ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            brokers: row.brokers,
            subnets: vec!["subnet-a", "subnet-b"],
            ..ClusterRequest::named(&format!("bring-up-{index}"))
        }
        .to_json();
        body["configurationInfo"] =
            ::serde_json::json!({ "arn": configuration, "revision": 1 });
        let created = emulator.call("POST", "/v1/clusters", Some(body)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateCluster to return 200, got {} {}",
            created.status, created.body
        );
        let cluster = created
            .at("/clusterArn")
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateCluster answered no clusterArn"))
            .to_string();
        assert_eq!(
            created.at("/state").as_str(),
            Some("CREATING"),
            "case {case:?}: expected the create to answer CREATING, got {}",
            created.body
        );
        // LOOKS AGAIN until the cluster reports the state the clock owes it rather than
        // waiting out a span.
        poll_until_active(&emulator, &cluster, case).await;

        // The address it then publishes, and the protocol answered there.
        let published = emulator.bootstrap_brokers(&cluster).await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: expected GetBootstrapBrokers to return 200, got {} {}",
            published.status, published.body
        );
        let string = published
            .at("/bootstrapBrokerString")
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: an ACTIVE plaintext cluster must publish a bootstrap \
                     string: {}",
                    published.body
                )
            })
            .to_string();
        let pairs: Vec<String> = string.split(',').map(|pair| pair.trim().to_string()).collect();
        assert!(
            !pairs.is_empty(),
            "case {case:?}: the published string must carry a hostname:port pair, got \
             {string:?}"
        );
        for pair in &pairs {
            let probe = format!("{case} / {pair}");
            let mut client = Client::plaintext(&probe, pair).await;
            // Every api key a client of this sequence needs, as the broker advertises
            // them.
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: expected ApiVersions to be answered, got error {}",
                negotiated.error_code
            );
            let advertised: Vec<i16> = negotiated
                .api_keys
                .iter()
                .map(|api| api.api_key)
                .collect();
            for needed in NEEDED_API_KEYS {
                assert!(
                    advertised.contains(needed),
                    "case {probe:?}: expected api key {needed} to be advertised, got \
                     {advertised:?}"
                );
            }
            // And the brokers it reports: the cluster's own nodes, every one of them at
            // an address the bootstrap string publishes.
            let metadata = client.metadata(&probe).await;
            let reported: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            assert_eq!(
                reported.len(),
                usize::try_from(row.brokers).unwrap_or_default(),
                "case {probe:?}: expected Metadata to report the cluster's {} broker nodes, \
                 got {reported:?}",
                row.brokers
            );
            assert!(
                reported.iter().all(|address| pairs.contains(address)),
                "case {probe:?}: expected every reported broker at a published address \
                 {pairs:?}, got {reported:?}"
            );
        }

        // The configuration is what the broker then applies to a request that leaves the
        // placement to it, which is the whole point of attaching one.
        let probe = format!("{case} / the published address");
        let mut client = Client::plaintext(&probe, &pairs[0]).await;
        client.api_versions(&probe).await;
        let topic = format!("bring-up-topic-{index}");
        let created = client
            .create_topics(
                &probe,
                &[(topic.as_str(), USE_BROKER_DEFAULT, -1, &[])],
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
            "case {probe:?}: expected the broker to place {} partitions with replication {}, \
             got {placed:?}",
            row.partitions,
            row.replication
        );
        // Read back over the protocol: the partitions the broker placed are the ones a
        // client finds, each replicated as many times as the factor in force.
        let metadata = client.metadata(&probe).await;
        let held = metadata
            .topics
            .iter()
            .find(|held| held.name.as_ref().is_some_and(|name| name.0.as_str() == topic))
            .cloned()
            .unwrap_or_else(|| panic!("case {probe:?}: Metadata reported no topic {topic}"));
        assert_eq!(
            held.partitions.len(),
            usize::try_from(row.partitions).unwrap_or_default(),
            "case {probe:?}: expected {} partitions, got {}",
            row.partitions,
            held.partitions.len()
        );
        for partition in held.partitions.iter() {
            assert_eq!(
                partition.replica_nodes.len(),
                usize::try_from(row.replication).unwrap_or_default(),
                "case {probe:?}: partition {} must be placed on {} brokers, got {:?}",
                partition.partition_index,
                row.replication,
                partition.replica_nodes
            );
        }
        // And a broker-level read reports the configuration the cluster runs with.
        let in_force = broker_configuration(&mut client, &probe, metadata.controller_id.0).await;
        assert_eq!(
            in_force.get("num.partitions").map(String::as_str),
            Some(row.partitions.to_string().as_str()),
            "case {probe:?}: expected the broker to report num.partitions={}, got {in_force:?}",
            row.partitions
        );
    }
}
