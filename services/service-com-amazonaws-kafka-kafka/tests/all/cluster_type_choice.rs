// SPDX-License-Identifier: CC0-1.0

//! The INVARIANCE of `CreateClusterV2`'s cluster-type refusals: one answer for each way the
//! choice can fail, over every body that fails it that way.
//!
//! The rule under test: a create carries EXACTLY ONE of the two alternative cluster-type
//! bodies, and each way of failing that has its own fixed answer — a create naming NEITHER is
//! refused about the `clusterType` parameter the request left unset, while a create naming
//! BOTH is refused on the SELECTION it made, with no member named: in the service's
//! mutual-exclusion sentence where it would have built a cluster from each named block, and in
//! its sentence for a body it cannot read where one of those blocks is one it would not have
//! built. Neither registers anything.
//!
//! Where [`super::create_and_update_input_gates`] pins the WORDING, this suite pins that each
//! answer is INVARIANT over a table of bodies that differ in everything that answer does not
//! depend on: what else the body carries (nothing, tags, an empty tag set) and what each named
//! block says (broker count, how many VPC configurations). Every row of one group must read one
//! answer, byte for byte, and the three groups must read different ones.
//!
//! The admitted side is driven too: the same store serves a create naming ONE kind, each of
//! the two, so what the rows assert is the choice and not the create.

use serde_json::{json, Value};

use crate::kafka_harness::{Emulator, Reply};

/// What MSK answers a create that names NEITHER cluster type, written out here rather than
/// read from the service's own constant, so the wording is pinned by this suite.
const NO_CLUSTER_TYPE_MESSAGE: &str =
    "You must specify a cluster type, either provisioned or serverless.";

/// What MSK answers a create whose body names BOTH cluster types with two blocks it would each
/// have built a cluster from: the service's sentence for the mutual exclusion of the two
/// cluster types, which states the rule and names no member.
const ONE_CLUSTER_TYPE_MESSAGE: &str =
    "You can specify only one cluster type, either provisioned or serverless.";

/// What MSK answers a create naming both cluster types one of whose blocks it would NOT have
/// built a cluster from: the service's sentence for a body it cannot read as a create of
/// either kind.
const INVALID_REQUEST_BODY_MESSAGE: &str = "Invalid request body";

/// A `provisioned` block asking for `brokers` brokers.
fn provisioned(brokers: u64) -> Value {
    json!({
        "kafkaVersion": "3.9.x",
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": "kafka.m5.large",
            "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa", "subnet-bbbbbbbbbbbbbbbbb"],
        },
    })
}

/// A `serverless` block naming `configurations` VPC configurations of one subnet
/// each, with the SASL/IAM client authentication a serverless cluster must use —
/// the one scheme MSK admits a serverless cluster's clients through, and one a
/// serverless create has to turn on.
fn serverless(configurations: usize) -> Value {
    json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({ "subnetIds": [format!("subnet-{index:017x}")] }))
            .collect::<Vec<Value>>(),
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// Bodies that name NEITHER cluster type, differing in what else they carry.
fn no_cluster_type_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the name alone",
            json!({ "clusterName": "choice-plain" }),
        ),
        (
            "with tags",
            json!({ "clusterName": "choice-tagged", "tags": { "team": "data" } }),
        ),
        (
            "with an empty tag set",
            json!({ "clusterName": "choice-untagged", "tags": {} }),
        ),
    ]
}

/// Bodies that name BOTH cluster types with two blocks the service would each have built a
/// cluster from, differing in what each block says.
fn both_readable_blocks_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one broker and one VPC configuration",
            json!({
                "clusterName": "choice-both-small",
                "provisioned": provisioned(1),
                "serverless": serverless(1),
            }),
        ),
        (
            "three brokers and two VPC configurations, with tags",
            json!({
                "clusterName": "choice-both-large",
                "tags": { "team": "data" },
                "provisioned": provisioned(3),
                "serverless": serverless(2),
            }),
        ),
    ]
}

/// Bodies that name BOTH cluster types where ONE of the two blocks is one the service would
/// not have built a cluster from, differing in which block that is and how it fails.
fn a_block_the_service_would_refuse_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a provisioned block that would itself have been refused",
            json!({
                "clusterName": "choice-both-bad-version",
                "provisioned": {
                    "kafkaVersion": "0.0.0-not-a-version",
                    "numberOfBrokerNodes": 2,
                    "brokerNodeGroupInfo": {
                        "instanceType": "kafka.m5.large",
                        "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa"],
                    },
                },
                "serverless": serverless(1),
            }),
        ),
        (
            "a serverless block that would itself have been refused",
            json!({
                "clusterName": "choice-both-unauthenticated",
                "provisioned": provisioned(2),
                "serverless": { "vpcConfigs": [{ "subnetIds": ["subnet-aaaaaaaaaaaaaaaaa"] }] },
            }),
        ),
    ]
}

/// The refusal as a comparable tuple: status, error code, message and the member blamed.
fn answered(reply: &Reply) -> (u16, String, String, Option<String>) {
    (
        reply.status,
        reply.error_code().to_string(),
        reply.error_message().to_string(),
        reply
            .at("/invalidParameter")
            .as_str()
            .map(str::to_string),
    )
}

/// RULE: each way of failing the cluster-type choice has ONE answer, invariant over the
/// bodies that fail it that way — and the three answers differ.
#[tokio::test]
async fn each_failed_cluster_type_choice_reads_its_own_one_refusal() {
    let emulator = Emulator::new();
    let no_type = (
        400u16,
        "BadRequestException".to_string(),
        NO_CLUSTER_TYPE_MESSAGE.to_string(),
        Some("clusterType".to_string()),
    );
    let both_readable = (
        400u16,
        "BadRequestException".to_string(),
        ONE_CLUSTER_TYPE_MESSAGE.to_string(),
        None,
    );
    let unreadable = (
        400u16,
        "BadRequestException".to_string(),
        INVALID_REQUEST_BODY_MESSAGE.to_string(),
        None,
    );
    for (left, right) in [
        (&no_type, &both_readable),
        (&no_type, &unreadable),
        (&both_readable, &unreadable),
    ] {
        assert_ne!(
            left, right,
            "every way of failing the choice must be told apart"
        );
    }

    for (case, body) in no_cluster_type_rows() {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            answered(&reply),
            no_type,
            "case a body naming neither cluster type, {case}: expected the parameter refusal, \
             got {} {}",
            reply.status,
            reply.body
        );
    }
    for (case, body) in both_readable_blocks_rows() {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            answered(&reply),
            both_readable,
            "case a body naming both cluster types with two readable blocks, {case}: expected \
             the mutual-exclusion refusal, got {} {}",
            reply.status,
            reply.body
        );
    }
    for (case, body) in a_block_the_service_would_refuse_rows() {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            answered(&reply),
            unreadable,
            "case a body naming both cluster types with a block the service would refuse, \
             {case}: expected the invalid-body refusal, got {} {}",
            reply.status,
            reply.body
        );
    }

    let listed = emulator
        .list_v2("")
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        listed, 0,
        "a refused create must register no cluster, got {listed} of them"
    );

    // The admitted side: naming ONE kind is served, whichever kind it is.
    for (case, body) in [
        (
            "a provisioned create",
            json!({ "clusterName": "choice-one-provisioned", "provisioned": provisioned(2) }),
        ),
        (
            "a serverless create",
            json!({ "clusterName": "choice-one-serverless", "serverless": serverless(1) }),
        ),
    ] {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: a create naming one cluster type must be accepted, got {} {}",
            reply.status, reply.body
        );
    }
}
