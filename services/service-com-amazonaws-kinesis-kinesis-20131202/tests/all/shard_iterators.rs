// SPDX-License-Identifier: CC0-1.0

//! A Kinesis shard ITERATOR, end to end over the wire: `CreateStream` -> `ListShards` ->
//! `GetShardIterator`.
//!
//! RULE: `GetShardIterator` answers an opaque iterator that names the position its
//! `ShardIteratorType` asks for inside one shard of the stream its request addresses, for
//! every type the model declares; it refuses a request whose members its own type does not
//! admit, or that names no position at all, with `InvalidArgumentException`, and one naming a
//! stream this account and region do not hold, a shard the stream does not hold, or a stream
//! that is not `ACTIVE` yet with `ResourceNotFoundException`. Handing an iterator out changes
//! nothing a later caller sees: two calls for the same position answer iterators that name
//! the same shard and position.
//!
//! A sequence number belongs to the SHARD that minted it, so a request that names one on
//! another shard of the same stream is refused with the sentence that names both shards — the
//! one the value is of and the one the call addressed — while the same value on its own shard
//! still names the position it denotes.
//!
//! The recorded flow this was derived from takes ONE iterator of the first shard of a live
//! stream (`get_shard_iterator(StreamName=…, ShardId=…, ShardIteratorType="TRIM_HORIZON")`)
//! and is one row of the tables below. The rows differ along what the rule generalises over:
//! which iterator type names the position, which shard of the map it is taken in, which
//! identifier the stream is resolved by, which member the type reads its position from, and
//! the status the stream is in when the call arrives.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::ops::get_shard_iterator::could_not_find_shard_message;
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    shard_id, shard_not_found_message, ShardIteratorToken,
    ShardPosition,
    SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER,
    SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER, SHARD_ITERATOR_TYPE_AT_TIMESTAMP,
    SHARD_ITERATOR_TYPE_LATEST, SHARD_ITERATOR_TYPE_TRIM_HORIZON, STREAM_ACTIVE,
};
use service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// THE ONE poll loop of this crate's suites, so a case here waits for the CONDITION the
// emulator reaches on its own clock rather than for a duration it measures.
use crate::stream_tags::poll_until;

/// The region every case signs for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of this service (`OPERATIONS.md`, "Wire protocol").
const TARGET_PREFIX: &str = "Kinesis_20131202";

/// The modelled maximum length of a `ShardIterator`
/// (`smithy_query(query="operation",operation="GetShardIterator")` -> `ShardIterator`
/// len 1..512).
const MAX_SHARD_ITERATOR_LEN: usize = 512;

/// Every `ShardIteratorType` the model declares
/// (`smithy_query(query="shape",shape="ShardIteratorType")`).
const SHARD_ITERATOR_TYPES: &[&str] = &[
    SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER,
    SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
    SHARD_ITERATOR_TYPE_AT_TIMESTAMP,
    SHARD_ITERATOR_TYPE_LATEST,
    SHARD_ITERATOR_TYPE_TRIM_HORIZON,
];

/// A router carrying exactly this service, over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, so requests are unauthenticated.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesis/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, response) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {operation} to answer 200, got {status} with body {response}"
        );
        response
    }

    /// Call `operation` expecting a client refusal, answering its error code and message.
    async fn refused(&self, case: &str, operation: &str, body: &Value) -> (String, String) {
        let (status, response) = self.call(operation, body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected {operation} to answer 400, got {status} with body {response}"
        );
        (error_code(&response), error_message(&response))
    }

    /// A provisioned stream of `shard_count` shards, past the transition the create armed.
    async fn active_stream(&self, case: &str, name: &str, shard_count: i64) -> String {
        self.ok(
            case,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": shard_count }),
        )
        .await;
        self.poll_until_active(case, name).await;
        let summary = self
            .ok(case, "DescribeStreamSummary", &json!({"StreamName": name}))
            .await;
        summary["StreamDescriptionSummary"]["StreamARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: DescribeStreamSummary reported no StreamARN"))
            .to_string()
    }

    /// Look again until `name` reports the state its create owes it, through the one poll
    /// loop of this crate's suites.
    async fn poll_until_active(&self, case: &str, name: &str) {
        poll_until(
            case,
            &format!("the stream {name} reports {STREAM_ACTIVE}"),
            || async {
                let (status, response) = self
                    .call("DescribeStreamSummary", &json!({ "StreamName": name }))
                    .await;
                status == 200
                    && response["StreamDescriptionSummary"]["StreamStatus"].as_str()
                        == Some(STREAM_ACTIVE)
            },
        )
        .await;
    }

    /// The `SequenceNumberRange.StartingSequenceNumber` the shard map publishes for
    /// `shard`, which is the only sequence number a reader can name before anything is
    /// written.
    async fn starting_sequence_number(&self, case: &str, stream: &str, shard: &str) -> String {
        let listed = self
            .ok(case, "ListShards", &json!({"StreamName": stream}))
            .await;
        listed["Shards"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: ListShards reported no Shards"))
            .iter()
            .find(|entry| entry["ShardId"].as_str() == Some(shard))
            .map(|entry| entry["SequenceNumberRange"]["StartingSequenceNumber"].clone())
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("case {case}: the shard map carries no {shard}"))
    }

    /// The `ShardIterator` a successful `GetShardIterator` answered.
    async fn iterator(&self, case: &str, body: &Value) -> String {
        let answer = self.ok(case, "GetShardIterator", body).await;
        let iterator = answer["ShardIterator"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a ShardIterator, got {answer}"))
            .to_string();
        assert!(
            !iterator.is_empty() && iterator.len() <= MAX_SHARD_ITERATOR_LEN,
            "case {case}: a ShardIterator is 1..{MAX_SHARD_ITERATOR_LEN} characters, got {} \
             characters",
            iterator.len()
        );
        iterator
    }
}

/// The `__type` an awsJson1_1 error body carries, reduced to the error's shape name.
fn error_code(response: &Value) -> String {
    response
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message` an awsJson1_1 error body carries, under either casing the protocols use.
fn error_message(response: &Value) -> String {
    response
        .get("message")
        .or_else(|| response.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Epoch milliseconds, the unit `GetShardIteratorInput$Timestamp` carries ("A time stamp is
/// the Unix epoch date with precision in milliseconds").
///
/// Rounded UP to the next whole second, for two reasons: an awsJson `Timestamp` travels as
/// epoch seconds with a fractional part, so a whole second survives that round trip exactly;
/// and the instant is then past the stream's trim horizon, which for a stream nothing has
/// retained data in yet is the instant it was created — a timestamp before that horizon is
/// answered at the horizon instead, which is the subject of its own case below.
fn now_millis() -> u128 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_millis();
    millis + (1_000 - millis % 1_000)
}

/// Every iterator type, in every shard of the map, resolved by either identifier: the
/// iterator a client is handed names the shard it asked for and the position its type
/// denotes.
#[tokio::test]
async fn every_iterator_type_names_the_requested_position_in_the_requested_shard() {
    let emulator = Emulator::new();
    let stream = "iterators-every-type";
    let arn = emulator.active_stream("every type", stream, 3).await;
    let first_sequence_number = emulator
        .starting_sequence_number("every type", stream, &shard_id(0))
        .await;
    let at_timestamp = now_millis();

    // (case, the iterator type, the member its position comes from)
    let mut seen: Vec<String> = Vec::new();
    for shard_index in 0..3u32 {
        let shard = shard_id(shard_index);
        let sequence_number = emulator
            .starting_sequence_number("every type", stream, &shard)
            .await;
        for iterator_type in SHARD_ITERATOR_TYPES {
            // Rows differ along which identifier addresses the stream, so both forms are
            // exercised across the table.
            let identifier = if shard_index % 2 == 0 {
                json!({"StreamName": stream})
            } else {
                json!({"StreamARN": arn})
            };
            let mut body = identifier;
            body["ShardId"] = json!(shard);
            body["ShardIteratorType"] = json!(iterator_type);
            match *iterator_type {
                SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER
                | SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER => {
                    body["StartingSequenceNumber"] = json!(sequence_number);
                }
                SHARD_ITERATOR_TYPE_AT_TIMESTAMP => {
                    // A timestamp travels as epoch SECONDS on the wire for a `Timestamp`
                    // member of an awsJson protocol, with millisecond precision.
                    body["Timestamp"] = json!(at_timestamp as f64 / 1_000.0);
                }
                _ => {}
            }
            let case = format!("{iterator_type} in {shard}");
            let iterator = emulator.iterator(&case, &body).await;
            let decoded = ShardIteratorToken::decode(&iterator).unwrap_or_else(|| {
                panic!("case {case}: the iterator this service issued must decode")
            });
            assert_eq!(
                decoded.shard_id, shard,
                "case {case}: the iterator must name the shard the request asked for, got \
                 {decoded:?}"
            );
            assert_eq!(
                decoded.stream_arn, arn,
                "case {case}: the iterator must name the addressed stream, got {decoded:?}"
            );
            let expected = match *iterator_type {
                SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER => {
                    ShardPosition::AtSequenceNumber(sequence_number.clone())
                }
                SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER => {
                    ShardPosition::AfterSequenceNumber(sequence_number.clone())
                }
                SHARD_ITERATOR_TYPE_AT_TIMESTAMP => ShardPosition::AtTimestamp(at_timestamp),
                SHARD_ITERATOR_TYPE_LATEST => ShardPosition::Latest,
                _ => ShardPosition::TrimHorizon,
            };
            assert_eq!(
                decoded.position, expected,
                "case {case}: the iterator must name the position its type denotes, got \
                 {decoded:?}"
            );
            assert!(
                !decoded.is_expired(SystemTime::now()),
                "case {case}: a freshly issued iterator is not expired, got {decoded:?}"
            );
            seen.push(iterator);
        }
    }
    assert_eq!(
        seen.len(),
        3 * SHARD_ITERATOR_TYPES.len(),
        "every shard of the map answers every iterator type, got {} iterators",
        seen.len()
    );

    // Handing out an iterator writes nothing a later caller can see: the same request twice
    // names the same shard and the same position.
    let repeat_body = json!({
        "StreamName": stream,
        "ShardId": shard_id(0),
        "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
        "StartingSequenceNumber": first_sequence_number,
    });
    let first = emulator.iterator("repeat", &repeat_body).await;
    let second = emulator.iterator("repeat", &repeat_body).await;
    let (first, second) = (
        ShardIteratorToken::decode(&first).expect("the first iterator decodes"),
        ShardIteratorToken::decode(&second).expect("the second iterator decodes"),
    );
    assert_eq!(
        (first.shard_id, first.position.clone()),
        (second.shard_id, second.position),
        "case repeat: two calls for one position name the same shard and position"
    );
}

/// A timestamp older than the stream's trim horizon is answered with the oldest untrimmed
/// position: "If the time stamp is older than the current trim horizon, the iterator returned
/// is for the oldest untrimmed data record (TRIM_HORIZON)."
#[tokio::test]
async fn a_timestamp_before_the_trim_horizon_is_answered_at_the_trim_horizon() {
    let emulator = Emulator::new();
    let stream = "iterators-horizon";
    emulator.active_stream("horizon", stream, 1).await;

    // (case, the timestamp the request names, the position it is answered at)
    let now = now_millis();
    let day_millis = 24 * 60 * 60 * 1_000u128;
    let cases = [
        (
            "two days before the horizon",
            now.saturating_sub(2 * day_millis),
            ShardPosition::TrimHorizon,
        ),
        ("inside the retention window", now, ShardPosition::AtTimestamp(now)),
    ];
    for (case, at, expected) in cases {
        let iterator = emulator
            .iterator(
                case,
                &json!({
                    "StreamName": stream,
                    "ShardId": shard_id(0),
                    "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_TIMESTAMP,
                    "Timestamp": at as f64 / 1_000.0,
                }),
            )
            .await;
        let decoded =
            ShardIteratorToken::decode(&iterator).expect("the iterator this service issued decodes");
        assert_eq!(
            decoded.position, expected,
            "case {case}: expected the position {expected:?}, got {decoded:?}"
        );
    }
}

/// Every refusal the rule names, each with the modelled error code it is refused under.
#[tokio::test]
async fn a_request_that_names_no_position_or_no_shard_is_refused() {
    let emulator = Emulator::new();
    let stream = "iterators-refusals";
    emulator.active_stream("refusals", stream, 2).await;
    let sequence_number = emulator
        .starting_sequence_number("refusals", stream, &shard_id(0))
        .await;
    let missing_shard = shard_id(9);

    // (case, the request, the modelled error code it is refused under)
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a stream this account and region do not hold",
            json!({
                "StreamName": "iterators-no-such-stream",
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
            "ResourceNotFoundException",
        ),
        (
            "a shard the stream does not hold",
            json!({
                "StreamName": stream,
                "ShardId": missing_shard,
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
            "ResourceNotFoundException",
        ),
        (
            "neither StreamName nor StreamARN",
            json!({
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
            "InvalidArgumentException",
        ),
        (
            "AT_SEQUENCE_NUMBER without a StartingSequenceNumber",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
            }),
            "InvalidArgumentException",
        ),
        (
            "AFTER_SEQUENCE_NUMBER without a StartingSequenceNumber",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER,
            }),
            "InvalidArgumentException",
        ),
        (
            "AT_TIMESTAMP without a Timestamp",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_TIMESTAMP,
            }),
            "InvalidArgumentException",
        ),
        (
            "TRIM_HORIZON with a StartingSequenceNumber",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
                "StartingSequenceNumber": sequence_number,
            }),
            "InvalidArgumentException",
        ),
        (
            "LATEST with a Timestamp",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_LATEST,
                "Timestamp": now_millis() as f64 / 1_000.0,
            }),
            "InvalidArgumentException",
        ),
        (
            "a sequence number of no shard of the stream",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
                "StartingSequenceNumber": "1",
            }),
            "InvalidArgumentException",
        ),
        (
            "a dry run of an otherwise valid request",
            json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
                "DryRun": true,
            }),
            "DryRunOperationException",
        ),
    ];
    for (case, body, expected) in cases {
        let (code, message) = emulator.refused(case, "GetShardIterator", &body).await;
        assert_eq!(
            code, expected,
            "case {case}: expected {expected}, got {code} with message {message:?}"
        );
        assert!(
            !message.is_empty(),
            "case {case}: a refusal carries a message, got none"
        );
    }

    // The two not-found refusals are both phrased at the SHARD this read asked for, each in
    // the sentence its own condition earns: a shard of a stream this account does not hold
    // does not exist at all, while a shard the HELD stream's map does not carry could not be
    // found in it.
    let (_, missing_stream_message) = emulator
        .refused(
            "the missing stream is named",
            "GetShardIterator",
            &json!({
                "StreamName": "iterators-no-such-stream",
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
        )
        .await;
    assert_eq!(
        missing_stream_message,
        shard_not_found_message(&shard_id(0), "iterators-no-such-stream"),
        "a shard of a missing stream is refused with the service's shard-not-found sentence"
    );
    let (_, missing_shard_message) = emulator
        .refused(
            "the missing shard is named",
            "GetShardIterator",
            &json!({
                "StreamName": stream,
                "ShardId": missing_shard,
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
        )
        .await;
    assert_eq!(
        missing_shard_message,
        could_not_find_shard_message(&missing_shard, stream),
        "a shard the held stream does not carry is refused with this operation's sentence for \
         the search that came up empty"
    );
}

/// A stream whose creation has not fallen due yet hands out no iterator: the shard is not
/// readable, which is the modelled not-found refusal.
#[tokio::test]
async fn a_stream_that_is_not_active_yet_hands_out_no_iterator() {
    let emulator = Emulator::new();
    let stream = "iterators-creating";
    emulator
        .ok(
            "creating",
            "CreateStream",
            &json!({"StreamName": stream, "ShardCount": 1}),
        )
        .await;
    let (code, message) = emulator
        .refused(
            "creating",
            "GetShardIterator",
            &json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
        )
        .await;
    assert_eq!(
        code, "ResourceNotFoundException",
        "case creating: expected ResourceNotFoundException, got {code} with message {message:?}"
    );

    // Once the transition the create armed has fallen due, the same request is served.
    emulator.poll_until_active("active", stream).await;
    let iterator = emulator
        .iterator(
            "active",
            &json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
            }),
        )
        .await;
    assert!(
        ShardIteratorToken::decode(&iterator).is_some(),
        "case active: the iterator an active stream answers must decode, got {iterator}"
    );
}

/// Write one record under `partition_key`, answering the shard it landed in and the sequence
/// number it was stored under — the two facts a producer hands a consumer to read on from.
async fn put_record(
    emulator: &Emulator,
    case: &str,
    stream: &str,
    payload: &str,
    partition_key: &str,
) -> (String, String) {
    let stored = emulator
        .ok(
            case,
            "PutRecord",
            &json!({
                "StreamName": stream,
                "Data": aws_smithy_types::base64::encode(payload),
                "PartitionKey": partition_key,
            }),
        )
        .await;
    let shard = stored["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: PutRecord answers a ShardId, got {stored}"))
        .to_string();
    let sequence_number = stored["SequenceNumber"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: PutRecord answers a SequenceNumber, got {stored}"))
        .to_string();
    (shard, sequence_number)
}

/// The record DATA one `GetRecords` page from `iterator` delivers, decoded.
async fn read_payloads(emulator: &Emulator, case: &str, iterator: &str) -> Vec<String> {
    let page = emulator
        .ok(case, "GetRecords", &json!({ "ShardIterator": iterator }))
        .await;
    page["Records"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: GetRecords must carry Records, got {page}"))
        .iter()
        .map(|record| {
            let encoded = record["Data"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a Record carries Data, got {record}"));
            let bytes = aws_smithy_types::base64::decode(encoded)
                .unwrap_or_else(|_| panic!("case {case}: Data is base64, got {encoded}"));
            String::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("case {case}: this suite writes text payloads"))
        })
        .collect()
}

/// The sentence a `StartingSequenceNumber` of ANOTHER shard is refused with: the shard the
/// value belongs to, then the shard the call addressed.
///
/// Written out here rather than borrowed from the crate, so the wording itself is what these
/// cases pin; the two shard ids are the only variable parts, and the value does not appear.
fn foreign_shard_message(minted_by: &str, addressed: &str) -> String {
    format!(
        "Invalid StartingSequenceNumber. It encodes {minted_by}, while it was used in a call \
         to a shard with {addressed}"
    )
}

/// A sequence number names a position only in the shard that minted it: on any other shard of
/// the same stream the request is refused, and the refusal names the shard the value is of.
///
/// Rows differ along which shard of the map minted the value, how it was minted (the range the
/// map publishes, or a write), which other shard the call addressed, and which of the two
/// sequence-number iterator types named the position.
#[tokio::test]
async fn a_sequence_number_of_another_shard_names_the_shard_it_belongs_to() {
    let emulator = Emulator::new();
    let stream = "iterators-foreign-shard";
    let case = "foreign shard";
    let shard_count = 3u32;
    emulator
        .active_stream(case, stream, i64::from(shard_count))
        .await;

    // One number of each shard of the map, the only one a reader can hold before anything is
    // written: the beginning of the range the shard publishes.
    let mut minted: Vec<(String, String)> = Vec::new();
    for index in 0..shard_count {
        let shard = shard_id(index);
        let published = emulator
            .starting_sequence_number(case, stream, &shard)
            .await;
        minted.push((shard, published));
    }
    // And one minted by a WRITE, which is the number a producer hands a consumer: it belongs
    // to the shard the record landed in, whichever shard the partition key chose.
    let payload = "owned-by-its-own-shard";
    let (written_in, written_number) = put_record(&emulator, case, stream, payload, "pk-a").await;
    minted.push((written_in.clone(), written_number.clone()));

    for (minted_by, sequence_number) in &minted {
        for addressed in (0..shard_count).map(shard_id).filter(|shard| shard != minted_by) {
            for position_type in [
                SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
                SHARD_ITERATOR_TYPE_AFTER_SEQUENCE_NUMBER,
            ] {
                let row =
                    format!("{sequence_number} of {minted_by} used on {addressed} as {position_type}");
                let (code, message) = emulator
                    .refused(
                        &row,
                        "GetShardIterator",
                        &json!({
                            "StreamName": stream,
                            "ShardId": addressed,
                            "ShardIteratorType": position_type,
                            "StartingSequenceNumber": sequence_number,
                        }),
                    )
                    .await;
                assert_eq!(
                    code, "InvalidArgumentException",
                    "case {row}: expected InvalidArgumentException, got {code} with message \
                     {message:?}"
                );
                let expected = foreign_shard_message(minted_by, &addressed);
                assert_eq!(
                    message, expected,
                    "case {row}: expected {expected:?}, got {message:?}"
                );
            }
        }
    }

    // The control arm: on its OWN shard the very same number still names the position it
    // denotes, and the record it names is served — the value was never malformed, it was
    // foreign.
    let own_shard = "its own shard";
    let iterator = emulator
        .iterator(
            own_shard,
            &json!({
                "StreamName": stream,
                "ShardId": written_in,
                "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
                "StartingSequenceNumber": written_number,
            }),
        )
        .await;
    let payloads = read_payloads(&emulator, own_shard, &iterator).await;
    assert_eq!(
        payloads,
        vec![payload.to_string()],
        "case {own_shard}: expected the record the sequence number names, got {payloads:?}"
    );

    // A value NO shard of the stream minted belongs to no shard the refusal could name, so it
    // is refused as a position in this shard instead — the other condition of this class, and
    // not the sentence above.
    for sequence_number in ["0", "1"] {
        let row = format!("{sequence_number}, which no shard of the stream minted");
        let addressed = shard_id(1);
        let (code, message) = emulator
            .refused(
                &row,
                "GetShardIterator",
                &json!({
                    "StreamName": stream,
                    "ShardId": addressed,
                    "ShardIteratorType": SHARD_ITERATOR_TYPE_AT_SEQUENCE_NUMBER,
                    "StartingSequenceNumber": sequence_number,
                }),
            )
            .await;
        assert_eq!(
            code, "InvalidArgumentException",
            "case {row}: expected InvalidArgumentException, got {code} with message {message:?}"
        );
        let expected = format!(
            "StartingSequenceNumber {sequence_number} is not a sequence number of shard \
             {addressed}"
        );
        assert_eq!(
            message, expected,
            "case {row}: expected {expected:?}, got {message:?}"
        );
    }

    // Nothing the refusals touched changed: the map still carries the same open shards, and a
    // position they do admit is still handed out.
    let after = "after the refusals";
    let listed = emulator
        .ok(after, "ListShards", &json!({ "StreamName": stream }))
        .await;
    let shards = listed["Shards"]
        .as_array()
        .unwrap_or_else(|| panic!("case {after}: ListShards must carry Shards, got {listed}"))
        .clone();
    let reported: Vec<String> = shards
        .iter()
        .map(|shard| shard["ShardId"].as_str().unwrap_or_default().to_string())
        .collect();
    let expected_map: Vec<String> = (0..shard_count).map(shard_id).collect();
    assert_eq!(
        reported, expected_map,
        "case {after}: expected the shard map {expected_map:?}, got {reported:?}"
    );
    for shard in &shards {
        assert!(
            shard["SequenceNumberRange"]["EndingSequenceNumber"].is_null(),
            "case {after}: every shard must still be open, got {shard}"
        );
    }
    for index in 0..shard_count {
        emulator
            .iterator(
                after,
                &json!({
                    "StreamName": stream,
                    "ShardId": shard_id(index),
                    "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
                }),
            )
            .await;
    }
}
