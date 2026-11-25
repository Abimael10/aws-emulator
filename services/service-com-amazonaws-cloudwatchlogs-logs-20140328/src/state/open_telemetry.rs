// SPDX-License-Identifier: CC0-1.0

//! The OpenTelemetry protocol's own wire encoding: what the OTLP logs endpoint of this service
//! READS a request body in, and what it ANSWERS in.
//!
//! The other HTTP ingestion endpoints of this service ([`super::http_ingestion`]) are its own
//! and answer in its own one-member JSON envelope. `/v1/logs` is not: it serves OTLP/HTTP, a
//! protocol with its own request and response messages, so what a client there sends and
//! reads is that protocol's and not this service's — an OTLP exporter decodes the body of a
//! rejection as a `google.rpc.Status` to learn whether to retry or to re-authenticate, and a
//! JSON document decodes to nothing under it.
//!
//! Three messages, each the one the protocol names for its position:
//!
//! * `opentelemetry.proto.collector.logs.v1.ExportLogsServiceRequest` is what a client ships,
//!   and its log records are the events to ingest ([`export_request_log_records`]);
//! * `opentelemetry.proto.collector.logs.v1.ExportLogsServiceResponse` is what an accepted
//!   export is answered with, carrying the partial success of a batch some of whose records
//!   were refused ([`export_response_body`]);
//! * `google.rpc.Status` is what a REJECTED export is answered with, carrying the canonical
//!   code of its class and the sentence stating it ([`status_body`]).
//!
//! The protobuf codec itself is [`prost`]'s — a maintained implementation — and what is
//! written here is only the message layout, transcribed from the protocol's own `.proto`
//! definitions with the field numbers it assigns. Fields this reader does not name are
//! skipped as unknown by that codec, which is what lets the layout carry only the levels the
//! records are nested at and the record fields a log event is read from.
//!
//! The same protocol also defines a JSON encoding of those messages, which is why a body is
//! read as JSON first and as protobuf only when it is not JSON at all
//! ([`super::http_ingestion::read_records`]): a client writing either encoding is served. A
//! record that ARRIVED as protobuf is projected back onto that JSON encoding
//! ([`log_record_document`]), so what a reader gets back out of the log stream is one
//! document form whichever encoding shipped it.
//!
//! What an ANSWER is encoded in is not negotiated: RECORDED behaviour is that this endpoint
//! answers the protobuf encoding of these messages, and the recorded rejection of an export
//! is the protobuf `google.rpc.Status` whichever encoding the export itself arrived in — so
//! the seam that serves it encodes every answer this one way
//! ([`super::super::service::http_ingestion`]) rather than mirroring the request's own media
//! type.

use ::prost::Message as _;
use ::serde_json::{Map, Value};

/// The media type the OTLP/HTTP protobuf encoding is carried under, and the one every answer
/// of the OTLP endpoint is served with.
pub const PROTOBUF_MEDIA_TYPE: &str = "application/x-protobuf";

/// The canonical codes `google.rpc.Status$code` carries, in the numbering `google.rpc.Code`
/// assigns them: the ones the HTTP statuses this endpoint answers map onto
/// ([`rpc_code_for_status`]).
const UNKNOWN: i32 = 2;
/// See [`UNKNOWN`].
const INVALID_ARGUMENT: i32 = 3;
/// See [`UNKNOWN`].
const NOT_FOUND: i32 = 5;
/// See [`UNKNOWN`].
const PERMISSION_DENIED: i32 = 7;
/// See [`UNKNOWN`].
const RESOURCE_EXHAUSTED: i32 = 8;
/// See [`UNKNOWN`].
const UNIMPLEMENTED: i32 = 12;
/// See [`UNKNOWN`].
const UNAVAILABLE: i32 = 14;

/// The canonical code a refusal answered with the HTTP status `status` carries, by the gRPC
/// specification's own mapping of an HTTP status onto a `google.rpc.Code` ("HTTP to gRPC
/// Status Code Mapping"): a status that mapping does not name is `UNKNOWN`, which is what it
/// states for every other one.
#[must_use]
pub fn rpc_code_for_status(status: u16) -> i32 {
    match status {
        400 => INVALID_ARGUMENT,
        401 | 403 => PERMISSION_DENIED,
        404 => NOT_FOUND,
        429 => RESOURCE_EXHAUSTED,
        501 => UNIMPLEMENTED,
        502..=504 => UNAVAILABLE,
        _ => UNKNOWN,
    }
}

/// The body a REFUSED export is answered with: the `google.rpc.Status` carrying `code` and
/// `message`, protobuf-encoded.
#[must_use]
pub fn status_body(code: i32, message: &str) -> Vec<u8> {
    Status {
        code,
        message: message.to_string(),
    }
    .encode_to_vec()
}

/// The body an ACCEPTED export is answered with: the `ExportLogsServiceResponse` of that
/// batch, protobuf-encoded.
///
/// A batch every record of which was ingested carries no `partial_success` at all — "if the
/// request is fully accepted the server MUST respond with an empty message" — so the encoding
/// of that answer is the empty message, zero bytes long. A batch some of whose records were
/// refused carries the count of them and the message stating why.
#[must_use]
pub fn export_response_body(rejected: u64, error_message: &str) -> Vec<u8> {
    let partial_success = (rejected > 0).then(|| ExportLogsPartialSuccess {
        rejected_log_records: i64::try_from(rejected).unwrap_or(i64::MAX),
        error_message: error_message.to_string(),
    });
    ExportLogsServiceResponse { partial_success }.encode_to_vec()
}

/// The log records one protobuf `ExportLogsServiceRequest` carries, each projected onto the
/// protocol's JSON encoding of a log record, or `None` when `body` is not such a message at
/// all.
///
/// The encoding nests the records three levels deep — `resource_logs[].scope_logs[]
/// .log_records[]` — so one request ships the records of several resources and scopes
/// together; what is read is every log record of all of them, because each one is one log
/// event, and at most `limit` + 1 of them, since a batch past the maximum is refused whole
/// and nothing past it has to be seen.
///
/// `None` for a body the codec cannot read as the message, and for one it reads as a message
/// carrying no log record: neither is an export document this endpoint has records from, and
/// the caller then reports the body as one that carried nothing to ingest.
#[must_use]
pub fn export_request_log_records(body: &[u8], limit: usize) -> Option<Vec<Value>> {
    let request = ExportLogsServiceRequest::decode(body).ok()?;
    let records: Vec<Value> = request
        .resource_logs
        .iter()
        .flat_map(|resource| resource.scope_logs.iter())
        .flat_map(|scope| scope.log_records.iter())
        .take(limit.saturating_add(1))
        .map(log_record_document)
        .collect();
    (!records.is_empty()).then_some(records)
}

/// One decoded log record as the protocol's JSON encoding of it.
///
/// The protobuf JSON mapping the protocol adopts is what decides each member's spelling and
/// form: the member names are the field names in `lowerCamelCase`, a 64-bit integer travels as
/// a STRING (so a client's `timeUnixNano` is not rounded by a JSON number), a 32-bit one as a
/// number, and a field holding its default value is left out — which is why a record that
/// states no instant, no severity and no attributes projects onto its body alone.
/// `trace_id` and `span_id` are the one exception the OTLP JSON encoding states: they travel
/// as the hex of their bytes rather than as base64.
fn log_record_document(record: &LogRecord) -> Value {
    let mut document = Map::new();
    if record.time_unix_nano != 0 {
        document.insert(
            "timeUnixNano".to_string(),
            Value::String(record.time_unix_nano.to_string()),
        );
    }
    if record.observed_time_unix_nano != 0 {
        document.insert(
            "observedTimeUnixNano".to_string(),
            Value::String(record.observed_time_unix_nano.to_string()),
        );
    }
    if record.severity_number != 0 {
        document.insert(
            "severityNumber".to_string(),
            Value::Number(record.severity_number.into()),
        );
    }
    if !record.severity_text.is_empty() {
        document.insert(
            "severityText".to_string(),
            Value::String(record.severity_text.clone()),
        );
    }
    if let Some(body) = &record.body {
        document.insert("body".to_string(), any_value_document(body));
    }
    if !record.attributes.is_empty() {
        document.insert(
            "attributes".to_string(),
            Value::Array(record.attributes.iter().map(key_value_document).collect()),
        );
    }
    if record.dropped_attributes_count != 0 {
        document.insert(
            "droppedAttributesCount".to_string(),
            Value::Number(record.dropped_attributes_count.into()),
        );
    }
    if record.flags != 0 {
        document.insert("flags".to_string(), Value::Number(record.flags.into()));
    }
    if !record.trace_id.is_empty() {
        document.insert(
            "traceId".to_string(),
            Value::String(::hex::encode(&record.trace_id)),
        );
    }
    if !record.span_id.is_empty() {
        document.insert(
            "spanId".to_string(),
            Value::String(::hex::encode(&record.span_id)),
        );
    }
    if !record.event_name.is_empty() {
        document.insert(
            "eventName".to_string(),
            Value::String(record.event_name.clone()),
        );
    }
    Value::Object(document)
}

/// One `AnyValue` as the JSON encoding of it: the one-member object naming which of the
/// alternatives the value holds. An `AnyValue` holding none is the empty object, which is
/// what the encoding states for a value that was never set.
fn any_value_document(value: &AnyValue) -> Value {
    nested_any_value_document(value, 0)
}

/// How deep a caller-supplied value this projection walks.
///
/// An `AnyValue` nests further `AnyValue`s through `arrayValue` and `kvlistValue`, and the
/// DEPTH is the request's to choose, so the walk carries its own explicit bound rather than
/// resting on one the protobuf codec happens to apply before it: a level past this bound is
/// projected as the empty object — the encoding's own form for a value that holds none — and
/// nothing below it is walked. The bound is this emulator's, well past any nesting an
/// instrumentation library produces for a log record's body or attributes.
const MAX_VALUE_DEPTH: usize = 32;

/// [`any_value_document`] at nesting level `depth`, which is what bounds the walk
/// ([`MAX_VALUE_DEPTH`]).
fn nested_any_value_document(value: &AnyValue, depth: usize) -> Value {
    let Some(held) = &value.value else {
        return Value::Object(Map::new());
    };
    if depth >= MAX_VALUE_DEPTH {
        return Value::Object(Map::new());
    }
    let (member, value) = match held {
        any_value::Value::StringValue(text) => ("stringValue", Value::String(text.clone())),
        any_value::Value::BoolValue(flag) => ("boolValue", Value::Bool(*flag)),
        // A 64-bit integer travels as a string under the protobuf JSON mapping.
        any_value::Value::IntValue(number) => ("intValue", Value::String(number.to_string())),
        any_value::Value::DoubleValue(number) => (
            "doubleValue",
            ::serde_json::Number::from_f64(*number)
                .map_or(Value::Null, Value::Number),
        ),
        any_value::Value::ArrayValue(array) => (
            "arrayValue",
            ::serde_json::json!({
                "values": array
                    .values
                    .iter()
                    .map(|value| nested_any_value_document(value, depth + 1))
                    .collect::<Vec<Value>>(),
            }),
        ),
        any_value::Value::KvlistValue(list) => (
            "kvlistValue",
            ::serde_json::json!({
                "values": list
                    .values
                    .iter()
                    .map(|pair| nested_key_value_document(pair, depth + 1))
                    .collect::<Vec<Value>>(),
            }),
        ),
        // Bytes travel as base64 under that same mapping.
        any_value::Value::BytesValue(bytes) => (
            "bytesValue",
            Value::String(::base64::Engine::encode(
                &::base64::engine::general_purpose::STANDARD,
                bytes,
            )),
        ),
    };
    ::serde_json::json!({ member: value })
}

/// One `KeyValue` as the JSON encoding of it: the key it names and the value it holds.
fn key_value_document(pair: &KeyValue) -> Value {
    nested_key_value_document(pair, 0)
}

/// [`key_value_document`] at nesting level `depth`. See [`MAX_VALUE_DEPTH`].
fn nested_key_value_document(pair: &KeyValue, depth: usize) -> Value {
    let mut document = Map::new();
    document.insert("key".to_string(), Value::String(pair.key.clone()));
    if let Some(value) = &pair.value {
        document.insert("value".to_string(), nested_any_value_document(value, depth));
    }
    Value::Object(document)
}

/// `google.rpc.Status`: the message a rejected request of a Google-API-shaped protocol — OTLP
/// among them — reports its refusal in.
#[derive(Clone, PartialEq, ::prost::Message)]
struct Status {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsServiceResponse`.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ExportLogsServiceResponse {
    #[prost(message, optional, tag = "1")]
    partial_success: Option<ExportLogsPartialSuccess>,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsPartialSuccess`.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ExportLogsPartialSuccess {
    #[prost(int64, tag = "1")]
    rejected_log_records: i64,
    #[prost(string, tag = "2")]
    error_message: String,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsServiceRequest`: what a client ships to
/// this endpoint.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ExportLogsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_logs: Vec<ResourceLogs>,
}

/// `opentelemetry.proto.logs.v1.ResourceLogs`. The resource it describes is skipped as an
/// unknown field: the records are what is ingested.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ResourceLogs {
    #[prost(message, repeated, tag = "2")]
    scope_logs: Vec<ScopeLogs>,
}

/// `opentelemetry.proto.logs.v1.ScopeLogs`. The instrumentation scope it describes is skipped
/// the same way.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ScopeLogs {
    #[prost(message, repeated, tag = "2")]
    log_records: Vec<LogRecord>,
}

/// `opentelemetry.proto.logs.v1.LogRecord`: ONE log event, with the field numbers that
/// definition assigns.
#[derive(Clone, PartialEq, ::prost::Message)]
struct LogRecord {
    #[prost(fixed64, tag = "1")]
    time_unix_nano: u64,
    #[prost(int32, tag = "2")]
    severity_number: i32,
    #[prost(string, tag = "3")]
    severity_text: String,
    #[prost(message, optional, tag = "5")]
    body: Option<AnyValue>,
    #[prost(message, repeated, tag = "6")]
    attributes: Vec<KeyValue>,
    #[prost(uint32, tag = "7")]
    dropped_attributes_count: u32,
    #[prost(fixed32, tag = "8")]
    flags: u32,
    #[prost(bytes = "vec", tag = "9")]
    trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "10")]
    span_id: Vec<u8>,
    #[prost(fixed64, tag = "11")]
    observed_time_unix_nano: u64,
    #[prost(string, tag = "12")]
    event_name: String,
}

/// `opentelemetry.proto.common.v1.AnyValue`: the one value a log record's body, or an
/// attribute, holds.
#[derive(Clone, PartialEq, ::prost::Message)]
struct AnyValue {
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4, 5, 6, 7")]
    value: Option<any_value::Value>,
}

/// The alternatives an [`AnyValue`] holds one of.
mod any_value {
    /// `opentelemetry.proto.common.v1.AnyValue.value`.
    ///
    /// Each variant is named for the FIELD of that definition it stands for — `string_value`,
    /// `bool_value` and the rest — so the layout here reads as the protocol writes it; that
    /// every one of them ends in the enum's own name is the protocol's naming and not a
    /// choice to make here.
    #[allow(clippy::enum_variant_names)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Value {
        #[prost(string, tag = "1")]
        StringValue(String),
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        #[prost(int64, tag = "3")]
        IntValue(i64),
        #[prost(double, tag = "4")]
        DoubleValue(f64),
        #[prost(message, tag = "5")]
        ArrayValue(super::ArrayValue),
        #[prost(message, tag = "6")]
        KvlistValue(super::KeyValueList),
        #[prost(bytes = "vec", tag = "7")]
        BytesValue(Vec<u8>),
    }
}

/// `opentelemetry.proto.common.v1.ArrayValue`.
#[derive(Clone, PartialEq, ::prost::Message)]
struct ArrayValue {
    #[prost(message, repeated, tag = "1")]
    values: Vec<AnyValue>,
}

/// `opentelemetry.proto.common.v1.KeyValueList`.
#[derive(Clone, PartialEq, ::prost::Message)]
struct KeyValueList {
    #[prost(message, repeated, tag = "1")]
    values: Vec<KeyValue>,
}

/// `opentelemetry.proto.common.v1.KeyValue`.
#[derive(Clone, PartialEq, ::prost::Message)]
struct KeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<AnyValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the canonical code a refusal carries is the one the gRPC mapping states for the
    /// HTTP status it is answered with.
    #[test]
    fn each_refused_status_carries_the_code_the_mapping_states() {
        let rows: &[(u16, i32)] = &[
            (400, INVALID_ARGUMENT),
            (401, PERMISSION_DENIED),
            (403, PERMISSION_DENIED),
            (404, NOT_FOUND),
            (429, RESOURCE_EXHAUSTED),
            (501, UNIMPLEMENTED),
            (503, UNAVAILABLE),
            (418, UNKNOWN),
        ];
        for (status, expected) in rows {
            let actual = rpc_code_for_status(*status);
            assert_eq!(
                actual, *expected,
                "case status {status}: expected code {expected}, got {actual}"
            );
        }
    }

    /// RULE: a refusal's body is the `google.rpc.Status` of its code and sentence, which is
    /// what a client reading that message back gets.
    #[test]
    fn a_refusal_body_decodes_to_the_code_and_the_sentence() {
        let body = status_body(PERMISSION_DENIED, "no");
        let read = Status::decode(body.as_slice()).expect("the body is a google.rpc.Status");
        assert_eq!(
            (read.code, read.message.as_str()),
            (PERMISSION_DENIED, "no"),
            "case the refusal: expected (7, \"no\"), got {read:?}"
        );
    }

    /// RULE: a fully accepted export is the EMPTY response message, and a partial one carries
    /// the refused count and the message stating it.
    #[test]
    fn an_accepted_export_carries_only_the_partial_success_it_has() {
        let whole = export_response_body(0, "ignored");
        assert!(
            whole.is_empty(),
            "case nothing refused: expected the empty message, got {whole:?}"
        );
        let partial = export_response_body(3, "why");
        let read = ExportLogsServiceResponse::decode(partial.as_slice())
            .expect("the body is an ExportLogsServiceResponse");
        let reported = read.partial_success.expect("a partial success is reported");
        assert_eq!(
            (reported.rejected_log_records, reported.error_message.as_str()),
            (3, "why"),
            "case three refused: got {reported:?}"
        );
    }

    /// RULE: every log record a protobuf export request nests is read, whichever resource and
    /// scope it was listed under, and each is projected onto the protocol's JSON encoding.
    #[test]
    fn every_nested_log_record_of_a_protobuf_request_is_read() {
        let text = |value: &str| AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        };
        let request = ExportLogsServiceRequest {
            resource_logs: vec![
                ResourceLogs {
                    scope_logs: vec![ScopeLogs {
                        log_records: vec![
                            LogRecord {
                                time_unix_nano: 1_700_000_000_000_000_000,
                                severity_text: "INFO".to_string(),
                                severity_number: 9,
                                body: Some(text("first")),
                                ..LogRecord::default()
                            },
                            LogRecord {
                                observed_time_unix_nano: 1_700_000_000_001_000_000,
                                body: Some(AnyValue {
                                    value: Some(any_value::Value::IntValue(7)),
                                }),
                                ..LogRecord::default()
                            },
                        ],
                    }],
                },
                ResourceLogs {
                    scope_logs: vec![ScopeLogs {
                        log_records: vec![LogRecord {
                            body: Some(text("second resource")),
                            attributes: vec![KeyValue {
                                key: "level".to_string(),
                                value: Some(text("WARN")),
                            }],
                            trace_id: vec![0x0a, 0x0b],
                            ..LogRecord::default()
                        }],
                    }],
                },
            ],
        };
        let read = export_request_log_records(&request.encode_to_vec(), 10)
            .expect("the body is an export request carrying records");
        let expected = vec![
            ::serde_json::json!({
                "timeUnixNano": "1700000000000000000",
                "severityNumber": 9,
                "severityText": "INFO",
                "body": {"stringValue": "first"},
            }),
            ::serde_json::json!({
                "observedTimeUnixNano": "1700000000001000000",
                "body": {"intValue": "7"},
            }),
            ::serde_json::json!({
                "body": {"stringValue": "second resource"},
                "attributes": [{"key": "level", "value": {"stringValue": "WARN"}}],
                "traceId": "0a0b",
            }),
        ];
        assert_eq!(
            read, expected,
            "case the nested records: expected {expected:?}, got {read:?}"
        );
    }

    /// RULE: a body that is not an export request carrying records is read as none at all.
    #[test]
    fn a_body_that_carries_no_record_reads_as_none() {
        for (case, body) in [
            ("text that is no protobuf message", b"not protobuf".to_vec()),
            ("an empty body", Vec::new()),
            (
                "an export request nesting no record",
                ExportLogsServiceRequest {
                    resource_logs: vec![ResourceLogs {
                        scope_logs: vec![ScopeLogs {
                            log_records: Vec::new(),
                        }],
                    }],
                }
                .encode_to_vec(),
            ),
        ] {
            let read = export_request_log_records(&body, 10);
            assert!(
                read.is_none(),
                "case {case:?}: expected no records at all, got {read:?}"
            );
        }
    }

    /// RULE: the walk over a caller-supplied value is bounded by its own depth limit, so a
    /// value nested past that bound projects to the empty object and nothing below it is
    /// walked — whatever depth the request chose.
    #[test]
    fn a_value_nested_past_the_bound_projects_to_the_empty_object() {
        // The deepest value the walk renders in full sits at `MAX_VALUE_DEPTH - 1`; the rows
        // span that level, the one at the bound and one far past it.
        let nested = |levels: usize| {
            let mut value = AnyValue {
                value: Some(any_value::Value::StringValue("leaf".to_string())),
            };
            for _ in 0..levels {
                value = AnyValue {
                    value: Some(any_value::Value::ArrayValue(ArrayValue {
                        values: vec![value],
                    })),
                };
            }
            value
        };
        for (case, levels, reaches_leaf) in [
            ("one level", 1, true),
            ("one level short of the bound", MAX_VALUE_DEPTH - 1, true),
            ("exactly the bound", MAX_VALUE_DEPTH, false),
            ("far past the bound", MAX_VALUE_DEPTH * 4, false),
        ] {
            let rendered = any_value_document(&nested(levels)).to_string();
            assert_eq!(
                rendered.contains("leaf"),
                reaches_leaf,
                "case {case:?} ({levels} levels): expected the leaf to be reached: \
                 {reaches_leaf}, got {rendered}"
            );
            // However deep the value, the projection ends: nothing below the bound is walked,
            // so the rendered document is at most that many levels of `arrayValue`.
            let depth = rendered.matches("arrayValue").count();
            assert!(
                depth <= MAX_VALUE_DEPTH,
                "case {case:?} ({levels} levels): expected at most {MAX_VALUE_DEPTH} levels \
                 walked, got {depth}"
            );
        }
    }
}
