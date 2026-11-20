// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OpenTelemetry form of the CloudWatch Logs HTTP ingestion
//! surface, and for the status every request to that surface that carries no credential the
//! service can authenticate is refused with.
//!
//! The rules under test:
//!
//! * a request to a path of this ingestion surface that presents no credential this service
//!   can authenticate — and one addressed to a log group that has not had bearer token
//!   authentication enabled on it — is REFUSED, with the forbidden status and this
//!   surface's one-member refusal body, rather than answered as a path nothing serves;
//! * `POST /v1/logs` takes one OTLP logs export document, whose log records — every one of
//!   them, however they are spread over the document's resources and scopes — are the
//!   events ingested, each stored as the document it arrived as and timestamped by the
//!   nanoseconds-since-the-epoch instant it states;
//! * a request names its destination in the headers CloudWatch Logs reads it from or in the
//!   query string, and either way the records reach the same log stream.
//!
//! Each case drives the surface the way a client does — an unsigned request through the
//! router that mounts the service — and reads what was ingested back through
//! `GetLogEvents`, so a record that was not stored cannot pass. The API key and the IAM
//! user that holds it are made with IAM's OWN operations over the same engine.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{form_encode, Emulator};
use crate::harness::now_ms;
use crate::http_ingestion_endpoints::{
    api_key, bearer_token_authentication, ingestible_group, stored, stored_events,
    COLLECTOR_PATH, LOGS_API_KEY_SERVICE, STRUCTURED_JSON_PATH,
};

/// The path the OpenTelemetry form is served at: the path an OTLP/HTTP exporter ships a logs
/// export request to. Stated here independently of the crate under test.
const OTLP_PATH: &str = "/v1/logs";

/// The headers a client names its destination in.
const LOG_GROUP_HEADER: &str = "x-aws-log-group";
const LOG_STREAM_HEADER: &str = "x-aws-log-stream";

/// The status a request this surface may not ingest for is refused with.
const FORBIDDEN_STATUS: u16 = 403;

/// The whole sentence such a request is refused with.
const FORBIDDEN: &str = "Forbidden";

/// The body a fully accepted batch is answered with.
const ACCEPTED: &str = "{}";

/// The whole sentence a batch with nothing to ingest is refused with.
const ALL_EVENTS_INVALID: &str = "All events were invalid";

/// Nanoseconds in one millisecond, the factor between the unit an OTLP log record states its
/// instant in and the one a log event's timestamp is counted in.
const NANOS_PER_MILLI: i64 = 1_000_000;

/// How a request addresses its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The headers CloudWatch Logs reads the destination from.
    Headers,
    /// The query string.
    Query,
}

/// POST `body` to `path`, addressed to `group`/`stream` the way `addressing` says,
/// presenting `credential` as an `Authorization` header where one is given.
async fn post(
    aws: &Emulator,
    path: &str,
    group: &str,
    stream: &str,
    addressing: Addressing,
    credential: Option<&str>,
    body: &str,
) -> (u16, String) {
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
        .header("content-type", "application/json");
    if addressing == Addressing::Headers {
        request = request
            .header(LOG_GROUP_HEADER, group)
            .header(LOG_STREAM_HEADER, stream);
    }
    if let Some(credential) = credential {
        request = request.header("authorization", credential);
    }
    let request = request
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed ingestion request");
    let (status, payload) = aws.answered(request).await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// One OTLP log record: the instant it states (in nanoseconds since the epoch, spelled as
/// the encoding spells a 64-bit integer) and the body that IS the log event.
fn log_record(nanos: Option<i64>, body: Value) -> Value {
    let mut record = json!({});
    if let Some(nanos) = nanos {
        record["timeUnixNano"] = json!(nanos.to_string());
    }
    record["body"] = body;
    record
}

/// One OTLP logs export document carrying `scopes` worth of log records, each scope's
/// records nested under its own resource.
fn export_document(resources: Vec<Vec<Value>>) -> String {
    let resource_logs: Vec<Value> = resources
        .into_iter()
        .map(|records| json!({"scopeLogs": [{"logRecords": records}]}))
        .collect();
    json!({"resourceLogs": resource_logs}).to_string()
}

/// RULE: a request to a path of this ingestion surface that this service may not ingest for
/// is REFUSED with the forbidden status and this surface's refusal body — never answered as
/// a path nothing serves.
///
/// The rows span every path of the surface, what can be wrong about the credential (absent,
/// another scheme, a secret the account never issued) and the destination that has not had
/// bearer token authentication enabled on it, and close with the request that IS admitted,
/// so no row passes merely because nothing is ever ingested.
#[tokio::test]
async fn an_unauthenticated_request_to_the_ingestion_surface_is_forbidden() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-gated-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let enabled = "otlp-gated-enabled";
    let not_enabled = "otlp-gated-not-enabled";
    let stream = "stream";
    ingestible_group(&aws, enabled, stream, None).await;
    ingestible_group(&aws, not_enabled, stream, None).await;
    bearer_token_authentication(&aws, not_enabled, false).await;

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
            "a group that has not enabled bearer token authentication",
            not_enabled,
            Some(credential.clone()),
        ),
    ];
    for path in [OTLP_PATH, COLLECTOR_PATH, STRUCTURED_JSON_PATH] {
        for (case, group, presented) in &rows {
            let (status, answered) = post(
                &aws,
                path,
                group,
                stream,
                Addressing::Query,
                presented.as_deref(),
                &export_document(vec![vec![log_record(None, json!({"stringValue": "refused"}))]]),
            )
            .await;
            assert_eq!(
                (status, answered.as_str()),
                (
                    FORBIDDEN_STATUS,
                    json!({"message": FORBIDDEN}).to_string().as_str()
                ),
                "case {path} {case:?}: expected the forbidden refusal, got {status} with \
                 {answered:?}"
            );
            let read_back = stored(&aws, group, stream).await;
            assert!(
                read_back.is_empty(),
                "case {path} {case:?}: a refused request ingests nothing, got {read_back:?}"
            );
        }
    }
    // The same path with the key the account issued, on a group that enabled bearer token
    // authentication: accepted, so the rows above are refused for their own reason.
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
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case the admitted request: got {status} with {answered:?}"
    );
}

/// RULE: every log record an OTLP export document nests is ingested, as the document it
/// arrived as, whichever resource and scope it was listed under.
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
        // The last row carries a record with nothing to ingest beside one to ingest, so its
        // batch is a partial success rather than a plain accept.
        let partial = expected.len() == 1 && case.starts_with("a record with no body");
        if partial {
            assert_eq!(
                status, 200,
                "case {case:?}: a batch with one record to ingest is accepted, got {status} \
                 with {answered:?}"
            );
            assert!(
                answered.contains("\"rejectedLogRecords\":1"),
                "case {case:?}: expected one record reported refused, got {answered:?}"
            );
        } else {
            assert_eq!(
                (status, answered.as_str()),
                (200, ACCEPTED),
                "case {case:?}: expected HTTP 200 with {ACCEPTED}, got {status} with \
                 {answered:?}"
            );
        }
        let read_back = stored(&aws, &group, &stream).await;
        assert_eq!(
            &read_back, expected,
            "case {case:?}: expected {expected:?} read back, got {read_back:?}"
        );
    }
}

/// RULE: a record is ingested at the instant it states, in the NANOSECONDS since the epoch
/// the OpenTelemetry encoding counts one in — the observed instant where it states no
/// `timeUnixNano`, and the instant it arrived where it states neither.
#[tokio::test]
async fn a_log_record_is_ingested_at_the_instant_it_states() {
    let aws = Emulator::new();
    let key = api_key(&aws, "otlp-instant-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let stated = now_ms() - 10 * 60 * 1_000;
    let observed = now_ms() - 5 * 60 * 1_000;
    let before = now_ms();
    let group = "otlp-instants";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    let body = json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [
        {"timeUnixNano": (stated * NANOS_PER_MILLI).to_string(), "body": {"stringValue": "stated"}},
        // A number rather than a string: the encoding admits either spelling.
        {"observedTimeUnixNano": observed * NANOS_PER_MILLI, "body": {"stringValue": "observed"}},
        {"body": {"stringValue": "unstamped"}},
    ]}]}]})
    .to_string();
    let (status, answered) = post(
        &aws,
        OTLP_PATH,
        group,
        stream,
        Addressing::Query,
        Some(&credential),
        &body,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case the three instants: got {status} with {answered:?}"
    );
    let events = stored_events(&aws, group, stream).await;
    let timestamps: Vec<i64> = events.iter().map(|(timestamp, _)| *timestamp).collect();
    assert_eq!(
        timestamps.len(),
        3,
        "case the three instants: expected three events, got {events:?}"
    );
    assert_eq!(
        (timestamps[0], timestamps[1]),
        (stated, observed),
        "case the stated instants: expected {stated} then {observed}, got {timestamps:?}"
    );
    assert!(
        timestamps[2] >= before,
        "case the unstamped record: expected the instant it arrived (at or after {before}), \
         got {timestamps:?}"
    );
}

/// RULE: a request names its destination in the headers or in the query string, and either
/// way its records reach the same log stream; a request that names neither addresses nothing.
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
            (status, answered.as_str()),
            (200, ACCEPTED),
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
    let answered = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 400,
        "case no destination at all: expected the addressing refusal, got {status} with \
         {answered:?}"
    );
    assert!(
        answered.contains("logGroup") && answered.contains("logStream"),
        "case no destination at all: the refusal names the two parameters that address one, \
         got {answered:?}"
    );
}

/// RULE: an export document that nests no log record at all carries nothing to ingest, and
/// is refused as a batch with nothing in it.
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
        ("a body that is not JSON at all", "not json".to_string()),
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
        assert_eq!(
            (status, answered.as_str()),
            (
                400,
                json!({"message": ALL_EVENTS_INVALID}).to_string().as_str()
            ),
            "case {case}: expected the empty-batch refusal, got {status} with {answered:?}"
        );
    }
    let read_back = stored(&aws, group, stream).await;
    assert!(
        read_back.is_empty(),
        "case every empty batch: nothing is ingested, got {read_back:?}"
    );
}
