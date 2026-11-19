// SPDX-License-Identifier: CC0-1.0

//! WHEN `CreateClusterV2` refuses the BODY rather than a member: the body-level refusal and
//! its place in the order of decisions.
//!
//! The rule under test: a `CreateClusterV2` body that names BOTH cluster types describes two
//! different clusters, so it is no create Amazon MSK can READ at all — it is refused on the
//! BODY, with HTTP 400 `BadRequestException`, MSK's one fixed sentence for a body it will not
//! read and NO
//! `invalidParameter`, because no single member is at fault. That decision is taken on the
//! body alone, so it comes BEFORE every decision taken on a member's value or on the
//! registry: a body naming both kinds reads the same sentence whether one of the blocks
//! would itself have been refused and whether the cluster name is one the account already
//! holds.
//!
//! The two neighbouring classes are driven alongside it, so what the rows pin is the class
//! and not the operation: a body naming NEITHER kind names no cluster type and is refused
//! about the `clusterType` parameter it left unset — a different sentence AND a named member
//! — while a body naming exactly ONE is served. Neither refusal registers a cluster. A body
//! the MODEL itself does not admit does not reach that decision at all: a member value
//! outside a declared bound is refused by the input-constraint layer, in this service's own
//! sentence for a parameter it will not accept.
//!
//! The rows differ along what the two blocks say (broker count, how many VPC configurations),
//! what else the body carries (tags), whether a named block would itself have been refused (an
//! unsupported `KafkaVersion`, a serverless block naming no VPC configuration) and whether the
//! name is free or already taken.

use serde_json::{json, Value};

use crate::kafka_harness::{assert_name_already_exists, Emulator, Reply};
use crate::refusal_wording::PARAMETERS_NOT_VALID;

/// MSK's fixed sentence for a create whose body names both cluster types, written out
/// here rather than read from the service's own constant, so the wording is pinned by this
/// suite instead of by the code under test: the two members are ALTERNATIVES, so a body
/// carrying both is a body MSK will not read, and the sentence it states about such a body
/// names nothing of the request.
const ONE_CLUSTER_TYPE_MESSAGE: &str = "Invalid request body";

/// MSK's fixed sentence for a create that names NO cluster type, and the parameter it blames.
const NO_CLUSTER_TYPE_MESSAGE: &str =
    "You must specify a cluster type, either provisioned or serverless.";
const CLUSTER_TYPE_MEMBER: &str = "clusterType";

/// A `provisioned` block asking for `brokers` brokers of broker software `version`.
fn provisioned(version: &str, brokers: i64) -> Value {
    json!({
        "kafkaVersion": version,
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": "kafka.m5.large",
            "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa", "subnet-bbbbbbbbbbbbbbbbb"],
        },
    })
}

/// A `serverless` block naming `configurations` VPC configurations of one subnet each.
fn serverless(configurations: usize) -> Value {
    json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({ "subnetIds": [format!("subnet-{index:017x}")] }))
            .collect::<Vec<Value>>(),
    })
}

/// The bodies that name both cluster types: how the case is named and the body itself, under
/// the cluster name the caller supplies.
fn both_cluster_type_rows(name: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "a minimal block of each kind",
            json!({
                "clusterName": name,
                "provisioned": provisioned("3.6.0", 1),
                "serverless": serverless(1),
            }),
        ),
        (
            "larger blocks, with tags",
            json!({
                "clusterName": name,
                "tags": { "team": "data" },
                "provisioned": provisioned("3.9.x", 6),
                "serverless": serverless(3),
            }),
        ),
        (
            "a provisioned block naming broker software the service does not offer",
            json!({
                "clusterName": name,
                "provisioned": provisioned("0.0.0-not-a-version", 2),
                "serverless": serverless(1),
            }),
        ),
        (
            "a serverless block naming no VPC configuration",
            json!({
                "clusterName": name,
                "provisioned": provisioned("3.6.0", 2),
                "serverless": { "vpcConfigs": [] },
            }),
        ),
    ]
}

/// A body naming both kinds whose `numberOfBrokerNodes` is outside the range the model
/// declares for it, which is refused by the input-constraint layer before any handler runs.
fn both_kinds_outside_a_modelled_bound(name: &str) -> Value {
    json!({
        "clusterName": name,
        "provisioned": provisioned("3.6.0", 0),
        "serverless": serverless(1),
    })
}

/// Assert `reply` is the selection refusal: the fixed sentence and no member named.
fn assert_invalid_request_body(reply: &Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", ONE_CLUSTER_TYPE_MESSAGE),
        "case {case}: expected a 400 BadRequestException {ONE_CLUSTER_TYPE_MESSAGE:?}, got \
         {} {}",
        reply.status,
        reply.body
    );
    assert!(
        reply.at("/invalidParameter").is_null(),
        "case {case}: expected the selection refusal to name no member, got {}",
        reply.body
    );
}

/// The cluster names the account holds, as `ListClustersV2` reports them.
async fn listed_names(emulator: &Emulator) -> Vec<String> {
    emulator
        .list_v2("")
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(|clusters| {
            clusters
                .iter()
                .map(|cluster| {
                    cluster["clusterName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a body naming both cluster types is refused on the SELECTION, whatever the two blocks
/// say — and the two neighbouring classes keep their own answers.
#[tokio::test]
async fn a_body_naming_both_cluster_types_is_refused_on_the_selection() {
    let emulator = Emulator::new();

    for (case, body) in both_cluster_type_rows("both-kinds-free") {
        let reply = emulator.create_cluster_v2(body).await;
        assert_invalid_request_body(&reply, case);
    }

    // A body naming NEITHER kind is the other class: a different sentence, and a member named.
    let reply = emulator
        .create_cluster_v2(json!({ "clusterName": "neither-kind" }))
        .await;
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", NO_CLUSTER_TYPE_MESSAGE),
        "case a body naming neither cluster type: expected a 400 BadRequestException \
         {NO_CLUSTER_TYPE_MESSAGE:?}, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(CLUSTER_TYPE_MEMBER),
        "case a body naming neither cluster type: expected {CLUSTER_TYPE_MEMBER:?} at fault, \
         got {}",
        reply.body
    );
    assert_ne!(
        NO_CLUSTER_TYPE_MESSAGE, ONE_CLUSTER_TYPE_MESSAGE,
        "the two ways of getting the cluster type wrong must read different sentences"
    );

    // A body the MODEL does not admit never reaches the decision above: a `numberOfBrokerNodes`
    // outside the range the model declares is refused by the input-constraint layer, in this
    // service's own sentence for a parameter it will not accept.
    let reply = emulator
        .create_cluster_v2(both_kinds_outside_a_modelled_bound("both-kinds-unbounded"))
        .await;
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", PARAMETERS_NOT_VALID),
        "case a body naming both kinds outside a modelled bound: expected a 400 \
         BadRequestException {PARAMETERS_NOT_VALID:?}, got {} {}",
        reply.status,
        reply.body
    );

    // Nothing was registered by either refusal, and a body naming exactly ONE kind is served.
    assert!(
        listed_names(&emulator).await.is_empty(),
        "a refused create must register no cluster, got {:?}",
        listed_names(&emulator).await
    );
    for (case, body) in [
        (
            "a provisioned create",
            json!({
                "clusterName": "one-kind-provisioned",
                "provisioned": provisioned("3.6.0", 2),
            }),
        ),
        (
            "a serverless create",
            json!({ "clusterName": "one-kind-serverless", "serverless": serverless(1) }),
        ),
    ] {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: a create naming one cluster type must be served, got {} {}",
            reply.status, reply.body
        );
    }
}

/// RULE, the ORDER: the selection refusal is taken on the body alone, so it answers a body
/// naming both kinds even when the name is one the account already holds — a conflict the
/// SAME name earns as soon as the body names one kind, which is what shows the decision was
/// reached first.
#[tokio::test]
async fn the_body_refusal_precedes_the_registry() {
    let emulator = Emulator::new();
    let taken = "taken-by-a-provisioned-cluster";
    let created = emulator
        .create_cluster_v2(json!({ "clusterName": taken, "provisioned": provisioned("3.6.0", 2) }))
        .await;
    assert_eq!(
        created.status, 200,
        "expected the first create to be served, got {} {}",
        created.status, created.body
    );
    let existing = created.at("/clusterArn").as_str().unwrap_or_default().to_string();

    // One kind under the taken name: the registry decides, and it is a conflict.
    let reply = emulator
        .create_cluster_v2(json!({ "clusterName": taken, "provisioned": provisioned("3.6.0", 2) }))
        .await;
    assert_name_already_exists(
        &reply,
        "one kind under a name already held",
        "clusterName",
        taken,
        &existing,
    );

    // Both kinds under that same taken name: the selection decides, before the registry is
    // asked.
    for (case, body) in both_cluster_type_rows(taken) {
        let reply = emulator.create_cluster_v2(body).await;
        assert_invalid_request_body(&reply, case);
    }

    // The store still holds the one cluster the served create registered.
    assert_eq!(
        listed_names(&emulator).await,
        vec![taken.to_string()],
        "expected the refused creates to register nothing beside the served one"
    );
}
