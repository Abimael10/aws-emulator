// SPDX-License-Identifier: CC0-1.0

//! WHICH sentence `CreateClusterV2` refuses a body naming BOTH cluster types with, over the
//! bodies that name both.
//!
//! RULE under test: `provisioned` and `serverless` are two alternative request bodies, so a
//! create naming both is refused on the SELECTION — HTTP 400 `BadRequestException`, no
//! `invalidParameter`, nothing registered — and the sentence it reads is decided by the two
//! named blocks alone:
//!
//! * two blocks this service would each have built a cluster of that kind from state two
//!   creates at once, and the refusal states the rule the request broke: `You can specify only
//!   one cluster type, either provisioned or serverless.`
//! * a body one of whose two blocks is one the service would NOT have built states a create of
//!   neither kind, and the refusal reports the body as unreadable: `Invalid request body`.
//!
//! Both sentences are FIXED text — neither carries the cluster name, a version, a broker type,
//! a count, a subnet id or a tag — and neither names a member, because either member would
//! have fixed the request. The decision is taken from the request alone, so neither answer
//! depends on what the account already holds; the refused create leaves nothing behind, which
//! each row checks through `ListClustersV2` filtered on the name it submitted.
//!
//! The rows of the first group differ along everything the rule generalises over and the
//! selection does not depend on: the broker count at BOTH ends of the range the model declares
//! for it (`ProvisionedRequest$NumberOfBrokerNodes`, `range` 1..15) and in between, the broker
//! type across both categories the service offers and its one burstable size, the broker
//! software, how many VPC configurations the serverless block names, whether the blocks carry
//! storage, security groups and subnet lists of their own, whether the body carries tags, and
//! the cluster name's length at both ends of its modelled bound (`length` 1..64). The rows of
//! the second group differ along each way ONE of the two blocks is a block the service would
//! have refused on its own: broker software it does not offer, a broker type it does not offer,
//! a repeated client subnet, a serverless block naming no VPC configuration, one naming a
//! configuration with no subnet, and one that turns no SASL/IAM client authentication on.
//!
//! The control beside them: each of the two blocks, submitted on its OWN, is SERVED — so what
//! the rows above refuse is the selection, not the blocks.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, Emulator, Reply};

/// MSK's sentence for a create whose body names both cluster types with two blocks the service
/// would each have built a cluster from. Written out here rather than read from the service's
/// own constant, so this suite pins the wording instead of the code under test.
const ONE_CLUSTER_TYPE_MESSAGE: &str =
    "You can specify only one cluster type, either provisioned or serverless.";

/// MSK's sentence for a create naming both cluster types one of whose blocks the service would
/// not have built a cluster from — a body it cannot read as a create of either kind.
const INVALID_REQUEST_BODY_MESSAGE: &str = "Invalid request body";

/// The bounds `ProvisionedRequest$NumberOfBrokerNodes` and
/// `CreateClusterV2Request$ClusterName` declare, from
/// `smithy_query(query="shape",shape="ProvisionedRequest,CreateClusterV2Request",
/// model_path="services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json")`:
/// the broker count's `range` 1..15 and the name's `length` 1..64.
const BROKER_NODES_MIN: u64 = 1;
const BROKER_NODES_MAX: u64 = 15;
const CLUSTER_NAME_MIN_LENGTH: usize = 1;
const CLUSTER_NAME_MAX_LENGTH: usize = 64;

/// A `provisioned` block the service would build a cluster from: `version` on `instance_type`,
/// `brokers` brokers over two distinct subnets.
fn provisioned(version: &str, instance_type: &str, brokers: u64) -> Value {
    json!({
        "kafkaVersion": version,
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": instance_type,
            "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa", "subnet-bbbbbbbbbbbbbbbbb"],
        },
    })
}

/// The same block with the storage and security groups a fuller request carries, so one row
/// submits the shape a console or CDK create sends rather than the minimal one.
fn provisioned_with_storage(version: &str, instance_type: &str, brokers: u64) -> Value {
    let mut block = provisioned(version, instance_type, brokers);
    block["brokerNodeGroupInfo"]["securityGroups"] = json!(["sg-aaaaaaaaaaaaaaaaa"]);
    block["brokerNodeGroupInfo"]["storageInfo"] = json!({ "ebsStorageInfo": { "volumeSize": 10 } });
    block
}

/// A `serverless` block the service would build a cluster from: `configurations` VPC
/// configurations of one subnet each, with the SASL/IAM client authentication MSK Serverless
/// admits every client through.
fn serverless(configurations: usize) -> Value {
    json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({ "subnetIds": [format!("subnet-{index:017x}")] }))
            .collect::<Vec<Value>>(),
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// The same block carrying security groups of its own.
fn serverless_with_security_groups(configurations: usize) -> Value {
    json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({
                "subnetIds": [format!("subnet-{index:017x}")],
                "securityGroupIds": ["sg-bbbbbbbbbbbbbbbbb"],
            }))
            .collect::<Vec<Value>>(),
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// One row: how the case is named, the cluster name it submits, the body, and the values a
/// sentence composed from the request would have leaked into the answer.
struct Row {
    case: &'static str,
    name: String,
    body: Value,
    submitted: Vec<String>,
}

/// Bodies naming both cluster types with two blocks the service would EACH have built a
/// cluster from.
///
/// The broker software and broker types named here are ones the service offers a new cluster
/// (`service_com_amazonaws_kafka_kafka::state::new_cluster_kafka_versions` and
/// `BROKER_INSTANCE_TYPE_CATALOG`, which the control at the end of this suite re-checks by
/// having each block served on its own).
fn two_readable_blocks_rows() -> Vec<Row> {
    let shortest = "z".repeat(CLUSTER_NAME_MIN_LENGTH);
    let longest = "w".repeat(CLUSTER_NAME_MAX_LENGTH);
    vec![
        Row {
            case: "the fewest brokers the model admits and one VPC configuration",
            name: shortest.clone(),
            body: json!({
                "clusterName": shortest,
                "provisioned": provisioned("3.8.x", "kafka.m5.large", BROKER_NODES_MIN),
                "serverless": serverless(1),
            }),
            submitted: vec!["3.8.x".to_string(), "kafka.m5.large".to_string()],
        },
        Row {
            case: "the recorded shape: two brokers with storage, SASL/IAM serverless beside it",
            name: "both-recorded-shape".to_string(),
            body: json!({
                "clusterName": "both-recorded-shape",
                "provisioned": provisioned_with_storage("3.9.x", "kafka.m5.large", 2),
                "serverless": serverless_with_security_groups(1),
            }),
            submitted: vec![
                "both-recorded-shape".to_string(),
                "3.9.x".to_string(),
                "sg-aaaaaaaaaaaaaaaaa".to_string(),
            ],
        },
        Row {
            case: "the most brokers the model admits, five VPC configurations and tags",
            name: longest.clone(),
            body: json!({
                "clusterName": longest,
                "tags": { "team": "data", "costCentre": "cc-4711" },
                "provisioned": provisioned("3.9.x", "kafka.m7g.large", BROKER_NODES_MAX),
                "serverless": serverless(5),
            }),
            submitted: vec![
                longest.clone(),
                "kafka.m7g.large".to_string(),
                "costCentre".to_string(),
                "cc-4711".to_string(),
            ],
        },
        Row {
            case: "the burstable broker size, with an empty tag set",
            name: "both-burstable".to_string(),
            body: json!({
                "clusterName": "both-burstable",
                "tags": {},
                "provisioned": provisioned("3.8.x", "kafka.t3.small", 3),
                "serverless": serverless(2),
            }),
            submitted: vec![
                "both-burstable".to_string(),
                "kafka.t3.small".to_string(),
            ],
        },
        Row {
            case: "an Express broker category on a KRaft release",
            name: "both-express-kraft".to_string(),
            body: json!({
                "clusterName": "both-express-kraft",
                "provisioned": provisioned("4.0.x.kraft", "express.m7g.large", 6),
                "serverless": serverless_with_security_groups(3),
            }),
            submitted: vec![
                "both-express-kraft".to_string(),
                "4.0.x.kraft".to_string(),
                "express.m7g.large".to_string(),
                "sg-bbbbbbbbbbbbbbbbb".to_string(),
            ],
        },
    ]
}

/// Bodies naming both cluster types where ONE of the two blocks is a block the service would
/// have refused on its own, one row per way of failing that.
fn a_block_the_service_would_refuse_rows() -> Vec<Row> {
    vec![
        Row {
            case: "broker software the service does not offer",
            name: "both-unoffered-version".to_string(),
            body: json!({
                "clusterName": "both-unoffered-version",
                "provisioned": provisioned("0.0.0-not-a-version", "kafka.m5.large", 2),
                "serverless": serverless(1),
            }),
            submitted: vec!["0.0.0-not-a-version".to_string()],
        },
        Row {
            case: "a broker type the service does not offer",
            name: "both-unoffered-type".to_string(),
            body: json!({
                "clusterName": "both-unoffered-type",
                "provisioned": provisioned("3.9.x", "kafka.not-a-broker-type", 2),
                "serverless": serverless(1),
            }),
            submitted: vec!["kafka.not-a-broker-type".to_string()],
        },
        Row {
            case: "a provisioned block naming one client subnet twice",
            name: "both-repeated-subnet".to_string(),
            body: json!({
                "clusterName": "both-repeated-subnet",
                "provisioned": {
                    "kafkaVersion": "3.9.x",
                    "numberOfBrokerNodes": 2,
                    "brokerNodeGroupInfo": {
                        "instanceType": "kafka.m5.large",
                        "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa", "subnet-aaaaaaaaaaaaaaaaa"],
                    },
                },
                "serverless": serverless(1),
            }),
            submitted: vec!["subnet-aaaaaaaaaaaaaaaaa".to_string()],
        },
        Row {
            case: "a serverless block naming no VPC configuration",
            name: "both-no-vpc-config".to_string(),
            body: json!({
                "clusterName": "both-no-vpc-config",
                "provisioned": provisioned("3.9.x", "kafka.m5.large", 2),
                "serverless": {
                    "vpcConfigs": [],
                    "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
                },
            }),
            submitted: vec!["both-no-vpc-config".to_string()],
        },
        Row {
            case: "a serverless VPC configuration naming no subnet",
            name: "both-no-subnet".to_string(),
            body: json!({
                "clusterName": "both-no-subnet",
                "provisioned": provisioned("3.9.x", "kafka.m5.large", 2),
                "serverless": {
                    "vpcConfigs": [{ "subnetIds": [] }],
                    "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
                },
            }),
            submitted: vec!["both-no-subnet".to_string()],
        },
        Row {
            case: "a serverless block turning no SASL/IAM client authentication on",
            name: "both-unauthenticated".to_string(),
            body: json!({
                "clusterName": "both-unauthenticated",
                "provisioned": provisioned("3.9.x", "kafka.m5.large", 2),
                "serverless": {
                    "vpcConfigs": [{ "subnetIds": ["subnet-aaaaaaaaaaaaaaaaa"] }],
                    "clientAuthentication": { "sasl": { "iam": { "enabled": false } } },
                },
            }),
            submitted: vec!["both-unauthenticated".to_string()],
        },
    ]
}

/// Assert `reply` is the selection refusal carrying `expected`, naming no member and echoing
/// none of the values `row` submitted.
fn assert_selection_refusal(reply: &Reply, case: &str, expected: &str, submitted: &[String]) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", expected),
        "case {case:?}: expected a 400 BadRequestException {expected:?}, got {} {}",
        reply.status,
        reply.body
    );
    assert!(
        reply.at("/invalidParameter").is_null(),
        "case {case:?}: the selection refusal names no member, got {}",
        reply.body
    );
    for value in submitted {
        assert!(
            !reply.error_message().contains(value.as_str()),
            "case {case:?}: the refusal must not echo the submitted {value:?}, got {:?}",
            reply.error_message()
        );
    }
}

/// How many clusters `ListClustersV2` reports under `name`.
async fn listed_under(emulator: &Emulator, name: &str) -> usize {
    emulator
        .list_v2(&format!("?clusterNameFilter={}", pct(name)))
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// RULE: a create naming both cluster types with two blocks the service would each have built
/// a cluster from reads the mutual-exclusion sentence, whatever the two blocks ask for, and
/// registers nothing.
#[tokio::test]
async fn two_readable_cluster_type_blocks_read_the_mutual_exclusion_sentence() {
    let emulator = Emulator::new();
    for row in two_readable_blocks_rows() {
        let reply = emulator.create_cluster_v2(row.body.clone()).await;
        assert_selection_refusal(&reply, row.case, ONE_CLUSTER_TYPE_MESSAGE, &row.submitted);
        let held = listed_under(&emulator, &row.name).await;
        assert_eq!(
            held, 0,
            "case {:?}: a refused create registers no cluster, got {held} under {:?}",
            row.case, row.name
        );
    }
}

/// RULE: a create naming both cluster types ONE of whose blocks the service would have refused
/// on its own reads the invalid-body sentence instead — the body states a create of neither
/// kind — and registers nothing either.
#[tokio::test]
async fn a_cluster_type_block_the_service_would_refuse_reads_the_invalid_body_sentence() {
    let emulator = Emulator::new();
    for row in a_block_the_service_would_refuse_rows() {
        let reply = emulator.create_cluster_v2(row.body.clone()).await;
        assert_selection_refusal(
            &reply,
            row.case,
            INVALID_REQUEST_BODY_MESSAGE,
            &row.submitted,
        );
        let held = listed_under(&emulator, &row.name).await;
        assert_eq!(
            held, 0,
            "case {:?}: a refused create registers no cluster, got {held} under {:?}",
            row.case, row.name
        );
    }
}

/// RULE: the two sentences are DIFFERENT answers to two different requests, and each of the
/// blocks the first group names is one the service really builds a cluster from — which is
/// what makes that group's bodies two creates at once rather than one unreadable body.
#[tokio::test]
async fn the_two_selection_sentences_differ_and_each_block_is_served_on_its_own() {
    assert_ne!(
        ONE_CLUSTER_TYPE_MESSAGE, INVALID_REQUEST_BODY_MESSAGE,
        "the two ways of naming both cluster types must read different sentences"
    );

    let emulator = Emulator::new();
    for (index, row) in two_readable_blocks_rows().into_iter().enumerate() {
        for (member, block) in [
            ("provisioned", row.body["provisioned"].clone()),
            ("serverless", row.body["serverless"].clone()),
        ] {
            let name = format!("one-kind-{member}-{index}");
            let reply = emulator
                .create_cluster_v2(json!({ "clusterName": name, member: block }))
                .await;
            assert_eq!(
                reply.status,
                200,
                "case {:?}: the {member} block of a both-kinds body must be served on its own, \
                 got {} {}",
                row.case,
                reply.status,
                reply.body
            );
            let held = listed_under(&emulator, &name).await;
            assert_eq!(
                held, 1,
                "case {:?}: the served {member} create must register its cluster, got {held}",
                row.case
            );
        }
    }
}
