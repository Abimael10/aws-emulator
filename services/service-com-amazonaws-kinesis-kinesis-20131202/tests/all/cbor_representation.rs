// SPDX-License-Identifier: CC0-1.0

//! The CBOR REPRESENTATION of this service's `awsJson1_1` protocol, end to end over the
//! wire.
//!
//! RULE: `awsJson1_1` is a body-only RPC protocol whose body encoding is the caller's
//! choice, stated in `Content-Type`: `application/x-amz-json-1.1` carries the input's
//! members as JSON and `application/x-amz-cbor-1.1` carries the same members as CBOR. A
//! request in either encoding decodes to the same modelled input, is served by the same
//! operation over the same state, and is answered in the encoding it arrived in — a success,
//! a modelled error and a refusal alike. A body value the member it is bound to cannot be
//! read from is a SERIALIZATION fault rather than a constraint violation: `400`
//! `SerializationException` whose capitalized `Message` quotes the value the request sent and
//! names the type that value could not be converted to; a value that WAS read and then broke
//! a declared rule stays this service's `ValidationException`.
//!
//! The recorded flow this was derived from sends `DescribeStreamSummary`, `PutRecord` and
//! `GetShardIterator` in the CBOR encoding and is a few rows of the tables below. The rows
//! differ along what the rule generalises over: which encoding carries the request, which
//! operation it names, which member holds the unreadable value, which modelled type that
//! member declares, and which class of answer the call earns (a success, a modelled error, a
//! serialization fault, a constraint refusal).
//!
//! Every case drives the real request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! headers and bodies asserted here are the ones a client sees.

use std::sync::Arc;
use std::time::Duration;

use aws_smithy_cbor::data::Type;
use aws_smithy_cbor::{Decoder, Encoder};
use aws_smithy_types::Blob;
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::stream_not_found_message;
use service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every case signs for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of this service (`OPERATIONS.md`, "Wire protocol").
const TARGET_PREFIX: &str = "Kinesis_20131202";

/// The media type of the JSON representation of this service's protocol.
const JSON: &str = "application/x-amz-json-1.1";

/// The media type of the CBOR representation of this service's protocol.
const CBOR: &str = "application/x-amz-cbor-1.1";

/// The modelled maximum length of a `ShardIterator`
/// (`smithy_query(query="operation",operation="GetShardIterator")` -> `ShardIterator`
/// len 1..512).
const MAX_SHARD_ITERATOR_LEN: usize = 512;

/// How long a case keeps looking for a stream to become `ACTIVE` before giving up: far longer
/// than the transition a create arms, so a loaded host never fails a case.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits between two looks at a stream it is waiting for.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One value a request body carries for a member, in whichever encoding carries it.
#[derive(Debug, Clone)]
enum Member {
    /// A text string.
    Text(String),
    /// A whole number.
    Whole(i64),
    /// A boolean.
    Flag(bool),
    /// A byte string — the form CBOR carries a blob in. The JSON representation carries the
    /// same bytes as the base64 text it carries a blob in.
    Bytes(Vec<u8>),
}

impl Member {
    fn text(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

/// `members` as the JSON object the JSON representation carries them in. A blob is base64
/// TEXT there, which is the form that representation carries one in.
fn json_body(members: &[(&str, Member)]) -> Bytes {
    let mut object = serde_json::Map::new();
    for (name, value) in members {
        let held = match value {
            Member::Text(text) => json!(text),
            Member::Whole(number) => json!(number),
            Member::Flag(flag) => json!(flag),
            Member::Bytes(bytes) => json!(::aws_smithy_types::base64::encode(bytes)),
        };
        object.insert((*name).to_string(), held);
    }
    Bytes::from(Value::Object(object).to_string())
}

/// `members` as the CBOR map the CBOR representation carries them in.
fn cbor_body(members: &[(&str, Member)]) -> Bytes {
    let mut encoder = Encoder::new(Vec::new());
    encoder.map(members.len());
    for (name, value) in members {
        encoder.str(name);
        match value {
            Member::Text(text) => {
                encoder.str(text);
            }
            Member::Whole(number) => {
                encoder.long(*number);
            }
            Member::Flag(flag) => {
                encoder.boolean(*flag);
            }
            Member::Bytes(bytes) => {
                encoder.blob(&Blob::new(bytes.clone()));
            }
        }
    }
    Bytes::from(encoder.into_writer())
}

/// One value a response body carries, whichever encoding carries it: the two encodings decode
/// to the same shape, so every assertion below is written once.
#[derive(Debug, Clone, PartialEq)]
enum Decoded {
    Text(String),
    Number(f64),
    Flag(bool),
    Bytes(Vec<u8>),
    List(Vec<Decoded>),
    Map(Vec<(String, Decoded)>),
    Null,
}

impl Decoded {
    /// The value under `key`, for a map.
    fn get(&self, key: &str) -> Option<&Decoded> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The element at `at`, for a list.
    fn at(&self, at: usize) -> Option<&Decoded> {
        match self {
            Self::List(elements) => elements.get(at),
            _ => None,
        }
    }

    /// This value's text, for a text member.
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// This value's bytes, for a blob member.
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// The text under `key`, or the empty string when the body carries none there — so an
    /// assertion reports what the body held rather than panicking on its absence.
    fn text_at(&self, key: &str) -> &str {
        self.get(key).and_then(Decoded::text).unwrap_or_default()
    }
}

/// A JSON response body as the shape above.
fn decoded_json(body: &[u8]) -> Decoded {
    fn convert(value: Value) -> Decoded {
        match value {
            Value::Null => Decoded::Null,
            Value::Bool(flag) => Decoded::Flag(flag),
            Value::Number(number) => Decoded::Number(number.as_f64().unwrap_or_default()),
            Value::String(text) => Decoded::Text(text),
            Value::Array(elements) => Decoded::List(elements.into_iter().map(convert).collect()),
            Value::Object(members) => Decoded::Map(
                members
                    .into_iter()
                    .map(|(key, held)| (key, convert(held)))
                    .collect(),
            ),
        }
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(value) => convert(value),
        Err(_) => Decoded::Null,
    }
}

/// A CBOR response body as the shape above, decoded with the CBOR implementation the
/// framework's own codec is built on rather than scanned as text, so what is asserted is the
/// WHOLE decoded value a client reads.
fn decoded_cbor(body: &[u8]) -> Decoded {
    let mut decoder = Decoder::new(body);
    read_cbor(&mut decoder)
}

/// One CBOR item, and everything nested inside it.
fn read_cbor(decoder: &mut Decoder<'_>) -> Decoded {
    let datatype = decoder
        .datatype()
        .unwrap_or_else(|error| panic!("the body ends where an item was expected: {error}"));
    match datatype {
        Type::String | Type::StringIndef => Decoded::Text(
            decoder
                .string()
                .unwrap_or_else(|error| panic!("a text item does not read: {error}")),
        ),
        Type::Bytes | Type::BytesIndef => Decoded::Bytes(
            decoder
                .blob()
                .unwrap_or_else(|error| panic!("a byte string does not read: {error}"))
                .into_inner(),
        ),
        Type::Bool => Decoded::Flag(
            decoder
                .boolean()
                .unwrap_or_else(|error| panic!("a boolean does not read: {error}")),
        ),
        Type::U8 | Type::U16 | Type::U32 | Type::U64 | Type::I8 | Type::I16 | Type::I32
        | Type::I64 | Type::Int => Decoded::Number(
            decoder
                .long()
                .unwrap_or_else(|error| panic!("a whole number does not read: {error}"))
                as f64,
        ),
        Type::F16 | Type::F32 | Type::F64 => Decoded::Number(
            decoder
                .double()
                .unwrap_or_else(|error| panic!("a number does not read: {error}")),
        ),
        // The form CBOR carries an instant in: a tagged epoch value.
        Type::Tag => Decoded::Number(
            decoder
                .timestamp()
                .unwrap_or_else(|error| panic!("a tagged item is not an instant: {error}"))
                .as_secs_f64(),
        ),
        Type::Null => {
            decoder.null().expect("a null reads");
            Decoded::Null
        }
        Type::Array | Type::ArrayIndef => {
            let declared = decoder
                .list()
                .unwrap_or_else(|error| panic!("an array header does not read: {error}"));
            let mut elements = Vec::new();
            while !at_end(decoder, declared, elements.len()) {
                elements.push(read_cbor(decoder));
            }
            end_container(decoder, declared);
            Decoded::List(elements)
        }
        Type::Map | Type::MapIndef => {
            let declared = decoder
                .map()
                .unwrap_or_else(|error| panic!("a map header does not read: {error}"));
            let mut entries = Vec::new();
            while !at_end(decoder, declared, entries.len()) {
                let key = decoder
                    .string()
                    .unwrap_or_else(|error| panic!("a map key is not text: {error}"));
                entries.push((key, read_cbor(decoder)));
            }
            end_container(decoder, declared);
            Decoded::Map(entries)
        }
        other => panic!("a response body carries no item of type {other:?}"),
    }
}

/// Whether a container that declared `declared` entries (or none, for an indefinite one) ends
/// here, `read` entries in.
fn at_end(decoder: &mut Decoder<'_>, declared: Option<u64>, read: usize) -> bool {
    match declared {
        Some(entries) => read as u64 == entries,
        None => matches!(
            decoder
                .datatype()
                .unwrap_or_else(|error| panic!("a container ends mid-entry: {error}")),
            Type::Break
        ),
    }
}

/// Consume the break that terminates an indefinite-length container.
fn end_container(decoder: &mut Decoder<'_>, declared: Option<u64>) {
    if declared.is_none() {
        decoder
            .skip()
            .unwrap_or_else(|error| panic!("the break of a container does not read: {error}"));
    }
}

/// A router carrying exactly this service, over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

/// One answer a call earns: the status, the media type the body is labelled with, and the
/// decoded body.
struct Answer {
    status: u16,
    content_type: String,
    body: Decoded,
}

impl Answer {
    /// The error code a refusal names, as a client branches on it.
    fn error_code(&self) -> &str {
        self.body.text_at("__type")
    }

    /// The prose a refusal carries, from either member an error of this service states it in:
    /// the capitalized one a protocol-level error uses, and the lowercase one the modelled
    /// exceptions declare.
    fn error_message(&self) -> &str {
        match self.body.get("Message") {
            Some(_) => self.body.text_at("Message"),
            None => self.body.text_at("message"),
        }
    }
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

    /// Call `operation` with `body` already encoded as `content_type`.
    async fn send(&self, content_type: &str, operation: &str, body: Bytes) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", content_type)
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesis/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(body)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered_as = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if answered_as.eq_ignore_ascii_case(CBOR) {
            decoded_cbor(response.body())
        } else {
            decoded_json(response.body())
        };
        Answer {
            status,
            content_type: answered_as,
            body,
        }
    }

    /// Call `operation` in the representation `content_type` names, with `members` as its
    /// input.
    async fn call(&self, content_type: &str, operation: &str, members: &[(&str, Member)]) -> Answer {
        let body = if content_type == CBOR {
            cbor_body(members)
        } else {
            json_body(members)
        };
        self.send(content_type, operation, body).await
    }

    /// Call `operation`, requiring the success the rule says it earns.
    async fn ok(
        &self,
        case: &str,
        content_type: &str,
        operation: &str,
        members: &[(&str, Member)],
    ) -> Answer {
        let answer = self.call(content_type, operation, members).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected {operation} over {content_type} to answer 200, got {} \
             with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.content_type, content_type,
            "case {case}: expected {operation} to answer in the encoding it was asked in, \
             got {:?}",
            answer.content_type
        );
        answer
    }

    /// A provisioned stream of `shards` shards, in the representation `content_type` names,
    /// looked at until the transition the create armed has been made.
    async fn active_stream(&self, case: &str, content_type: &str, name: &str, shards: i64) {
        self.ok(
            case,
            content_type,
            "CreateStream",
            &[
                ("StreamName", Member::text(name)),
                ("ShardCount", Member::Whole(shards)),
            ],
        )
        .await;
        let deadline = std::time::Instant::now() + READY_DEADLINE;
        loop {
            let summary = self
                .ok(
                    case,
                    content_type,
                    "DescribeStreamSummary",
                    &[("StreamName", Member::text(name))],
                )
                .await;
            let status = summary
                .body
                .get("StreamDescriptionSummary")
                .map(|held| held.text_at("StreamStatus"))
                .unwrap_or_default()
                .to_string();
            if status == "ACTIVE" {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case}: expected {name} to reach ACTIVE, it is still {status:?}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
    }

    /// The id of the first shard of `name`, read in the representation `content_type` names.
    async fn first_shard(&self, case: &str, content_type: &str, name: &str) -> String {
        let listed = self
            .ok(
                case,
                content_type,
                "ListShards",
                &[("StreamName", Member::text(name))],
            )
            .await;
        listed
            .body
            .get("Shards")
            .and_then(|shards| shards.at(0))
            .map(|shard| shard.text_at("ShardId").to_string())
            .unwrap_or_else(|| panic!("case {case}: ListShards reported no shard: {:?}", listed.body))
    }
}

/// Both encodings of this service's protocol, so a rule stated over "the encoding the request
/// arrived in" is checked in each of them.
const REPRESENTATIONS: &[&str] = &[JSON, CBOR];

/// RULE: a request in the CBOR representation is DECODED as the modelled input, served by the
/// operation, and answered in that same representation — so a flow that provisions a stream,
/// waits for it, lists its shards, takes an iterator and round-trips a record through it is
/// served entirely in CBOR, exactly as the JSON representation serves it.
#[tokio::test]
async fn a_flow_asked_for_in_cbor_is_served_and_answered_in_cbor() {
    let emulator = Emulator::new();
    let case = "a flow in the CBOR representation";
    let stream = "cbor-flow";
    emulator.active_stream(case, CBOR, stream, 2).await;
    let shard = emulator.first_shard(case, CBOR, stream).await;

    // The iterator a reader starts from, taken over CBOR: the step the recording shows as a
    // 200 carrying an iterator.
    let answered = emulator
        .ok(
            case,
            CBOR,
            "GetShardIterator",
            &[
                ("StreamName", Member::text(stream)),
                ("ShardId", Member::text(&shard)),
                ("ShardIteratorType", Member::text("TRIM_HORIZON")),
            ],
        )
        .await;
    let iterator = answered.body.text_at("ShardIterator").to_string();
    assert!(
        !iterator.is_empty() && iterator.len() <= MAX_SHARD_ITERATOR_LEN,
        "case {case}: expected an iterator within the modelled length, got {} bytes: {:?}",
        iterator.len(),
        answered.body
    );

    // A record written as a CBOR byte string comes back as the same bytes: the blob is
    // carried in the form this representation carries one, in both directions.
    let payload = b"\x00\x01cbor payload\xff".to_vec();
    let written = emulator
        .ok(
            case,
            CBOR,
            "PutRecord",
            &[
                ("StreamName", Member::text(stream)),
                ("PartitionKey", Member::text("key-1")),
                ("Data", Member::Bytes(payload.clone())),
            ],
        )
        .await;
    assert_eq!(
        written.body.text_at("ShardId"),
        shard,
        "case {case}: expected the write to name the shard it landed in, got {:?}",
        written.body
    );
    let read = emulator
        .ok(
            case,
            CBOR,
            "GetRecords",
            &[("ShardIterator", Member::text(&iterator))],
        )
        .await;
    let first = read
        .body
        .get("Records")
        .and_then(|records| records.at(0))
        .unwrap_or_else(|| panic!("case {case}: GetRecords reported no record: {:?}", read.body));
    assert_eq!(
        first.get("Data").and_then(Decoded::bytes),
        Some(payload.as_slice()),
        "case {case}: expected the record's own bytes back, got {:?}",
        first
    );
}

/// RULE: both encodings are the same service over the same state — a stream created in one is
/// described in the other, and a stream neither holds is the same modelled
/// `ResourceNotFoundException`, with the message this service words that class with, in
/// whichever encoding asked.
#[tokio::test]
async fn one_state_answers_both_representations() {
    let emulator = Emulator::new();
    // (case, the encoding the stream is created in, the encoding it is described in)
    let rows: &[(&str, &str, &str)] = &[
        ("created in JSON, described in CBOR", JSON, CBOR),
        ("created in CBOR, described in JSON", CBOR, JSON),
        ("created and described in CBOR", CBOR, CBOR),
    ];
    for (case, created_in, described_in) in rows.iter().copied() {
        let stream = format!("shared-{}", case.replace(' ', "-").replace(',', ""));
        emulator.active_stream(case, created_in, &stream, 1).await;
        let summary = emulator
            .ok(
                case,
                described_in,
                "DescribeStreamSummary",
                &[("StreamName", Member::text(&stream))],
            )
            .await;
        let described = summary
            .body
            .get("StreamDescriptionSummary")
            .unwrap_or_else(|| panic!("case {case}: no summary in {:?}", summary.body));
        assert_eq!(
            described.text_at("StreamName"),
            stream,
            "case {case}: expected the stream the other encoding created, got {described:?}"
        );
    }
}

/// RULE: a stream this account and region do not hold is the operation's own modelled
/// `ResourceNotFoundException`, carrying the sentence this service words that class with —
/// answered in the encoding the request arrived in, and reached in CBOR only because the body
/// was decoded at all.
#[tokio::test]
async fn an_absent_stream_is_the_modelled_refusal_in_both_representations() {
    let emulator = Emulator::new();
    // The names differ along the only variable part of the sentence: the stream name taken
    // from the request.
    let absent = ["nonesuch", "absent.stream-1", "A", &"n".repeat(128)];
    for representation in REPRESENTATIONS.iter().copied() {
        for name in absent {
            let case = format!("{name:?} over {representation}");
            let answer = emulator
                .call(
                    representation,
                    "DescribeStreamSummary",
                    &[("StreamName", Member::text(name))],
                )
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.content_type, representation,
                "case {case}: expected the answer in the encoding asked, got {:?}",
                answer.content_type
            );
            assert_eq!(
                answer.error_code(),
                "ResourceNotFoundException",
                "case {case}: expected the modelled not-found error, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.error_message(),
                stream_not_found_message(name),
                "case {case}: expected this service's sentence for that class, got {:?}",
                answer.error_message()
            );
        }
    }
}

/// One row of the serialization-fault table: a member given a value its modelled type cannot
/// be read from.
struct Unreadable {
    case: &'static str,
    /// The encodings this row is sent in: a byte string has no JSON form, and base64 text IS
    /// how JSON carries a blob, so those rows are CBOR-only.
    representations: &'static [&'static str],
    operation: &'static str,
    members: fn() -> Vec<(&'static str, Member)>,
    /// The sentence the refusal states, whichever encoding carried the request.
    message: &'static str,
}

/// RULE: a value the modelled member it is bound to cannot be read from never becomes a
/// value, so it is refused at the deserialization layer with `400 SerializationException`
/// whose capitalized `Message` quotes the value the request sent and names the type that
/// value could not be converted to — in the encoding the request arrived in.
///
/// The rows differ along what the rule generalises over: the modelled type of the member
/// (string, blob, integer), the class of value sent (text, number, flag), the operation, and
/// the encoding — including the one class only CBOR has a form for, where the recorded case
/// stands.
#[tokio::test]
async fn an_unreadable_value_is_a_serialization_fault_naming_the_value_and_the_type() {
    let emulator = Emulator::new();
    let rows: &[Unreadable] = &[
        Unreadable {
            case: "a number at a string member",
            representations: REPRESENTATIONS,
            operation: "DescribeStreamSummary",
            members: || vec![("StreamName", Member::Whole(12))],
            message: "'12' can not be converted to a CharSequence",
        },
        Unreadable {
            case: "a flag at a string member",
            representations: REPRESENTATIONS,
            operation: "DescribeStreamSummary",
            members: || vec![("StreamName", Member::Flag(true))],
            message: "'true' can not be converted to a CharSequence",
        },
        Unreadable {
            case: "a number at the stream name of a write",
            representations: REPRESENTATIONS,
            operation: "PutRecord",
            members: || {
                vec![
                    ("StreamName", Member::Whole(-7)),
                    ("PartitionKey", Member::text("key")),
                    ("Data", Member::Bytes(b"payload".to_vec())),
                ]
            },
            message: "'-7' can not be converted to a CharSequence",
        },
        Unreadable {
            // The recorded case: text where the blob of a write is carried as a byte string.
            case: "text at a blob member",
            representations: &[CBOR],
            operation: "PutRecord",
            members: || {
                vec![
                    ("StreamName", Member::text("any")),
                    ("PartitionKey", Member::text("key")),
                    ("Data", Member::text("notbytes")),
                ]
            },
            message: "'notbytes' can not be converted to a ByteBuffer",
        },
        Unreadable {
            case: "a number at a blob member",
            representations: REPRESENTATIONS,
            operation: "PutRecord",
            members: || {
                vec![
                    ("StreamName", Member::text("any")),
                    ("PartitionKey", Member::text("key")),
                    ("Data", Member::Whole(5)),
                ]
            },
            message: "'5' can not be converted to a ByteBuffer",
        },
        Unreadable {
            case: "text at an integer member",
            representations: REPRESENTATIONS,
            operation: "CreateStream",
            members: || {
                vec![
                    ("StreamName", Member::text("counted")),
                    ("ShardCount", Member::text("two")),
                ]
            },
            message: "'two' can not be converted to an Integer",
        },
        Unreadable {
            case: "a flag at an integer member",
            representations: REPRESENTATIONS,
            operation: "GetRecords",
            members: || {
                vec![
                    ("ShardIterator", Member::text("any")),
                    ("Limit", Member::Flag(false)),
                ]
            },
            message: "'false' can not be converted to an Integer",
        },
        Unreadable {
            case: "a number past the width of an integer member",
            representations: REPRESENTATIONS,
            operation: "CreateStream",
            members: || {
                vec![
                    ("StreamName", Member::text("counted")),
                    ("ShardCount", Member::Whole(i64::from(i32::MAX) + 1)),
                ]
            },
            message: "'2147483648' can not be converted to an Integer",
        },
    ];

    for row in rows {
        for representation in row.representations.iter().copied() {
            let case = format!("{} over {representation}", row.case);
            let members = (row.members)();
            let answer = emulator
                .call(representation, row.operation, &members)
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.content_type, representation,
                "case {case}: expected the refusal in the encoding asked, got {:?}",
                answer.content_type
            );
            assert_eq!(
                answer.error_code(),
                "SerializationException",
                "case {case}: expected the serialization class, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.body.text_at("Message"),
                row.message,
                "case {case}: expected the capitalized Message to state the value and the \
                 type, got {:?}",
                answer.body
            );
        }
    }
}

/// RULE: a value that WAS read and then broke a rule the model declares is the OTHER class —
/// this service's `ValidationException`, worded the way it words a constraint refusal —
/// answered in the encoding the request arrived in. The two classes are what a client
/// branches on, so a request that breaks a bound is never a serialization fault.
#[tokio::test]
async fn a_value_that_was_read_and_broke_a_bound_is_the_constraint_refusal() {
    let emulator = Emulator::new();
    // Each row is a value of the RIGHT class that breaks a modelled constraint: an empty
    // name against `@length 1..128`, and a name against `@pattern`.
    struct Broken {
        case: &'static str,
        operation: &'static str,
        members: fn() -> Vec<(&'static str, Member)>,
    }
    let rows: &[Broken] = &[
        Broken {
            case: "an empty stream name",
            operation: "DescribeStreamSummary",
            members: || vec![("StreamName", Member::Text(String::new()))],
        },
        Broken {
            case: "a stream name the pattern does not admit",
            operation: "DescribeStreamSummary",
            members: || vec![("StreamName", Member::text("no spaces here"))],
        },
    ];
    for representation in REPRESENTATIONS.iter().copied() {
        for row in rows {
            let case = format!("{} over {representation}", row.case);
            let members = (row.members)();
            let answer = emulator.call(representation, row.operation, &members).await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.content_type, representation,
                "case {case}: expected the refusal in the encoding asked, got {:?}",
                answer.content_type
            );
            assert_eq!(
                answer.error_code(),
                "ValidationException",
                "case {case}: expected the constraint class, got {:?}",
                answer.body
            );
            assert!(
                answer
                    .error_message()
                    .contains("failed to satisfy constraint"),
                "case {case}: expected this service's constraint sentence, got {:?}",
                answer.error_message()
            );
        }
    }
}
