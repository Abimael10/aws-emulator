// SPDX-License-Identifier: CC0-1.0

//! The SHAPE of a caller-supplied identifier is judged on the input, before the
//! registry is consulted at all.
//!
//! Two rules are pinned here, and the second half of each is what makes them rules
//! rather than two more constants:
//!
//! * a member that carries a stream ARN (`DescribeStream$StreamArn`,
//!   `GetShardIterator$StreamArn`, `ListStreams$ExclusiveStartStreamArn` — every
//!   member of this service's inputs targeting `StreamArn`) is refused as INVALID
//!   INPUT — HTTP 400, `ValidationException`, the fixed sentence `Invalid StreamArn`
//!   — when the value is not written as a stream's ARN, while a value that IS so
//!   written is resolved and may legitimately be absent
//!   (`ResourceNotFoundException`) or present (200); a request that omits the
//!   identifier it addresses — a stream ARN, a shard iterator — names no position
//!   either, so it draws that same sentence rather than the shared constraint
//!   envelope for a null member;
//! * a `ListStreams$TableName` the model's own constraints do not admit is refused
//!   with one sentence stating the member's modelled length window, with no
//!   constraint envelope around it and nothing of the request inside it, while a name
//!   the constraints admit is served.
//!
//! The rows span what each rule generalises over. For the ARN shape: which field of
//! the ARN is wrong (the literal prefix, the vendor, the resource type, the stream
//! step, the label), how many fields the value carries, and plain text that is no ARN
//! at all — against well-formed stream ARNs over several partitions, Regions,
//! accounts and table-name lengths. For the table name: how the value misses the
//! member's constraints (too short, too long, or a character the name pattern does
//! not admit, an ARN among them) and names at both ends of the admitted window.
//!
//! Every row goes over the wire (`POST /` +
//! `X-Amz-Target: DynamoDBStreams_20120810.<Op>`), so nothing is asserted that a
//! client cannot observe.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::KeySchemaElement;
use streams::state::{
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, StreamRegistration,
    StreamViewType,
};

/// A SigV4 credential scope naming `dynamodb`, which is how a DynamoDB Streams
/// request is signed. Request authentication is off here, so only its routing signal
/// matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

/// The whole sentence a member carrying a stream ARN is refused with when the value
/// is not written as one.
const INVALID_STREAM_ARN: &str = "Invalid StreamArn";

/// The whole sentence the member carrying a shard iterator is refused with when the
/// value names no position this service can read from.
const INVALID_SHARD_ITERATOR: &str = "Invalid ShardIterator";

/// The whole sentence a `TableName` the model's constraints do not admit is refused
/// with. The two figures are the member's own modelled bounds — `TableName` is
/// `@length(min: 3, max: 255)`, from
/// `smithy_query(query="shape", shape="TableName")`.
const TABLE_NAME_REFUSAL: &str =
    "TableName must be at least 3 characters long and at most 255 characters long";

/// The modelled `TableName` length bounds the sentence above states, named once so a
/// row sitting on a bound cannot drift from the sentence.
const MIN_TABLE_NAME_LENGTH: usize = 3;
const MAX_TABLE_NAME_LENGTH: usize = 255;

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

/// The wire error code, as an SDK client matches it: the local name of whatever shape
/// id the envelope carries.
fn error_code(body: &serde_json::Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

fn error_message(body: &serde_json::Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
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

/// Values that are NOT written as a stream's ARN, each inside the modelled
/// `StreamArn` length window of 37..1024 characters so every row reaches a handler
/// rather than the length bound. Each row says which part of the form it breaks.
fn malformed_stream_arns() -> Vec<(&'static str, String)> {
    vec![
        (
            "a table's own ARN, with no stream step at all",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/ledger"),
        ),
        (
            "a table ARN whose stream step carries no label",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/ledger/stream/"),
        ),
        (
            "a stream step with no table name before it",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table//stream/2026-01-01T00:00:00.000"),
        ),
        (
            "a label carrying a further path step",
            format!(
                "arn:aws:dynamodb:{REGION}:000000000000:table/ledger/stream/\
                 2026-01-01T00:00:00.000/extra"
            ),
        ),
        (
            "an index of the table rather than a stream of it",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/ledger/index/by-owner-00000"),
        ),
        (
            "a backup of the table rather than a stream of it",
            format!("arn:aws:dynamodb:{REGION}:000000000000:backup/ledger/stream/00000000000"),
        ),
        (
            "another vendor's ARN in the stream ARN's own form",
            format!("arn:aws:kinesis:{REGION}:000000000000:table/ledger/stream/2026-01-01T00:00:00"),
        ),
        (
            "the vendor field in another casing",
            format!("arn:aws:DynamoDB:{REGION}:000000000000:table/ledger/stream/2026-01-01T00:00:00"),
        ),
        (
            "too few fields to be an ARN",
            format!("arn:aws:dynamodb:{REGION}:table/ledger/stream/2026-01-01T00:00:00.000"),
        ),
        (
            "an empty partition field",
            format!("arn::dynamodb:{REGION}:000000000000:table/ledger/stream/2026-01-01T00:00:00"),
        ),
        (
            "the leading literal misspelled",
            format!("urn:aws:dynamodb:{REGION}:000000000000:table/ledger/stream/2026-01-01T00:00:00"),
        ),
        (
            "plain text that is no ARN at all",
            "ledger-stream-0000000000000000000000000".to_string(),
        ),
        (
            "a URL rather than an ARN",
            format!("https://dynamodb.{REGION}.amazonaws.com/ledger/stream/2026-01-01T00:00:00.0"),
        ),
    ]
}

/// Well-formed stream ARNs no stream is registered under: the neighbouring class the
/// rule must NOT swallow. Spans the partition, the Region, the account and the table
/// name's length.
fn absent_stream_arns() -> Vec<(&'static str, String)> {
    vec![
        (
            "this Region, a table that was never created",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/absent/stream/2026-01-01T00:00:00.000"),
        ),
        (
            "another Region",
            "arn:aws:dynamodb:ap-south-1:000000000000:table/ledger/stream/2026-01-01T00:00:00.000"
                .to_string(),
        ),
        (
            "another partition",
            "arn:aws-cn:dynamodb:cn-north-1:000000000000:table/ledger/stream/2026-01-01T00:00:00.000"
                .to_string(),
        ),
        (
            "another account",
            format!("arn:aws:dynamodb:{REGION}:111122223333:table/ledger/stream/2026-01-01T00:00:00.000"),
        ),
        (
            "a table name at the longest the name bound allows",
            format!(
                "arn:aws:dynamodb:{REGION}:000000000000:table/{}/stream/2026-01-01T00:00:00.000",
                "t".repeat(MAX_TABLE_NAME_LENGTH)
            ),
        ),
    ]
}

/// Assert one refused row: HTTP 400, `ValidationException`, the fixed sentence, and
/// nothing of the request reflected back.
fn assert_invalid_stream_arn(case: &str, submitted: &str, status: u16, body: &serde_json::Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: a malformed StreamArn must be refused with 400, got {status} and {body}"
    );
    assert_eq!(
        error_code(body),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {body}"
    );
    let message = error_message(body);
    assert_eq!(
        message, INVALID_STREAM_ARN,
        "case {case:?}: expected {INVALID_STREAM_ARN:?}, got {message:?}"
    );
    assert!(
        !message.contains(submitted),
        "case {case:?}: the refusal must not reproduce the submitted value, got {message:?}"
    );
}

/// `DescribeStream` refuses a `StreamArn` that is not written as a stream's ARN as
/// invalid input, and keeps `ResourceNotFoundException` for one that is written as a
/// stream's ARN but names no stream.
#[tokio::test]
async fn describe_stream_separates_a_malformed_arn_from_an_absent_stream() {
    let (engine, router) = build();
    let registered = register(&engine, "ledger", seconds_ago(3_600));

    for (case, submitted) in malformed_stream_arns() {
        let (status, body) = call(
            &router,
            "DescribeStream",
            serde_json::json!({ "StreamArn": submitted }),
        )
        .await;
        assert_invalid_stream_arn(case, &submitted, status, &body);
    }

    for (case, submitted) in absent_stream_arns() {
        let (status, body) = call(
            &router,
            "DescribeStream",
            serde_json::json!({ "StreamArn": submitted }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an absent stream is still refused with 400, got {status} and {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: a well-formed ARN naming no stream must stay a not-found, got {body}"
        );
    }

    // And the ARN of a stream that IS registered is still served, so the shape check
    // refuses nothing the registry can answer.
    let (status, body) = call(
        &router,
        "DescribeStream",
        serde_json::json!({ "StreamArn": registered }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the registered stream: DescribeStream must serve it, got {status} and {body}"
    );
    assert_eq!(
        body["StreamDescription"]["StreamArn"].as_str(),
        Some(registered.as_str()),
        "case the registered stream: the description must name it, got {body}"
    );
}

/// `GetShardIterator` applies the same shape check to its own `StreamArn`, before any
/// stream or shard is resolved — so a malformed ARN is refused whatever shard id or
/// iterator type the request carries.
#[tokio::test]
async fn get_shard_iterator_separates_a_malformed_arn_from_an_absent_stream() {
    let (engine, router) = build();
    let registered = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &registered).await;
    // A shard id no stream has, inside the modelled `ShardId` bound of 28..65.
    let absent_shard = "shardId-00000000000000000000";

    // The iterator types are the model's own `ShardIteratorType` values that need no
    // further member, so a row's refusal cannot come from a missing one.
    for (case, submitted) in malformed_stream_arns() {
        for iterator_type in ["TRIM_HORIZON", "LATEST"] {
            for shard in [shard_id.as_str(), absent_shard] {
                let case = format!("{case} ({iterator_type}, shard {shard})");
                let (status, body) = call(
                    &router,
                    "GetShardIterator",
                    serde_json::json!({
                        "StreamArn": submitted,
                        "ShardId": shard,
                        "ShardIteratorType": iterator_type,
                    }),
                )
                .await;
                assert_invalid_stream_arn(&case, &submitted, status, &body);
            }
        }
    }

    for (case, submitted) in absent_stream_arns() {
        let (status, body) = call(
            &router,
            "GetShardIterator",
            serde_json::json!({
                "StreamArn": submitted,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an absent stream is still refused with 400, got {status} and {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: a well-formed ARN naming no stream must stay a not-found, got {body}"
        );
    }

    // The registered stream's own shard still hands out an iterator.
    let (status, body) = call(
        &router,
        "GetShardIterator",
        serde_json::json!({
            "StreamArn": registered,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the registered stream: GetShardIterator must serve it, got {status} and {body}"
    );
    assert!(
        body["ShardIterator"].is_string(),
        "case the registered stream: expected a minted iterator, got {body}"
    );
}

/// `ListStreams` carries the same `StreamArn` shape in its page boundary, so the same
/// refusal holds there — while a boundary that IS written as a stream's ARN pages the
/// enumeration whether or not a stream sits at it.
#[tokio::test]
async fn list_streams_refuses_a_page_boundary_that_is_no_stream_arn() {
    let (engine, router) = build();
    let registered = register(&engine, "ledger", seconds_ago(3_600));

    for (case, submitted) in malformed_stream_arns() {
        let (status, body) = call(
            &router,
            "ListStreams",
            serde_json::json!({ "ExclusiveStartStreamArn": submitted }),
        )
        .await;
        assert_invalid_stream_arn(case, &submitted, status, &body);
    }

    // A well-formed boundary is honoured: one that sorts before the registered
    // stream's ARN leaves it in the page, one that sorts after it excludes it, and
    // neither has to name a stream that exists.
    let (status, body) = call(
        &router,
        "ListStreams",
        serde_json::json!({
            "ExclusiveStartStreamArn":
                format!("arn:aws:dynamodb:{REGION}:000000000000:table/aaa/stream/2026-01-01T00:00:00.000"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a boundary before the registered stream: expected 200, got {status} and {body}"
    );
    let arns: Vec<&str> = body["Streams"]
        .as_array()
        .unwrap_or_else(|| panic!("no Streams in {body}"))
        .iter()
        .map(|entry| entry["StreamArn"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        arns,
        vec![registered.as_str()],
        "case a boundary before the registered stream: expected it in the page, got {arns:?}"
    );

    let (status, body) = call(
        &router,
        "ListStreams",
        serde_json::json!({
            "ExclusiveStartStreamArn":
                format!("arn:aws:dynamodb:{REGION}:000000000000:table/zzz/stream/2026-01-01T00:00:00.000"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a boundary after the registered stream: expected 200, got {status} and {body}"
    );
    assert_eq!(
        body["Streams"].as_array().map(Vec::len),
        Some(0),
        "case a boundary after the registered stream: expected an empty page, got {body}"
    );
}

/// A `ListStreams` `TableName` the model's own constraints do not admit draws ONE
/// sentence stating the member's length window — never the shared constraint
/// envelope, and never with the submitted value spliced into it.
#[tokio::test]
async fn list_streams_refuses_an_inadmissible_table_name_with_the_length_sentence() {
    let (engine, router) = build();
    register(&engine, "ledger", seconds_ago(3_600));

    // Each row is a value the member's modelled constraints reject, and they differ
    // along HOW they miss them: below the length floor, above its ceiling, or outside
    // the character class `^[a-zA-Z0-9_.-]+$` the name pattern admits.
    let refused: Vec<(&str, String)> = vec![
        (
            "a table ARN where a table name belongs",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/ledger"),
        ),
        (
            "a stream ARN where a table name belongs",
            format!("arn:aws:dynamodb:{REGION}:000000000000:table/ledger/stream/2026-01-01T00:00:00.000"),
        ),
        ("the empty name", String::new()),
        ("one character, below the floor", "a".to_string()),
        (
            "one short of the floor",
            "a".repeat(MIN_TABLE_NAME_LENGTH - 1),
        ),
        (
            "one past the ceiling",
            "t".repeat(MAX_TABLE_NAME_LENGTH + 1),
        ),
        (
            "far past the ceiling",
            "t".repeat(MAX_TABLE_NAME_LENGTH * 4),
        ),
        ("a name carrying a space", "my ledger".to_string()),
        ("a name carrying a slash", "ledger/live".to_string()),
        ("a name carrying a colon", "ledger:live".to_string()),
        ("a name carrying a wildcard", "ledger*".to_string()),
        ("a name of punctuation only", "***".to_string()),
    ];

    for (case, submitted) in refused {
        let (status, body) = call(
            &router,
            "ListStreams",
            serde_json::json!({ "TableName": submitted }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: an inadmissible TableName must be refused with 400, got {status} \
             and {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, TABLE_NAME_REFUSAL,
            "case {case:?}: expected {TABLE_NAME_REFUSAL:?}, got {message:?}"
        );
        // A value shorter than the window's floor is a substring of ordinary English,
        // so only a value long enough for the check to mean anything is asserted on.
        assert!(
            submitted.len() < MIN_TABLE_NAME_LENGTH || !message.contains(&submitted),
            "case {case:?}: the refusal must not reproduce the submitted value, got {message:?}"
        );
        assert!(
            !message.contains("validation error"),
            "case {case:?}: this member does not use the shared constraint envelope, got \
             {message:?}"
        );
    }

    // The neighbouring class: a name the constraints DO admit is served, at both ends
    // of the window and over every character class the pattern allows. None of them
    // has a stream, so each is an empty page rather than an error.
    let admitted: Vec<(&str, String)> = vec![
        (
            "the shortest name the window allows",
            "a".repeat(MIN_TABLE_NAME_LENGTH),
        ),
        (
            "the longest name the window allows",
            "t".repeat(MAX_TABLE_NAME_LENGTH),
        ),
        (
            "every character class the pattern allows",
            "Ledger_2026.v1-BETA".to_string(),
        ),
    ];

    for (case, submitted) in admitted {
        let (status, body) = call(
            &router,
            "ListStreams",
            serde_json::json!({ "TableName": submitted }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an admitted TableName must be served, got {status} and {body}"
        );
        assert_eq!(
            body["Streams"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: no stream is registered for it, so the page is empty, got {body}"
        );
    }

    // And the table that DOES have a stream still reports it, so the refusals above
    // are about the value's form and not about the filter.
    let (status, body) = call(
        &router,
        "ListStreams",
        serde_json::json!({ "TableName": "ledger" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the registered table: ListStreams must serve it, got {status} and {body}"
    );
    assert_eq!(
        body["Streams"].as_array().map(Vec::len),
        Some(1),
        "case the registered table: expected its one stream, got {body}"
    );
}

/// A request that OMITS the identifier it addresses names no position either, so it
/// draws that identifier's OWN fixed sentence — the same one a value of the wrong form
/// draws — and never the shared constraint envelope for a null member.
///
/// The rows span what the rule generalises over: which identifier the request left out
/// (a stream ARN, a shard iterator), which operation carried it, and what else the
/// request carried — nothing, the operation's other members, and a second member that
/// is ITSELF outside its bounds, which the identifier is still judged before.
#[tokio::test]
async fn an_omitted_addressing_identifier_draws_the_identifiers_own_sentence() {
    let (engine, router) = build();
    let registered = register(&engine, "ledger", seconds_ago(3_600));
    let shard_id = only_shard_id(&router, &registered).await;

    // (case, operation, the request, the sentence the rule requires)
    let rows: Vec<(String, &str, serde_json::Value, &str)> = vec![
        (
            "DescribeStream with no member at all".to_string(),
            "DescribeStream",
            serde_json::json!({}),
            INVALID_STREAM_ARN,
        ),
        (
            "DescribeStream with only a page width".to_string(),
            "DescribeStream",
            serde_json::json!({ "Limit": 1 }),
            INVALID_STREAM_ARN,
        ),
        (
            "DescribeStream with a page width below its own floor".to_string(),
            "DescribeStream",
            serde_json::json!({ "Limit": 0 }),
            INVALID_STREAM_ARN,
        ),
        (
            "DescribeStream with a resume point but no stream".to_string(),
            "DescribeStream",
            serde_json::json!({ "ExclusiveStartShardId": shard_id }),
            INVALID_STREAM_ARN,
        ),
        (
            "GetShardIterator with every other member but no stream".to_string(),
            "GetShardIterator",
            serde_json::json!({ "ShardId": shard_id, "ShardIteratorType": "LATEST" }),
            INVALID_STREAM_ARN,
        ),
        (
            "GetShardIterator with no member at all".to_string(),
            "GetShardIterator",
            serde_json::json!({}),
            INVALID_STREAM_ARN,
        ),
        (
            "GetRecords with no member at all".to_string(),
            "GetRecords",
            serde_json::json!({}),
            INVALID_SHARD_ITERATOR,
        ),
        (
            "GetRecords with only a page width".to_string(),
            "GetRecords",
            serde_json::json!({ "Limit": 10 }),
            INVALID_SHARD_ITERATOR,
        ),
        (
            "GetRecords with a page width above its own ceiling".to_string(),
            "GetRecords",
            serde_json::json!({ "Limit": 100_000 }),
            INVALID_SHARD_ITERATOR,
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a request naming no position must be refused with 400, got \
             {status} and {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {answered}"
        );
        let message = error_message(&answered);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains("validation error"),
            "case {case:?}: this refusal is the identifier's own sentence, not the shared \
             constraint envelope, got {message:?}"
        );
    }

    // The neighbouring classes the rule must not swallow. A required member this
    // service states no sentence of its own for keeps the shared envelope: nothing
    // about the identifiers above generalises to every absent member.
    let (status, answered) = call(
        &router,
        "GetShardIterator",
        serde_json::json!({ "StreamArn": registered, "ShardId": shard_id }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case an absent iterator type: expected 400, got {status} and {answered}"
    );
    let message = error_message(&answered);
    assert_eq!(
        message,
        "1 validation error detected: Value null at 'shardIteratorType' failed to satisfy \
         constraint: Member must not be null",
        "case an absent iterator type: expected the shared constraint envelope, got {message:?}"
    );

    // And an OPTIONAL member carrying one of those identifiers may legitimately be
    // absent: `ListStreams` with no page boundary enumerates from the start.
    let (status, answered) = call(&router, "ListStreams", serde_json::json!({})).await;
    assert_eq!(
        status, 200,
        "case no page boundary at all: an optional identifier may be absent, got {status} \
         and {answered}"
    );
}
