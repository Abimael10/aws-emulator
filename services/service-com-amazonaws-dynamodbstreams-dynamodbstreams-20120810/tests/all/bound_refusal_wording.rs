// SPDX-License-Identifier: CC0-1.0

//! How a request this service refuses over a modelled BOUND is worded.
//!
//! The rule pinned here: a value outside a bound the model declares on its member — a
//! `@length` window or a `@range` floor — is refused with HTTP 400,
//! `ValidationException`, and the constraint envelope
//! `1 validation error detected: Value at '<member>' failed to satisfy constraint:
//! Member must have <bound>`. The member path and the bound are the model's; the value
//! the request carried is NOT part of the sentence, so the refusal reads the same
//! whatever was submitted and never reflects a caller's own input back at it. The one
//! other constraint class this service's inputs carry, an enumerated type's value set,
//! is the same rule and lives in `probe_tmp.rs`.
//!
//! The rows span the class rather than one member: every bounded member of every
//! operation of this service, each at one past its floor and one past its ceiling, and
//! over values that differ in length, in form (a number, a token, an ARN, a shard id, a
//! sequence number) and in sign. Each bound is also exercised AT both published ends,
//! where the rule requires the bound NOT to refuse, and beside the neighbouring classes
//! the rule must not swallow: an absent required member, which keeps the `null` an
//! absent member is stated with, and a request inside every bound, which is served.
//!
//! The modelled bounds the clauses state are named once below, from
//! `smithy_query(query="operation",
//! operation="GetRecords,GetShardIterator,DescribeStream,ListStreams")`.
//!
//! Every row goes over the wire (`POST /` +
//! `X-Amz-Target: DynamoDBStreams_20120810.<Op>`), so nothing is asserted that a client
//! cannot observe.

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

/// The `@length` window the model declares on `ShardIterator`, which `GetRecords`
/// carries.
const SHARD_ITERATOR_LENGTH: (usize, usize) = (1, 2048);

/// The `@length` window the model declares on `StreamArn`, which `GetShardIterator`,
/// `DescribeStream` and `ListStreams$ExclusiveStartStreamArn` all carry.
const STREAM_ARN_LENGTH: (usize, usize) = (37, 1024);

/// The `@length` window the model declares on `ShardId`, which `GetShardIterator` and
/// `DescribeStream$ExclusiveStartShardId` carry.
const SHARD_ID_LENGTH: (usize, usize) = (28, 65);

/// The `@length` window the model declares on `SequenceNumber`, which
/// `GetShardIterator` carries.
const SEQUENCE_NUMBER_LENGTH: (usize, usize) = (21, 40);

/// The `@range` floor the model declares on `PositiveIntegerObject`, which every
/// `Limit` of this service targets.
const POSITIVE_INTEGER_FLOOR: i64 = 1;

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

/// The whole message a value outside one modelled bound is refused with: the envelope,
/// the member the bound sits on, and the bound itself.
fn bound_refusal(member: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement clause for a value shorter than its member's `@length` minimum.
fn min_length(min: usize) -> String {
    format!("Member must have length greater than or equal to {min}")
}

/// The requirement clause for a value longer than its member's `@length` maximum.
fn max_length(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The requirement clause for a number below its member's `@range` minimum.
fn min_value(min: i64) -> String {
    format!("Member must have value greater than or equal to {min}")
}

/// A string of exactly `len` characters, opening on `seed` and padded out with a
/// character the seed's own form carries — so a value is refused for its LENGTH and the
/// row says nothing about any other rule.
fn sized(seed: &str, len: usize) -> String {
    let mut text: String = seed.chars().take(len).collect();
    while text.chars().count() < len {
        text.push('0');
    }
    text
}

/// An instant `seconds` before now, so a registered stream's asynchronous enable is
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

/// A shard iterator over the registered stream's only shard.
async fn iterator(router: &MultiServiceRouter, stream_arn: &str, shard_id: &str) -> String {
    let (status, body) = call(
        router,
        "GetShardIterator",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    assert_eq!(status, 200, "GetShardIterator answered {status}: {body}");
    body["ShardIterator"]
        .as_str()
        .unwrap_or_else(|| panic!("no ShardIterator in {body}"))
        .to_string()
}

/// Assert one refused row: HTTP 400, `ValidationException`, the whole envelope the rule
/// requires, and no value from the request anywhere inside it.
fn assert_bound_refusal(
    case: &str,
    status: u16,
    body: &serde_json::Value,
    member: &str,
    requirement: &str,
) {
    let expected = bound_refusal(member, requirement);
    assert_eq!(
        status, 400,
        "case {case:?}: a value outside a modelled bound must be refused with 400, got \
         {status} and {body}"
    );
    assert_eq!(
        error_code(body),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {body}"
    );
    let stated = message(body);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected {expected:?}, got {stated:?}"
    );
    // The quoted-value half of a clause, which this class of refusal does not carry
    // whatever the request submitted.
    assert!(
        !stated.contains("Value '"),
        "case {case:?}: the clause must quote no submitted value, got {stated:?}"
    );
}

/// Assert one row sitting AT a published bound: whatever else the request draws, the
/// bound itself does not refuse it.
fn assert_not_a_bound_refusal(
    case: &str,
    body: &serde_json::Value,
    member: &str,
    requirement: &str,
) {
    let refusal = bound_refusal(member, requirement);
    let stated = message(body);
    assert_ne!(
        stated, refusal,
        "case {case:?}: a value at the bound must not be refused by it, got {stated:?}"
    );
}

/// Every `Limit` of this service targets one `@range`-floored type, so a value below
/// that floor draws the floor and nothing of the request — whichever operation carried
/// it, and however far below the floor the value sat.
#[tokio::test]
async fn a_limit_below_its_modelled_floor_states_the_floor_and_no_value() {
    let (engine, router) = build();
    let stream_arn = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &stream_arn).await;
    let token = iterator(&router, &stream_arn, &shard_id).await;

    // The rows differ along how far below the floor the value sits and its sign: the
    // floor's own neighbour, zero, a negative value, and the widest the member carries.
    let below: [(&str, i64); 4] = [
        ("one below the floor", POSITIVE_INTEGER_FLOOR - 1),
        ("zero", 0),
        ("a negative limit", -1),
        ("the narrowest the member carries", i64::from(i32::MIN)),
    ];

    for (how, limit) in below {
        let operations: [(&str, serde_json::Value); 3] = [
            (
                "GetRecords",
                serde_json::json!({ "ShardIterator": token, "Limit": limit }),
            ),
            (
                "DescribeStream",
                serde_json::json!({ "StreamArn": stream_arn, "Limit": limit }),
            ),
            ("ListStreams", serde_json::json!({ "Limit": limit })),
        ];
        for (operation, body) in operations {
            let case = format!("{operation} with {how}");
            let (status, answered) = call(&router, operation, body).await;
            assert_bound_refusal(
                &case,
                status,
                &answered,
                "limit",
                &min_value(POSITIVE_INTEGER_FLOOR),
            );
        }
    }

    // The floor itself is served by every one of them, so the clause above is about the
    // bound and not about the member being present.
    let served: [(&str, serde_json::Value); 3] = [
        (
            "GetRecords",
            serde_json::json!({ "ShardIterator": token, "Limit": POSITIVE_INTEGER_FLOOR }),
        ),
        (
            "DescribeStream",
            serde_json::json!({ "StreamArn": stream_arn, "Limit": POSITIVE_INTEGER_FLOOR }),
        ),
        (
            "ListStreams",
            serde_json::json!({ "Limit": POSITIVE_INTEGER_FLOOR }),
        ),
    ];
    for (operation, body) in served {
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation} at the floor: a Limit at the floor must be served, got \
             {status} and {answered}"
        );
    }
}

/// Every member the model gives a `@length` window states that window and nothing of the
/// request when a value misses it — one past either end — and does not refuse a value
/// sitting at either end.
#[tokio::test]
async fn a_value_outside_a_modelled_length_window_states_the_window_and_no_value() {
    let (engine, router) = build();
    let stream_arn = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &stream_arn).await;

    // (operation, member path, the window, a value of the right form to size)
    let members: [(&str, &str, (usize, usize), &str); 7] = [
        (
            "GetRecords",
            "shardIterator",
            SHARD_ITERATOR_LENGTH,
            "iterator-token",
        ),
        (
            "GetShardIterator",
            "streamArn",
            STREAM_ARN_LENGTH,
            stream_arn.as_str(),
        ),
        (
            "GetShardIterator",
            "shardId",
            SHARD_ID_LENGTH,
            shard_id.as_str(),
        ),
        (
            "GetShardIterator",
            "sequenceNumber",
            SEQUENCE_NUMBER_LENGTH,
            "1000000000000000000000",
        ),
        (
            "DescribeStream",
            "streamArn",
            STREAM_ARN_LENGTH,
            stream_arn.as_str(),
        ),
        (
            "DescribeStream",
            "exclusiveStartShardId",
            SHARD_ID_LENGTH,
            shard_id.as_str(),
        ),
        (
            "ListStreams",
            "exclusiveStartStreamArn",
            STREAM_ARN_LENGTH,
            stream_arn.as_str(),
        ),
    ];

    for (operation, member, (min, max), seed) in members {
        // Each window is exercised at both published ends and one past each of them.
        let lengths: [(&str, usize, Option<String>); 4] = [
            ("one short of the floor", min - 1, Some(min_length(min))),
            ("at the floor", min, None),
            ("at the ceiling", max, None),
            ("one past the ceiling", max + 1, Some(max_length(max))),
        ];
        for (how, len, requirement) in lengths {
            let case = format!("{operation}${member} {how} ({len} characters)");
            let value = sized(seed, len);
            let body = request_with(operation, member, &value, &stream_arn, &shard_id);
            let (status, answered) = call(&router, operation, body).await;
            match requirement {
                Some(requirement) => {
                    assert_bound_refusal(&case, status, &answered, member, &requirement);
                }
                None => {
                    // A value at either end of the window may still be refused for what
                    // it NAMES — a stream or shard that does not exist, a token that
                    // decodes to nothing — but never for the length it has.
                    assert_not_a_bound_refusal(&case, &answered, member, &min_length(min));
                    assert_not_a_bound_refusal(&case, &answered, member, &max_length(max));
                }
            }
        }
    }
}

/// The request that carries `value` at `member`, with every other member of `operation`
/// filled from the registered stream so nothing else in it can be refused.
fn request_with(
    operation: &str,
    member: &str,
    value: &str,
    stream_arn: &str,
    shard_id: &str,
) -> serde_json::Value {
    match (operation, member) {
        ("GetRecords", _) => serde_json::json!({ "ShardIterator": value }),
        ("GetShardIterator", "streamArn") => serde_json::json!({
            "StreamArn": value,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
        ("GetShardIterator", "shardId") => serde_json::json!({
            "StreamArn": stream_arn,
            "ShardId": value,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
        ("GetShardIterator", _) => serde_json::json!({
            "StreamArn": stream_arn,
            "ShardId": shard_id,
            "ShardIteratorType": "AT_SEQUENCE_NUMBER",
            "SequenceNumber": value,
        }),
        ("DescribeStream", "exclusiveStartShardId") => serde_json::json!({
            "StreamArn": stream_arn,
            "ExclusiveStartShardId": value,
        }),
        ("DescribeStream", _) => serde_json::json!({ "StreamArn": value }),
        (_, _) => serde_json::json!({ "ExclusiveStartStreamArn": value }),
    }
}

/// The neighbouring class the rule must not swallow: a member that was not in the
/// request at all has no value to withhold, so it keeps the `null` an absent member is
/// stated with — and a request inside every bound is served.
///
/// The rows are the required members this service does NOT address a position by. A
/// request that omits the identifier it addresses — a stream ARN, a shard iterator —
/// draws that identifier's own fixed sentence instead, which is pinned over the whole
/// class in `identifier_shape_refusals.rs`.
#[tokio::test]
async fn an_absent_required_member_keeps_the_null_clause() {
    let (engine, router) = build();
    let stream_arn = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &stream_arn).await;

    // (case, operation, the request, the member the walk reports absent)
    let absent: [(&str, &str, serde_json::Value, &str); 2] = [
        (
            "GetShardIterator without its shard",
            "GetShardIterator",
            serde_json::json!({ "StreamArn": stream_arn, "ShardIteratorType": "LATEST" }),
            "shardId",
        ),
        (
            "GetShardIterator without its iterator type",
            "GetShardIterator",
            serde_json::json!({ "StreamArn": stream_arn, "ShardId": shard_id }),
            "shardIteratorType",
        ),
    ];

    for (case, operation, body, member) in absent {
        let (status, answered) = call(&router, operation, body).await;
        let expected = format!(
            "1 validation error detected: Value null at '{member}' failed to satisfy \
             constraint: Member must not be null"
        );
        assert_eq!(
            status, 400,
            "case {case:?}: an absent required member must be refused with 400, got \
             {status} and {answered}"
        );
        let stated = message(&answered);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // And a request every bound admits is served, so none of the above is the whole
    // operation refusing its input.
    let token = iterator(&router, &stream_arn, &shard_id).await;
    let (status, answered) = call(
        &router,
        "GetRecords",
        serde_json::json!({ "ShardIterator": token, "Limit": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a request inside every bound: expected it to be served, got {status} and \
         {answered}"
    );
}
