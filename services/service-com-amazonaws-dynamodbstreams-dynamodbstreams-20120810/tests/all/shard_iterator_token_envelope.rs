// SPDX-License-Identifier: CC0-1.0

//! The SHAPE of every shard-iterator token this service hands out, read off the wire.
//!
//! A DynamoDB Streams shard iterator is `<StreamArn>|<format version>|<envelope>`,
//! and the third segment is a base64 envelope that opens with a fixed preamble,
//! declares the length of the payload that follows it, and runs to the order of
//! magnitude DynamoDB Streams' own tokens run to (roughly 950 base64 characters) —
//! which is the part of the token a client or proxy can observe and check while
//! budgeting against the modelled `ShardIterator` maximum. The payload BYTES are
//! opaque and nothing here asserts them.
//!
//! The rule holds for EVERY mint, so the rows below span what the token is minted
//! from: `GetShardIterator` and `GetRecords`' `NextShardIterator`, streams whose ARNs
//! are as short and as long as a table name makes them, every `ShardIteratorType`
//! that needs no extra member, and shards holding no records, one record and several.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::{AttributeValue, KeySchemaElement};
use streams::state::{
    AttributeMap, ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState,
    OperationType, RecordAppend, StreamRegistration, StreamViewType,
};

use crate::service_enforced_parameter_bounds::last_character_changed;

/// A SigV4 credential scope naming `dynamodb`, which is how both DynamoDB and
/// DynamoDB Streams requests are signed. Request authentication is off in these
/// tests, so only the scope's routing signal matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

/// The `@length` window the model declares on the `ShardIterator` shape, which both
/// `GetShardIterator$ShardIterator` and `GetRecords$NextShardIterator` target.
/// Source: `smithy_query(query="shape", shape="ShardIterator")`.
const SHARD_ITERATOR_LENGTH: std::ops::RangeInclusive<usize> = 1..=2048;

/// The base64 preamble the opaque segment of a DynamoDB Streams iterator opens with.
/// Recorded behaviour: the model says nothing about the segment's internals, and
/// every iterator the real service hands out begins its third segment here.
const ENVELOPE_PREAMBLE: &str = "AAAAAAAAAAH";

/// How many envelope bytes come before the payload: the fixed preamble, then the
/// `u16` length of the payload that follows it.
const ENVELOPE_HEADER_BYTES: usize = 11;

/// The window the recorded envelopes' base64 length falls in — "roughly 950
/// characters" to the order of magnitude, which is what a client budgeting a token
/// against [`SHARD_ITERATOR_LENGTH`] observes. Asserted as a window, not a figure:
/// the exact count is not a requirement, its scale is.
const ENVELOPE_BASE64_LENGTH: std::ops::RangeInclusive<usize> = 800..=1100;

/// The `ShardIteratorType` values that position a cursor from nothing but the shard
/// itself, so one table can mint from each without carrying a sequence number or a
/// timestamp. Source: the modelled `ShardIteratorType` enum
/// (`smithy_query(query="shape", shape="ShardIteratorType")`).
const SELF_CONTAINED_ITERATOR_TYPES: [&str; 2] = ["TRIM_HORIZON", "LATEST"];

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

/// A whole-second instant `seconds` before now, far enough back that a stream
/// registered then has finished enabling and serves its shard.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs();
    UNIX_EPOCH + Duration::from_secs(now - seconds)
}

fn register(
    engine: &engine::InMemoryEngine,
    table_name: &str,
    stream_view_type: StreamViewType,
    at: SystemTime,
) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type,
                key_schema: vec![KeySchemaElement {
                    attribute_name: "Id".to_string(),
                    key_type: "HASH".to_string(),
                }],
                region: REGION.to_string(),
            },
            at,
        )
        .expect("the registry accepts the registration")
}

fn image_of(id: &str) -> AttributeMap {
    HashMap::from([("Id".to_string(), AttributeValue::S(id.to_string()))])
}

fn append(engine: &engine::InMemoryEngine, stream_arn: &str, id: &str, at: SystemTime) {
    engine
        .state::<StreamsState>()
        .lock()
        .append_record(
            stream_arn,
            &RecordAppend {
                event_name: OperationType::Insert,
                keys: image_of(id),
                new_image: Some(image_of(id)),
                old_image: None,
            },
            at,
        )
        .expect("the stream is registered and has an open shard");
}

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

/// Everything the token layout requires of ONE minted iterator.
fn assert_iterator_envelope(case: &str, stream_arn: &str, shard_id: &str, token: &str) {
    assert!(
        SHARD_ITERATOR_LENGTH.contains(&token.len()),
        "case {case}: ShardIterator is modelled @length {}..={}, got {} characters",
        SHARD_ITERATOR_LENGTH.start(),
        SHARD_ITERATOR_LENGTH.end(),
        token.len()
    );
    let segments: Vec<&str> = token.split('|').collect();
    assert_eq!(
        segments.len(),
        3,
        "case {case}: an iterator is <StreamArn>|<version>|<envelope>, got {} segments in {token}",
        segments.len()
    );
    assert_eq!(
        segments[0], stream_arn,
        "case {case}: the token must name the stream it belongs to, got {}",
        segments[0]
    );
    assert_eq!(
        segments[1], "1",
        "case {case}: the token must carry its format version, got {}",
        segments[1]
    );

    let envelope = segments[2];
    assert!(
        envelope.starts_with(ENVELOPE_PREAMBLE),
        "case {case}: the opaque segment must open with {ENVELOPE_PREAMBLE}, got {}",
        &envelope[..envelope.len().min(ENVELOPE_PREAMBLE.len())]
    );
    assert!(
        ENVELOPE_BASE64_LENGTH.contains(&envelope.len()),
        "case {case}: the opaque segment must be a base64 envelope of {}..={} characters, got {}",
        ENVELOPE_BASE64_LENGTH.start(),
        ENVELOPE_BASE64_LENGTH.end(),
        envelope.len()
    );
    // Opaque, not an encoding of the request: the position the iterator stands at
    // lives in the service's own iterator table, so neither the shard nor the offset
    // is readable off the token.
    assert!(
        !envelope.contains(shard_id),
        "case {case}: the opaque segment must not echo the shard id, got {envelope}"
    );

    let decoded = ::aws_smithy_types::base64::decode(envelope)
        .unwrap_or_else(|error| panic!("case {case}: the opaque segment must be base64: {error}"));
    assert!(
        decoded.len() > ENVELOPE_HEADER_BYTES,
        "case {case}: the envelope must carry a payload after its {ENVELOPE_HEADER_BYTES}-byte \
         header, got {} bytes in all",
        decoded.len()
    );
    let declared = usize::from(u16::from_be_bytes([decoded[9], decoded[10]]));
    assert_eq!(
        decoded.len() - ENVELOPE_HEADER_BYTES,
        declared,
        "case {case}: the envelope must declare its own payload length, declared {declared} \
         bytes and carries {}",
        decoded.len() - ENVELOPE_HEADER_BYTES
    );
}

/// Every iterator minted on either operation carries the recorded envelope, whatever
/// the stream, the start point or how much the shard holds — and every one of them is
/// a token `GetRecords` resolves.
#[tokio::test]
async fn every_minted_iterator_carries_the_recorded_envelope() {
    let (engine, router) = build();
    let base = seconds_ago(600);

    // Rows differ along what the rule generalises over: the length of the stream ARN
    // the token is prefixed with (a one-character table name, an ordinary one, and one
    // at DynamoDB's published 255-character table-name maximum, which is the longest
    // prefix a token can carry), what the stream captures, where the cursor starts,
    // and how many records the shard holds.
    let longest_table_name = "t".repeat(255);
    let cases: [(&str, &str, StreamViewType, u64); 4] = [
        ("the shortest table name", "t", StreamViewType::KeysOnly, 0),
        ("an ordinary table name", "ledger", StreamViewType::NewImage, 1),
        (
            "a stream capturing both images",
            "ledger-two",
            StreamViewType::NewAndOldImages,
            5,
        ),
        (
            "the longest table name",
            longest_table_name.as_str(),
            StreamViewType::OldImage,
            3,
        ),
    ];

    // Two iterators of the same position are still distinct tokens, so every mint in
    // this test must be unique.
    let mut seen: HashSet<String> = HashSet::new();

    for (row, table_name, view_type, records) in cases {
        let stream_arn = register(&engine, table_name, view_type, base);
        for index in 0..records {
            append(
                &engine,
                &stream_arn,
                &format!("item-{index}"),
                base + Duration::from_secs(index),
            );
        }
        let shard_id = only_shard_id(&router, &stream_arn).await;

        for iterator_type in SELF_CONTAINED_ITERATOR_TYPES {
            let case = format!("{row} / {iterator_type} / {records} records");
            let (status, body) = call(
                &router,
                "GetShardIterator",
                serde_json::json!({
                    "StreamArn": stream_arn,
                    "ShardId": shard_id,
                    "ShardIteratorType": iterator_type,
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: GetShardIterator answered {status}: {body}"
            );
            let mut token = body["ShardIterator"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: no ShardIterator in {body}"))
                .to_string();
            assert_iterator_envelope(
                &format!("{case}: GetShardIterator.ShardIterator"),
                &stream_arn,
                &shard_id,
                &token,
            );
            assert!(
                seen.insert(token.clone()),
                "case {case}: GetShardIterator minted a token it had minted before: {token}"
            );

            // Every continuation token a paging reader is handed carries the same
            // envelope, one page at a time over the whole shard.
            for page in 0..=records {
                let (status, body) = call(
                    &router,
                    "GetRecords",
                    serde_json::json!({ "ShardIterator": token, "Limit": 1 }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case}: GetRecords page {page} answered {status}: {body}"
                );
                let next = body["NextShardIterator"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case}: page {page} of an open shard must carry a NextShardIterator, got {body}")
                    })
                    .to_string();
                assert_iterator_envelope(
                    &format!("{case}: GetRecords.NextShardIterator page {page}"),
                    &stream_arn,
                    &shard_id,
                    &next,
                );
                assert!(
                    seen.insert(next.clone()),
                    "case {case}: GetRecords minted a token it had minted before: {next}"
                );
                token = next;
            }
        }
    }
}

/// The envelope is the emitted FORM of a token, not a new identity for it: a token
/// carrying it still resolves to exactly the position it was minted for, and a value
/// that merely looks like one resolves to nothing.
#[tokio::test]
async fn an_enveloped_token_still_resolves_to_the_position_it_was_minted_for() {
    let (engine, router) = build();
    let base = seconds_ago(600);
    let stream_arn = register(&engine, "ledger", StreamViewType::NewImage, base);
    for index in 0..3u64 {
        append(
            &engine,
            &stream_arn,
            &format!("item-{index}"),
            base + Duration::from_secs(index),
        );
    }
    let shard_id = only_shard_id(&router, &stream_arn).await;

    let (status, body) = call(
        &router,
        "GetShardIterator",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    assert_eq!(status, 200, "GetShardIterator answered {status}: {body}");
    let token = body["ShardIterator"]
        .as_str()
        .unwrap_or_else(|| panic!("no ShardIterator in {body}"))
        .to_string();

    // Each row: what is presented, and the HTTP status the rule requires of it.
    let cases: [(&str, String, u16); 4] = [
        ("the token as minted", token.clone(), 200),
        (
            "the envelope with its preamble cut off",
            token.replacen(ENVELOPE_PREAMBLE, "", 1),
            400,
        ),
        (
            "the envelope with one payload character changed",
            last_character_changed(&token),
            400,
        ),
        (
            "an envelope of the right shape minted by nobody",
            {
                let (named, envelope) = token
                    .rsplit_once('|')
                    .expect("a minted token has three segments");
                format!("{named}|{ENVELOPE_PREAMBLE}{}", "A".repeat(envelope.len() - ENVELOPE_PREAMBLE.len()))
            },
            400,
        ),
    ];

    for (case, presented, expected) in cases {
        let (status, body) = call(
            &router,
            "GetRecords",
            serde_json::json!({ "ShardIterator": presented }),
        )
        .await;
        assert_eq!(
            status, expected,
            "case {case}: expected HTTP {expected}, got {status} with {body}"
        );
        if expected == 200 {
            assert_eq!(
                body["Records"].as_array().map(Vec::len),
                Some(3),
                "case {case}: a TRIM_HORIZON iterator delivers the whole shard, got {body}"
            );
        }
    }
}
