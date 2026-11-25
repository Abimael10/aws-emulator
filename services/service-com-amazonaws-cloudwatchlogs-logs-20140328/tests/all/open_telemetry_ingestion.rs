// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OpenTelemetry form of the CloudWatch Logs HTTP ingestion
//! surface: what a client may ship to it, in which of the protocol's two encodings, and what
//! it reads back out of it.
//!
//! The rules under test:
//!
//! * `POST /v1/logs` serves OTLP/HTTP, so what it ANSWERS is that protocol's own message and
//!   not this service's JSON envelope: a refusal is a protobuf `google.rpc.Status` carrying
//!   the canonical code its class maps onto and the sentence stating it, and an acceptance is
//!   a protobuf `ExportLogsServiceResponse`, empty for a batch every record of which was
//!   ingested and carrying the partial success of one some of whose records were refused;
//! * a log group states THREE things about bearer token authentication, and the two that are
//!   not "enabled" are two different refusals: a group `PutBearerTokenAuthentication` was
//!   never called for does not authenticate the credential (`PERMISSION_DENIED`, 403), while
//!   one it explicitly DISABLED has stated that it does not serve the operation
//!   (`INVALID_ARGUMENT`, 400, naming that group);
//! * the export request is read in EITHER encoding the protocol defines — the JSON one and
//!   the protobuf one — and every log record it nests, however they are spread over the
//!   document's resources and scopes, is ingested, each stored as the document it arrived as
//!   and timestamped by the nanoseconds-since-the-epoch instant it states;
//! * a request names its destination in the headers CloudWatch Logs reads it from or in the
//!   query string, and either way the records reach the same log stream.
//!
//! Each case drives the surface the way a client does — an unsigned request through the
//! router that mounts the service — and reads what was ingested back through
//! `GetLogEvents`, so a record that was not stored cannot pass. The API key and the IAM
//! user that holds it are made with IAM's OWN operations over the same engine.

use prost::Message as _;
use serde_json::{json, Value};

use crate::delivery_identity_authorization::{form_encode, Emulator};
use crate::harness::now_ms;
use crate::http_ingestion_endpoints::{
    api_key, bearer_token_authentication, ingestible_group, stored, stored_events,
    unsupported_for_log_group, AUTHENTICATION_FAILED, COLLECTOR_PATH, LOGS_API_KEY_SERVICE,
    STRUCTURED_JSON_PATH,
};

/// The path the OpenTelemetry form is served at: the path an OTLP/HTTP exporter ships a logs
/// export request to. Stated here independently of the crate under test.
const OTLP_PATH: &str = "/v1/logs";

/// The headers a client names its destination in.
const LOG_GROUP_HEADER: &str = "x-aws-log-group";
const LOG_STREAM_HEADER: &str = "x-aws-log-stream";

/// The canonical codes of `google.rpc.Code` the refusals here carry, in that enumeration's own
/// numbering. Stated here independently of the crate under test.
const INVALID_ARGUMENT: i32 = 3;
/// See [`INVALID_ARGUMENT`].
const PERMISSION_DENIED: i32 = 7;

/// The whole sentence a batch with nothing to ingest is refused with.
const ALL_EVENTS_INVALID: &str = "All events were invalid";

/// Nanoseconds in one millisecond, the factor between the unit an OTLP log record states its
/// instant in and the one a log event's timestamp is counted in.
const NANOS_PER_MILLI: i64 = 1_000_000;

/// `google.rpc.Status`, as a client reading a refusal of this endpoint decodes it. Transcribed
/// here from that message's own definition rather than read from the crate under test.
#[derive(Clone, PartialEq, prost::Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsServiceResponse`, likewise.
#[derive(Clone, PartialEq, prost::Message)]
struct ExportLogsServiceResponse {
    #[prost(message, optional, tag = "1")]
    partial_success: Option<ExportLogsPartialSuccess>,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsPartialSuccess`.
#[derive(Clone, PartialEq, prost::Message)]
struct ExportLogsPartialSuccess {
    #[prost(int64, tag = "1")]
    rejected_log_records: i64,
    #[prost(string, tag = "2")]
    error_message: String,
}

/// `opentelemetry.proto.collector.logs.v1.ExportLogsServiceRequest`, as a client WRITING the
/// protobuf encoding builds one.
#[derive(Clone, PartialEq, prost::Message)]
struct ExportLogsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    resource_logs: Vec<ResourceLogs>,
}

/// `opentelemetry.proto.logs.v1.ResourceLogs`.
#[derive(Clone, PartialEq, prost::Message)]
struct ResourceLogs {
    #[prost(message, repeated, tag = "2")]
    scope_logs: Vec<ScopeLogs>,
}

/// `opentelemetry.proto.logs.v1.ScopeLogs`.
#[derive(Clone, PartialEq, prost::Message)]
struct ScopeLogs {
    #[prost(message, repeated, tag = "2")]
    log_records: Vec<LogRecord>,
}

/// `opentelemetry.proto.logs.v1.LogRecord`.
#[derive(Clone, PartialEq, prost::Message)]
struct LogRecord {
    #[prost(fixed64, tag = "1")]
    time_unix_nano: u64,
    #[prost(int32, tag = "2")]
    severity_number: i32,
    #[prost(string, tag = "3")]
    severity_text: String,
    #[prost(message, optional, tag = "5")]
    body: Option<AnyValue>,
    #[prost(fixed64, tag = "11")]
    observed_time_unix_nano: u64,
}

/// `opentelemetry.proto.common.v1.AnyValue`, carrying the one alternative a case needs.
#[derive(Clone, PartialEq, prost::Message)]
struct AnyValue {
    #[prost(string, optional, tag = "1")]
    string_value: Option<String>,
    #[prost(int64, optional, tag = "3")]
    int_value: Option<i64>,
}

/// An `AnyValue` holding `text`.
fn text_value(text: &str) -> AnyValue {
    AnyValue {
        string_value: Some(text.to_string()),
        int_value: None,
    }
}

/// How a request addresses its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The headers CloudWatch Logs reads the destination from.
    Headers,
    /// The query string.
    Query,
}

/// The BODY one request ships: the bytes and the media type they are carried under, which is
/// which of the protocol's two encodings they are in.
struct Payload {
    content_type: &'static str,
    body: Vec<u8>,
}

/// POST `payload` to `path`, addressed to `group`/`stream` the way `addressing` says,
/// presenting `credential` as an `Authorization` header where one is given. The answer is its
/// status and the bytes exactly as served.
async fn post_bytes(
    aws: &Emulator,
    path: &str,
    group: &str,
    stream: &str,
    addressing: Addressing,
    credential: Option<&str>,
    payload: Payload,
) -> (u16, bytes::Bytes) {
    let uri = match addressing {
        Addressing::Headers => path.to_string(),
        Addressing::Query => format!(
            "{path}?logGroup={}&logStream={}",
            form_encode(group),
            form_encode(stream)
        ),
    };
    let mut request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", payload.content_type);
    if addressing == Addressing::Headers {
        request = request
            .header(LOG_GROUP_HEADER, group)
            .header(LOG_STREAM_HEADER, stream);
    }
    if let Some(credential) = credential {
        request = request.header("authorization", credential);
    }
    let request = request
        .body(bytes::Bytes::from(payload.body))
        .expect("a well-formed ingestion request");
    aws.answered(request).await
}

/// [`post_bytes`] with a JSON body, which is the encoding most of these cases ship.
async fn post(
    aws: &Emulator,
    path: &str,
    group: &str,
    stream: &str,
    addressing: Addressing,
    credential: Option<&str>,
    body: &str,
) -> (u16, bytes::Bytes) {
    post_bytes(
        aws,
        path,
        group,
        stream,
        addressing,
        credential,
        Payload {
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        },
    )
    .await
}

/// The `(code, message)` a refusal body decodes to as a `google.rpc.Status` — what an OTLP
/// client reads to learn whether to retry or to re-authenticate.
fn rpc_status(case: &str, body: &[u8]) -> (i32, String) {
    let read = RpcStatus::decode(body).unwrap_or_else(|error| {
        panic!("case {case:?}: the refusal body must be a google.rpc.Status, got {body:?}: {error}")
    });
    (read.code, read.message)
}

/// The partial success an acceptance body reports, or `None` for the empty response message a
/// fully accepted batch is answered with.
fn export_response(case: &str, body: &[u8]) -> Option<(i64, String)> {
    let read = ExportLogsServiceResponse::decode(body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the answer must be an ExportLogsServiceResponse, got {body:?}: {error}"
        )
    });
    read.partial_success
        .map(|partial| (partial.rejected_log_records, partial.error_message))
}

/// One OTLP log record in the protocol's JSON encoding: the instant it states (in nanoseconds
/// since the epoch, spelled as the encoding spells a 64-bit integer) and the body that IS the
/// log event.
fn log_record(nanos: Option<i64>, body: Value) -> Value {
    let mut record = json!({});
    if let Some(nanos) = nanos {
        record["timeUnixNano"] = json!(nanos.to_string());
    }
    record["body"] = body;
    record
}

/// One OTLP logs export document in the JSON encoding, carrying `resources` worth of log
/// records, each resource's records nested under its own scope.
fn export_document(resources: Vec<Vec<Value>>) -> String {
    let resource_logs: Vec<Value> = resources
        .into_iter()
        .map(|records| json!({"scopeLogs": [{"logRecords": records}]}))
        .collect();
    json!({"resourceLogs": resource_logs}).to_string()
}

/// RULE: a request whose credential did not authenticate an ingestion for the destination it
/// addressed is refused `403` with the protobuf `google.rpc.Status` of `PERMISSION_DENIED` and
/// the sentence saying so — never answered as a path nothing serves, and never with a JSON
/// document an OTLP client decodes to nothing.
///
/// The rows span what can be wrong about the credential (absent, another scheme, a secret the
/// account never issued) and the destination that was never enabled, and close with the
/// request that IS admitted, so no row passes merely because nothing is ever ingested.
#[tokio::test]
async fn an_unauthenticated_request_to_the_otlp_endpoint_carries_a_rpc_status() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-gated-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let enabled = "otlp-gated-enabled";
    let never = "otlp-gated-never-enabled";
    let stream = "stream";
    ingestible_group(&aws, enabled, stream, None).await;
    aws.ok("CreateLogGroup", &json!({"logGroupName": never}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": never, "logStreamName": stream}),
    )
    .await;

    let rows: Vec<(&str, &str, Option<String>)> = vec![
        ("no credential at all", enabled, None),
        (
            "another authentication scheme",
            enabled,
            Some(format!("Basic {}", key.secret)),
        ),
        (
            "a secret the account never issued",
            enabled,
            Some("Bearer 0123456789abcdef0123456789abcdef01234567".to_string()),
        ),
        (
            "a group that was never enabled for bearer token authentication",
            never,
            Some(credential.clone()),
        ),
    ];
    for (case, group, presented) in &rows {
        let (status, answered) = post(
            &aws,
            OTLP_PATH,
            group,
            stream,
            Addressing::Query,
            presented.as_deref(),
            &export_document(vec![vec![log_record(None, json!({"stringValue": "refused"}))]]),
        )
        .await;
        let decoded = rpc_status(case, &answered);
        assert_eq!(
            (status, decoded.0, decoded.1.as_str()),
            (403, PERMISSION_DENIED, AUTHENTICATION_FAILED),
            "case {case:?}: expected the authentication refusal as a google.rpc.Status, got \
             {status} with {decoded:?}"
        );
        let read_back = stored(&aws, group, stream).await;
        assert!(
            read_back.is_empty(),
            "case {case:?}: a refused request ingests nothing, got {read_back:?}"
        );
    }
    // The same path with the key the account issued, on a group that enabled bearer token
    // authentication: accepted, so the rows above are refused for their own reason.
    let case = "the admitted request";
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        enabled,
        stream,
        Addressing::Query,
        Some(&credential),
        &export_document(vec![vec![log_record(None, json!({"stringValue": "admitted"}))]]),
    )
    .await;
    assert_eq!(
        (status, export_response(case, &answered)),
        (200, None),
        "case {case}: expected the empty export response, got {status} with {answered:?}"
    );
}

/// RULE: a log group states THREE things about bearer token authentication, and the two that
/// are not "enabled" are two DIFFERENT refusals: a group the operation was never called for
/// does not authenticate the credential, while one it explicitly disabled has stated that it
/// does not serve the operation at all — which is the status, the canonical code and the
/// sentence of its own class, the group it names being the one the request addressed.
///
/// The one request is sent around that state: before anything is stated, once it is enabled,
/// with a token the account never issued while it is enabled, and once it is disabled again —
/// so what decides is the group's state and nothing about the request.
#[tokio::test]
async fn the_same_export_follows_the_groups_bearer_token_state() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-lifecycle-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let group = "/otlp/lifecycle";
    let stream = "stream";
    aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    let body = export_document(vec![vec![log_record(
        Some((now_ms() - 60 * 1_000) * NANOS_PER_MILLI),
        json!({"stringValue": "lifecycle"}),
    )]]);

    // Before anything is stated for the group: the credential authenticates no ingestion for
    // it.
    let case = "before bearer token authentication is enabled";
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Headers,
        Some(&credential),
        &body,
    )
    .await;
    let decoded = rpc_status(case, &answered);
    assert_eq!(
        (status, decoded.0, decoded.1.as_str()),
        (403, PERMISSION_DENIED, AUTHENTICATION_FAILED),
        "case {case}: got {status} with {decoded:?}"
    );

    // Enabled: the identical request is taken, and the answer is the protocol's empty
    // response message.
    bearer_token_authentication(&aws, group, true).await;
    let case = "with bearer token authentication enabled";
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Headers,
        Some(&credential),
        &body,
    )
    .await;
    assert_eq!(
        (status, export_response(case, &answered)),
        (200, None),
        "case {case}: got {status} with {answered:?}"
    );

    // A token the account never issued, while the group is enabled: the same refusal as
    // before the group was enabled, because the answer says only that the credential did not
    // authenticate the ingestion.
    let case = "a token the account never issued while the group is enabled";
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Headers,
        Some("Bearer 89abcdef0123456789abcdef0123456789abcdef"),
        &body,
    )
    .await;
    let decoded = rpc_status(case, &answered);
    assert_eq!(
        (status, decoded.0, decoded.1.as_str()),
        (403, PERMISSION_DENIED, AUTHENTICATION_FAILED),
        "case {case}: got {status} with {decoded:?}"
    );

    // Disabled again: a group that has STATED it does not serve the operation, which is the
    // other refusal, and the sentence names that group.
    bearer_token_authentication(&aws, group, false).await;
    let case = "with bearer token authentication disabled again";
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Headers,
        Some(&credential),
        &body,
    )
    .await;
    let decoded = rpc_status(case, &answered);
    assert_eq!(
        (status, decoded.0, decoded.1.as_str()),
        (
            400,
            INVALID_ARGUMENT,
            unsupported_for_log_group(group).as_str()
        ),
        "case {case}: got {status} with {decoded:?}"
    );

    // Exactly the one admitted export was ingested.
    let read_back = stored(&aws, group, stream).await;
    assert_eq!(
        read_back.len(),
        1,
        "case the whole lifecycle: expected exactly the one admitted record, got {read_back:?}"
    );
}

/// RULE: the record-shaped endpoints of this surface are this service's own and keep answering
/// in its one-member JSON envelope — the protocol's protobuf messages are the OTLP endpoint's
/// and only its.
#[tokio::test]
async fn the_record_shaped_endpoints_answer_in_the_services_own_envelope() {
    let aws = Emulator::new();
    let key = api_key(&aws, "envelope-writer", LOGS_API_KEY_SERVICE).await;
    let stream = "stream";
    for (index, path) in [COLLECTOR_PATH, STRUCTURED_JSON_PATH].iter().enumerate() {
        let group = format!("envelope-{index}");
        aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
        aws.ok(
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
        let (status, answered) = post(
            &aws,
            path,
            &group,
            stream,
            Addressing::Query,
            Some(&format!("Bearer {}", key.secret)),
            r#"{"event": "refused"}"#,
        )
        .await;
        let body = String::from_utf8_lossy(&answered).to_string();
        assert_eq!(
            (status, body.as_str()),
            (
                403,
                json!({"message": AUTHENTICATION_FAILED}).to_string().as_str()
            ),
            "case {path}: expected the JSON envelope, got {status} with {body:?}"
        );
    }
}

/// RULE: every log record an export document nests is ingested, as the document it arrived
/// as, whichever resource and scope it was listed under.
///
/// The rows span what the rule generalises over: one record, several records under one
/// scope, records spread over two resources, each shape of `body` the encoding admits, and
/// a record carrying no body at all.
#[tokio::test]
async fn every_log_record_an_export_document_nests_is_ingested() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let stamp = now_ms() - 60 * 1_000;
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "one record",
            export_document(vec![vec![log_record(
                Some(stamp * NANOS_PER_MILLI),
                json!({"stringValue": "one"}),
            )]]),
            vec![format!(
                r#"{{"timeUnixNano":"{}","body":{{"stringValue":"one"}}}}"#,
                stamp * NANOS_PER_MILLI
            )],
        ),
        (
            "two records under one scope",
            export_document(vec![vec![
                log_record(Some(stamp * NANOS_PER_MILLI), json!({"stringValue": "a"})),
                log_record(
                    Some((stamp + 1) * NANOS_PER_MILLI),
                    json!({"stringValue": "b"}),
                ),
            ]]),
            vec![
                format!(
                    r#"{{"timeUnixNano":"{}","body":{{"stringValue":"a"}}}}"#,
                    stamp * NANOS_PER_MILLI
                ),
                format!(
                    r#"{{"timeUnixNano":"{}","body":{{"stringValue":"b"}}}}"#,
                    (stamp + 1) * NANOS_PER_MILLI
                ),
            ],
        ),
        (
            "records spread over two resources",
            export_document(vec![
                vec![log_record(
                    Some(stamp * NANOS_PER_MILLI),
                    json!({"stringValue": "first resource"}),
                )],
                vec![log_record(
                    Some((stamp + 2) * NANOS_PER_MILLI),
                    json!({"kvlistValue": {"values": [{"key": "level", "value": {"stringValue": "WARN"}}]}}),
                )],
            ]),
            vec![
                format!(
                    r#"{{"timeUnixNano":"{}","body":{{"stringValue":"first resource"}}}}"#,
                    stamp * NANOS_PER_MILLI
                ),
                format!(
                    r#"{{"timeUnixNano":"{}","body":{{"kvlistValue":{{"values":[{{"key":"level","value":{{"stringValue":"WARN"}}}}]}}}}}}"#,
                    (stamp + 2) * NANOS_PER_MILLI
                ),
            ],
        ),
        (
            "a record with no body beside one that has one",
            export_document(vec![vec![
                json!({"timeUnixNano": (stamp * NANOS_PER_MILLI).to_string(), "severityText": "INFO"}),
                log_record(Some(stamp * NANOS_PER_MILLI), json!({"intValue": "7"})),
            ]]),
            vec![format!(
                r#"{{"timeUnixNano":"{}","body":{{"intValue":"7"}}}}"#,
                stamp * NANOS_PER_MILLI
            )],
        ),
    ];
    for (index, (case, body, expected)) in rows.iter().enumerate() {
        let group = format!("otlp-ingested-{index}");
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, None).await;
        let (status, answered) = post(
            &aws,
            OTLP_PATH,
            &group,
            &stream,
            Addressing::Query,
            Some(&credential),
            body,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a batch with a record to ingest is accepted, got {status} with \
             {answered:?}"
        );
        // The last row carries a record with nothing to ingest beside one to ingest, so its
        // batch is a partial success rather than a plain accept, reported as the protocol's
        // own `partialSuccess`.
        let partial = expected.len() == 1 && case.starts_with("a record with no body");
        let reported = export_response(case, &answered);
        if partial {
            let (rejected, reason) = reported
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: expected a partial success, got none"));
            assert_eq!(
                rejected, 1,
                "case {case:?}: expected one record reported refused, got {reported:?}"
            );
            assert!(
                reason.contains("tooNewLogEventCount"),
                "case {case:?}: expected the per-class counts as the reason, got {reason:?}"
            );
        } else {
            assert_eq!(
                reported, None,
                "case {case:?}: expected the empty export response, got {reported:?}"
            );
        }
        let read_back = stored(&aws, &group, &stream).await;
        assert_eq!(
            &read_back, expected,
            "case {case:?}: expected {expected:?} read back, got {read_back:?}"
        );
    }
}

/// RULE: the export request is read in EITHER encoding the OpenTelemetry protocol defines, so
/// a client writing the PROTOBUF one is served exactly as one writing the JSON one — the
/// records it nested are ingested, projected onto that protocol's JSON encoding of a log
/// record.
///
/// The rows span what the protobuf encoding carries that the reader has to project: the
/// instant a record states and the observed one it falls back to, a string body and an
/// integer one, a record carrying severity beside its body, and records spread over two
/// resources.
#[tokio::test]
async fn an_export_request_in_the_protobuf_encoding_is_ingested() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-protobuf-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let stamp = now_ms() - 60 * 1_000;
    let nanos = |millis: i64| u64::try_from(millis * NANOS_PER_MILLI).expect("a positive instant");
    let group = "otlp-protobuf";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    let request = ExportLogsServiceRequest {
        resource_logs: vec![
            ResourceLogs {
                scope_logs: vec![ScopeLogs {
                    log_records: vec![
                        LogRecord {
                            time_unix_nano: nanos(stamp),
                            body: Some(text_value("protobuf one")),
                            ..LogRecord::default()
                        },
                        LogRecord {
                            observed_time_unix_nano: nanos(stamp + 1),
                            severity_number: 9,
                            severity_text: "INFO".to_string(),
                            body: Some(AnyValue {
                                string_value: None,
                                int_value: Some(7),
                            }),
                            ..LogRecord::default()
                        },
                    ],
                }],
            },
            ResourceLogs {
                scope_logs: vec![ScopeLogs {
                    log_records: vec![LogRecord {
                        time_unix_nano: nanos(stamp + 2),
                        body: Some(text_value("protobuf two")),
                        ..LogRecord::default()
                    }],
                }],
            },
        ],
    };
    let case = "an export request in the protobuf encoding";
    let (status, answered) = post_bytes(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Headers,
        Some(&credential),
        Payload {
            content_type: "application/x-protobuf",
            body: request.encode_to_vec(),
        },
    )
    .await;
    assert_eq!(
        (status, export_response(case, &answered)),
        (200, None),
        "case {case}: got {status} with {answered:?}"
    );
    let events = stored_events(&aws, group, stream).await;
    let expected: Vec<(i64, String)> = vec![
        (
            stamp,
            format!(
                r#"{{"timeUnixNano":"{}","body":{{"stringValue":"protobuf one"}}}}"#,
                stamp * NANOS_PER_MILLI
            ),
        ),
        (
            stamp + 1,
            format!(
                r#"{{"observedTimeUnixNano":"{}","severityNumber":9,"severityText":"INFO","body":{{"intValue":"7"}}}}"#,
                (stamp + 1) * NANOS_PER_MILLI
            ),
        ),
        (
            stamp + 2,
            format!(
                r#"{{"timeUnixNano":"{}","body":{{"stringValue":"protobuf two"}}}}"#,
                (stamp + 2) * NANOS_PER_MILLI
            ),
        ),
    ];
    assert_eq!(
        events, expected,
        "case {case}: expected {expected:?} read back, got {events:?}"
    );
}

/// RULE: a request names its destination in the headers or in the query string, and either
/// way its records reach the same log stream; a request that names neither addresses nothing,
/// and that refusal too travels as the protocol's own message.
#[tokio::test]
async fn a_destination_is_named_in_the_headers_or_in_the_query() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-addressing-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let group = "/otlp/addressed";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    for (case, addressing, text) in [
        ("the headers", Addressing::Headers, "by header"),
        ("the query string", Addressing::Query, "by query"),
    ] {
        let (status, answered) = post(
            &aws,
            OTLP_PATH,
            group,
            stream,
            addressing,
            Some(&credential),
            &export_document(vec![vec![log_record(None, json!({"stringValue": text}))]]),
        )
        .await;
        assert_eq!(
            (status, export_response(case, &answered)),
            (200, None),
            "case {case}: got {status} with {answered:?}"
        );
    }
    let read_back = stored(&aws, group, stream).await;
    assert_eq!(
        read_back,
        vec![
            r#"{"body":{"stringValue":"by header"}}"#.to_string(),
            r#"{"body":{"stringValue":"by query"}}"#.to_string(),
        ],
        "case both addressings: expected both records in the one stream, got {read_back:?}"
    );
    // A request naming neither addresses no destination at all.
    let request = http::Request::builder()
        .method("POST")
        .uri(OTLP_PATH)
        .header("content-type", "application/json")
        .header("authorization", &credential)
        .body(bytes::Bytes::from(export_document(vec![vec![log_record(
            None,
            json!({"stringValue": "unaddressed"}),
        )]])))
        .expect("a well-formed request");
    let (status, payload) = aws.answered(request).await;
    let case = "no destination at all";
    let (code, message) = rpc_status(case, &payload);
    assert_eq!(
        (status, code),
        (400, INVALID_ARGUMENT),
        "case {case}: expected the addressing refusal, got {status} with {message:?}"
    );
    assert!(
        message.contains("logGroup") && message.contains("logStream"),
        "case {case}: the refusal names the two parameters that address one, got {message:?}"
    );
}

/// RULE: an export document that nests no log record at all carries nothing to ingest, and
/// is refused as a batch with nothing in it — under the canonical code of that class and the
/// sentence that says so.
#[tokio::test]
async fn an_export_document_with_no_log_record_is_refused() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-empty-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let group = "otlp-empty";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    for (case, body) in [
        ("no resourceLogs at all", json!({}).to_string()),
        ("an empty resourceLogs", json!({"resourceLogs": []}).to_string()),
        (
            "a scope nesting no log record",
            json!({"resourceLogs": [{"scopeLogs": [{"logRecords": []}]}]}).to_string(),
        ),
        (
            "every record carrying no body",
            json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"severityText": "INFO"}]}]}]})
                .to_string(),
        ),
        ("a body in neither encoding at all", "not json".to_string()),
    ] {
        let (status, answered) = post(
            &aws,
            OTLP_PATH,
            group,
            stream,
            Addressing::Headers,
            Some(&credential),
            &body,
        )
        .await;
        let decoded = rpc_status(case, &answered);
        assert_eq!(
            (status, decoded.0, decoded.1.as_str()),
            (400, INVALID_ARGUMENT, ALL_EVENTS_INVALID),
            "case {case}: expected the empty-batch refusal, got {status} with {decoded:?}"
        );
    }
    let read_back = stored(&aws, group, stream).await;
    assert!(
        read_back.is_empty(),
        "case every empty batch: nothing is ingested, got {read_back:?}"
    );
}
