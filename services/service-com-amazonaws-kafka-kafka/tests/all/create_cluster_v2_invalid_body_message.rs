// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE `CreateClusterV2` refuses an unacceptable request BODY with is FIXED TEXT.
//!
//! RULE under test: a refusal a service takes on the request BODY as a whole — here the
//! cluster-type selection, which the API admits only one valid form of, the model declaring
//! `provisioned` and `serverless` as two separate optional structures rather than a union —
//! is answered with the service's ONE fixed sentence for a body it will not accept, naming no
//! member and interpolating nothing of the request. So the answer is the identical string for
//! every such body: no value the caller submitted and nothing the account already holds
//! reaches it.
//!
//! The neighbouring suite `create_cluster_v2_body_refusal` pins that this class is refused on
//! the BODY and where that decision sits in the order of decisions; its bodies, its
//! assertion and its sentence are reused here rather than restated. What these rows add is the
//! FIXEDNESS of the text, so they vary what that suite holds constant: the cluster name's form
//! and its length at both bounds the model declares for it, the broker count at both bounds of
//! its modelled range, the broker type, what the tags say, whether the serverless block names
//! client authentication, whether the name is one the account already holds and which region
//! the call is signed for — and each row asserts the answer echoes none of the values it
//! submitted and names neither cluster-type member. One name PAST the model's length bound is
//! driven too, because that request never reaches the decision: the input-constraint layer
//! answers it in this service's own sentence for a parameter it will not accept.

use serde_json::{json, Value};

use crate::create_cluster_v2_body_refusal::{
    assert_one_cluster_type, listed_names_in, provisioned, serverless,
};
use crate::kafka_harness::{Emulator, REGION};
use crate::refusal_wording::PARAMETERS_NOT_VALID;

/// The bounds `CreateClusterV2Request$ClusterName` declares, from
/// `smithy_query(query="operation",operation="CreateClusterV2")` on this crate's
/// `smithy-model/com-amazonaws-kafka-kafka.json`: `length` 1..64.
const CLUSTER_NAME_MIN_LENGTH: usize = 1;
const CLUSTER_NAME_MAX_LENGTH: usize = 64;

/// The bounds `ProvisionedRequest$NumberOfBrokerNodes` declares, from the same query:
/// `range` 1..15 — driven at both ends, so the sentence is pinned for a legal broker count at
/// either end of the published range.
const BROKER_NODES_MIN: i64 = 1;
const BROKER_NODES_MAX: i64 = 15;

/// The wire names of the two cluster-type members and of the parameter MSK blames when a
/// create names NEITHER of them: a refusal taken on the BODY names no member, so none of
/// these tokens may appear in its sentence.
const MEMBER_TOKENS: &[&str] = &["provisioned", "serverless", "clusterType"];

/// A region OTHER than the harness default, so one row shows the sentence is the service's own
/// rather than something composed per region.
const OTHER_REGION: &str = "eu-west-1";

/// The broker software and the broker type the shared `provisioned` block names, which a row
/// lists among the values its answer must not echo.
const BLOCK_VERSION: &str = "3.6.0";
const BLOCK_INSTANCE_TYPE: &str = "kafka.m5.large";

/// The same `provisioned` block on the broker type the caller names, so a row can name one the
/// service does not offer.
fn provisioned_on(instance_type: &str, version: &str, brokers: i64) -> Value {
    let mut block = provisioned(version, brokers);
    block["brokerNodeGroupInfo"]["instanceType"] = json!(instance_type);
    block
}

/// The same `serverless` block with no client authentication at all — a serverless body MSK
/// would itself have refused, had the selection not been refused first.
fn serverless_unauthenticated(configurations: usize) -> Value {
    let mut block = serverless(configurations);
    let _ = block
        .as_object_mut()
        .expect("the serverless block is a JSON object")
        .remove("clientAuthentication");
    block
}

/// One row: the body, the region the call is signed for, and every value the row submitted
/// that a sentence composed from the request would have leaked.
struct Row {
    case: &'static str,
    region: &'static str,
    body: Value,
    submitted: Vec<String>,
}

/// The rows. `held` is a cluster name the account ALREADY holds, so one row makes a selection
/// no body may make under a name the registry would itself have refused.
fn rows(held: &str) -> Vec<Row> {
    let shortest = "z".repeat(CLUSTER_NAME_MIN_LENGTH);
    let longest = "w".repeat(CLUSTER_NAME_MAX_LENGTH);
    vec![
        Row {
            case: "a cluster name of the shortest length the model admits",
            region: REGION,
            body: json!({
                "clusterName": shortest,
                "provisioned": provisioned(BLOCK_VERSION, BROKER_NODES_MIN),
                "serverless": serverless(1),
            }),
            // The name this row submits is ONE character, which any sentence can carry by
            // coincidence, so only the values long enough to be evidence are listed.
            submitted: vec![BLOCK_VERSION.to_string(), BLOCK_INSTANCE_TYPE.to_string()],
        },
        Row {
            case: "a cluster name of the longest length the model admits, with tags",
            region: REGION,
            body: json!({
                "clusterName": longest,
                "tags": { "owner": "platform-crew", "costCentre": "cc-4711" },
                "provisioned": provisioned("3.9.x", BROKER_NODES_MAX),
                "serverless": serverless(4),
            }),
            submitted: vec![
                longest.clone(),
                "platform-crew".to_string(),
                "costCentre".to_string(),
                "cc-4711".to_string(),
                "3.9.x".to_string(),
            ],
        },
        Row {
            case: "a name the account already holds",
            region: REGION,
            body: json!({
                "clusterName": held,
                "provisioned": provisioned(BLOCK_VERSION, 2),
                "serverless": serverless(2),
            }),
            submitted: vec![held.to_string()],
        },
        Row {
            case: "a provisioned block naming a broker type the service does not offer",
            region: REGION,
            body: json!({
                "clusterName": "body-refusal-unoffered-type",
                "provisioned": provisioned_on("kafka.not-a-broker-type", BLOCK_VERSION, 2),
                "serverless": serverless(1),
            }),
            submitted: vec![
                "body-refusal-unoffered-type".to_string(),
                "kafka.not-a-broker-type".to_string(),
            ],
        },
        Row {
            case: "a serverless block naming no client authentication",
            region: REGION,
            body: json!({
                "clusterName": "body-refusal-unauthenticated",
                "provisioned": provisioned(BLOCK_VERSION, 2),
                "serverless": serverless_unauthenticated(1),
            }),
            submitted: vec!["body-refusal-unauthenticated".to_string()],
        },
        Row {
            case: "a call signed for another region",
            region: OTHER_REGION,
            body: json!({
                "clusterName": "body-refusal-other-region",
                "provisioned": provisioned("3.9.x", 2),
                "serverless": serverless(3),
            }),
            submitted: vec![
                "body-refusal-other-region".to_string(),
                OTHER_REGION.to_string(),
            ],
        },
    ]
}

/// RULE: the sentence MSK refuses an unacceptable body with is FIXED, so every body naming
/// both cluster types reads the identical string — one that names neither cluster-type member
/// and carries no value the request submitted — whatever the name is, however long it is,
/// whatever the two blocks say, whatever tags the body carries, whether the name is one the
/// account already holds and whichever region the call is signed for.
#[tokio::test]
async fn the_body_refusal_sentence_is_fixed_whatever_the_request_says() {
    let emulator = Emulator::new();

    // A cluster the account holds, so one row's name is one the registry would itself have
    // refused — and so a stray write by a refused row shows as a change in what this region
    // reports.
    let held = "body-refusal-name-already-held";
    let created = emulator
        .create_cluster_v2(json!({ "clusterName": held, "provisioned": provisioned("3.9.x", 2) }))
        .await;
    assert_eq!(
        created.status, 200,
        "expected the held cluster to be created, got {} {}",
        created.status, created.body
    );

    for row in rows(held) {
        let case = row.case;
        let reply = emulator
            .call_in(row.region, "POST", "/api/v2/clusters", Some(row.body.clone()))
            .await;
        assert_one_cluster_type(&reply, case);
        let message = reply.error_message();
        for token in MEMBER_TOKENS {
            assert!(
                !message.contains(token),
                "case {case}: the body refusal must not name {token:?}, got {message:?}"
            );
        }
        for value in &row.submitted {
            assert!(
                !message.contains(value.as_str()),
                "case {case}: the body refusal must not echo the submitted {value:?}, got \
                 {message:?}"
            );
        }
    }

    // One character PAST the model's length bound for the cluster name: that request never
    // reaches the decision above, because the input-constraint layer refuses it first, in this
    // service's own sentence for a parameter it will not accept.
    let reply = emulator
        .create_cluster_v2(json!({
            "clusterName": "w".repeat(CLUSTER_NAME_MAX_LENGTH + 1),
            "provisioned": provisioned(BLOCK_VERSION, 2),
            "serverless": serverless(1),
        }))
        .await;
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", PARAMETERS_NOT_VALID),
        "case a cluster name past the model's length bound: expected a 400 BadRequestException \
         {PARAMETERS_NOT_VALID:?}, got {} {}",
        reply.status,
        reply.body
    );

    // A body naming exactly ONE kind is served in either region, so the rows above are the
    // selection being refused rather than the operation refusing everything.
    for (case, region, name, member, block) in [
        (
            "a provisioned create",
            REGION,
            "one-kind-provisioned",
            "provisioned",
            provisioned("3.9.x", 2),
        ),
        (
            "a serverless create in another region",
            OTHER_REGION,
            "one-kind-serverless",
            "serverless",
            serverless(1),
        ),
    ] {
        let reply = emulator
            .call_in(
                region,
                "POST",
                "/api/v2/clusters",
                Some(json!({ "clusterName": name, member: block })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: a create naming one cluster type must be served, got {} {}",
            reply.status, reply.body
        );
    }

    // The refused rows registered nothing: each region reports exactly the clusters its own
    // served creates minted. Sorted, because WHICH ORDER a list reports in is another rule's
    // subject and not this one's.
    let mut names = listed_names_in(&emulator, REGION).await;
    names.sort();
    assert_eq!(
        names,
        vec![held.to_string(), "one-kind-provisioned".to_string()],
        "expected the refused creates to register nothing in {REGION}"
    );
    assert_eq!(
        listed_names_in(&emulator, OTHER_REGION).await,
        vec!["one-kind-serverless".to_string()],
        "expected the refused creates to register nothing in {OTHER_REGION}"
    );
}
