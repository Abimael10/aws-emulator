// SPDX-License-Identifier: CC0-1.0

//! How a request refused for a value outside an ENUMERATED TYPE's value set is worded.
//!
//! The same rule `bound_refusal_wording.rs` pins over the modelled `@length` and
//! `@range` requirements, over the one constraint class of this service's inputs that is
//! neither: `GetShardIterator$ShardIteratorType`. A value the type does not admit is
//! refused with HTTP 400, `ValidationException`, and the constraint envelope
//! `1 validation error detected: Value at 'shardIteratorType' failed to satisfy
//! constraint: Member must satisfy enum value set: [<the declared values>]` — the member
//! and the set the model declares, and nothing the request carried.
//!
//! The rows differ in HOW a value misses the set (a declared value in another casing, a
//! misspelling of one, the empty string, a value no stream API declares, a long value),
//! beside the neighbouring class the rule must not swallow: every value the model DOES
//! declare passes the set, and the two that need no further member are served.
//!
//! The value set is the model's, from
//! `smithy_query(query="shape", shape="ShardIteratorType")`.
//!
//! Every row goes over the wire (`POST /` +
//! `X-Amz-Target: DynamoDBStreams_20120810.GetShardIterator`), so nothing is asserted
//! that a client cannot observe.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::KeySchemaElement;
use streams::state::{
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, StreamRegistration,
    StreamViewType,
};

/// A SigV4 credential scope naming `dynamodb`, which is how a DynamoDB Streams request
/// is signed. Request authentication is off here, so only its routing signal matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

/// The values the model declares for `ShardIteratorType`, in the order it declares them,
/// which is the order the refusal lists the set in.
const SHARD_ITERATOR_TYPES: [&str; 5] = [
    "TRIM_HORIZON",
    "LATEST",
    "AT_SEQUENCE_NUMBER",
    "AFTER_SEQUENCE_NUMBER",
    "AT_TIMESTAMP",
];

/// The two declared values that name a start point needing no further member, so a
/// request carrying one of them is served rather than refused for what it left out.
const SELF_SUFFICIENT_TYPES: [&str; 2] = ["TRIM_HORIZON", "LATEST"];

fn build() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(streams::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("authorization", AUTHORIZATION)
        .header(
            "x-amz-target",
            format!("DynamoDBStreams_20120810.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

/// The wire error code of a refusal, as an SDK client matches it.
fn error_code(body: &serde_json::Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The message a refusal carries, under either of the casings a client reads it by.
fn message(body: &serde_json::Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The whole message a value outside the modelled value set is refused with.
fn value_set_refusal() -> String {
    format!(
        "1 validation error detected: Value at 'shardIteratorType' failed to satisfy \
         constraint: Member must satisfy enum value set: [{}]",
        SHARD_ITERATOR_TYPES.join(", ")
    )
}

/// An instant `seconds` before now, so the registered stream's asynchronous enable is
/// long due and no row waits on the clock.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch");
    UNIX_EPOCH + now - Duration::from_secs(seconds)
}

/// Enable a stream on `table_name` through the publishing seam DynamoDB's
/// `CreateTable`/`UpdateTable` calls, and return its ARN.
fn register(engine: &engine::InMemoryEngine, table_name: &str, at: SystemTime) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type: StreamViewType::KeysOnly,
                key_schema: vec![KeySchemaElement {
                    attribute_name: "Id".to_string(),
                    key_type: "HASH".to_string(),
                }],
                region: REGION.to_string(),
            },
            at,
        )
        .unwrap_or_else(|rejected| panic!("the registry must accept {table_name}: {rejected}"))
}

/// The only shard of a freshly registered stream, read back over the wire.
async fn only_shard_id(router: &MultiServiceRouter, stream_arn: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStream",
        serde_json::json!({ "StreamArn": stream_arn }),
    )
    .await;
    assert_eq!(status, 200, "DescribeStream answered {status}: {body}");
    body["StreamDescription"]["Shards"][0]["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("no ShardId in {body}"))
        .to_string()
}

/// A `ShardIteratorType` the model does not admit states the declared value set, and
/// never the value the request carried.
#[tokio::test]
async fn an_unmodelled_iterator_type_states_the_value_set_and_no_value() {
    let (engine, router) = build();
    let stream_arn = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &stream_arn).await;
    let expected = value_set_refusal();

    let outside: [(&str, String); 5] = [
        ("a declared value lower-cased", "trim_horizon".to_string()),
        (
            "a misspelling of a declared value",
            "TRIM-HORIZON".to_string(),
        ),
        ("the empty string", String::new()),
        ("a value no stream API declares", "SOMETIME".to_string()),
        ("a long value", "X".repeat(256)),
    ];

    for (case, submitted) in outside {
        let (status, body) = call(
            &router,
            "GetShardIterator",
            serde_json::json!({
                "StreamArn": stream_arn,
                "ShardId": shard_id,
                "ShardIteratorType": submitted,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the set must be refused with 400, got {status} \
             and {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        let stated = message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
        // The quoted-value half of a clause, which this service's refusals never carry.
        assert!(
            !stated.contains("Value '"),
            "case {case:?}: the clause must quote no submitted value, got {stated:?}"
        );
    }
}

/// The neighbouring class: every value the model declares passes the set, and the ones
/// needing no further member are served.
#[tokio::test]
async fn every_declared_iterator_type_passes_the_value_set() {
    let (engine, router) = build();
    let stream_arn = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &stream_arn).await;
    let refusal = value_set_refusal();

    for declared in SHARD_ITERATOR_TYPES {
        let (status, body) = call(
            &router,
            "GetShardIterator",
            serde_json::json!({
                "StreamArn": stream_arn,
                "ShardId": shard_id,
                "ShardIteratorType": declared,
            }),
        )
        .await;
        let stated = message(&body);
        assert_ne!(
            stated, refusal,
            "case {declared:?}: a declared value must not be refused by the value set, got \
             {stated:?}"
        );
        if SELF_SUFFICIENT_TYPES.contains(&declared) {
            assert_eq!(
                status, 200,
                "case {declared:?}: a declared type needing no further member must be \
                 served, got {status} and {body}"
            );
            assert!(
                body["ShardIterator"].is_string(),
                "case {declared:?}: expected a minted iterator, got {body}"
            );
        }
    }
}
