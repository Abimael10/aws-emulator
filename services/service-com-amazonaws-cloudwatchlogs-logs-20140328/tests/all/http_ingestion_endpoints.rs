// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the HTTP INGESTION endpoints of CloudWatch Logs.
//!
//! RULE under test: CloudWatch Logs takes log records over two HTTP endpoints of its own —
//! `POST /services/collector/event`, where a record carries its log event in `event`, and
//! `POST /ingest/json`, where a record spells a `timestamp` and a `message` — each
//! authenticated by a CloudWatch Logs API key the account issued (an IAM service-specific
//! credential for `logs.amazonaws.com`, presented as a bearer credential) and addressed to
//! the log group and log stream its query string names, the group having had bearer token
//! authentication enabled on it. Each record is stored as the document it arrived as, and
//! the batch is answered with an empty JSON object when every record of it was ingested, a
//! partial success naming how many records were refused and per class how many when only
//! some were, and `400 {"message":"All events were invalid"}` when none was. A request this
//! endpoint may not ingest for is `403 {"message":"Forbidden"}` and stores nothing.//!
//! Each case drives the endpoints the way a client does — an unsigned `POST` with an
//! `Authorization: Bearer` header — and reads what was ingested back through
//! `GetLogEvents`, so a record that was not stored cannot pass. The API key and the IAM user
//! that holds it are made with IAM's OWN operations over the same engine, so the credential
//! the endpoint authenticates is the one IAM issued.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{form_encode, statement, xml_element, Emulator};
use crate::harness::{messages, now_ms};

/// The two paths the record-shaped endpoints are served at.
///
/// `pub(crate)` because the suite about the OpenTelemetry form drives the same surface and
/// states these paths once rather than transcribing them again.
pub(crate) const COLLECTOR_PATH: &str = "/services/collector/event";
pub(crate) const STRUCTURED_JSON_PATH: &str = "/ingest/json";

/// The IAM `ServiceName` a CloudWatch Logs API key is created for.
pub(crate) const LOGS_API_KEY_SERVICE: &str = "logs.amazonaws.com";

/// A service whose service-specific credentials are NOT CloudWatch Logs API keys, for the
/// row that presents one of those instead.
const OTHER_API_KEY_SERVICE: &str = "cassandra.amazonaws.com";

/// The IAM action an ingestion over these endpoints performs, as a policy statement spells
/// it. Stated here independently of the crate under test.
const INGEST_ACTION: &str = "logs:PutLogEvents";

/// IAM's awsQuery API version.
const IAM_VERSION: &str = "2010-05-08";

/// The body a fully accepted batch is answered with.
const ACCEPTED: &str = "{}";

/// The whole sentence a batch with nothing to ingest is refused with.
const ALL_EVENTS_INVALID: &str = "All events were invalid";

/// The whole sentence a request this endpoint may not ingest for is refused with.
const FORBIDDEN: &str = "Forbidden";

/// Milliseconds in one day, for the timestamps the ingestion window rows state.
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// One IAM user holding one CloudWatch Logs API key: the key's secret, which is the bearer
/// credential a client presents, and the credential's id, so a row can make it inactive or
/// delete it.
///
/// `pub(crate)` because the OpenTelemetry suite authenticates with a key made the same way.
pub(crate) struct ApiKey {
    pub(crate) user: String,
    pub(crate) id: String,
    pub(crate) secret: String,
}

/// Create an IAM user holding the grant every ingesting client needs — `logs:PutLogEvents`
/// on this service's resources — and issue it a service-specific credential for
/// `service_name`.
pub(crate) async fn api_key(aws: &Emulator, user: &str, service_name: &str) -> ApiKey {
    api_key_granted(
        aws,
        user,
        service_name,
        Some(statement("Allow", INGEST_ACTION, "*")),
    )
    .await
}

/// [`api_key`], with the identity-based policy the user holds stated by the case: the grant
/// an ingestion needs, a document that withholds or denies it, or none at all.
async fn api_key_granted(
    aws: &Emulator,
    user: &str,
    service_name: &str,
    grant: Option<String>,
) -> ApiKey {
    aws.query(
        "iam",
        IAM_VERSION,
        "CreateUser",
        &[("UserName", user.to_string())],
    )
    .await;
    if let Some(document) = grant {
        aws.query(
            "iam",
            IAM_VERSION,
            "PutUserPolicy",
            &[
                ("UserName", user.to_string()),
                ("PolicyName", "ingest".to_string()),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    let created = aws
        .query(
            "iam",
            IAM_VERSION,
            "CreateServiceSpecificCredential",
            &[
                ("UserName", user.to_string()),
                ("ServiceName", service_name.to_string()),
            ],
        )
        .await;
    // A long-term API key reports its secret in `ServiceCredentialSecret`; the
    // user-name-and-password form reports it in `ServicePassword`. Either is the secret
    // half a client would present.
    let secret = xml_element(&created, "ServiceCredentialSecret")
        .or_else(|| xml_element(&created, "ServicePassword"))
        .unwrap_or_else(|| {
            panic!("case {service_name}: the create reports a secret, got {created}")
        });
    let id = xml_element(&created, "ServiceSpecificCredentialId")
        .unwrap_or_else(|| panic!("case {service_name}: the create reports an id, got {created}"));
    ApiKey {
        user: user.to_string(),
        id,
        secret,
    }
}

/// A log group with bearer token authentication enabled and one log stream in it, and a
/// retention period where one is given.
pub(crate) async fn ingestible_group(
    aws: &Emulator,
    group: &str,
    stream: &str,
    retention_in_days: Option<i64>,
) {
    aws.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    if let Some(days) = retention_in_days {
        aws.ok(
            "PutRetentionPolicy",
            &json!({"logGroupName": group, "retentionInDays": days}),
        )
        .await;
    }
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    bearer_token_authentication(aws, group, true).await;
}

/// Enable or disable bearer token authentication on `group`, through the operation that
/// owns that state.
pub(crate) async fn bearer_token_authentication(aws: &Emulator, group: &str, enabled: bool) {
    aws.ok(
        "PutBearerTokenAuthentication",
        &json!({"logGroupIdentifier": group, "bearerTokenAuthenticationEnabled": enabled}),
    )
    .await;
}

/// POST `body` to the endpoint at `path`, addressed to `group`/`stream`, presenting
/// `token` as a bearer credential where one is given, and answer the status and the body
/// exactly as served.
async fn post(
    aws: &Emulator,
    path: &str,
    group: &str,
    stream: &str,
    credential: Option<&str>,
    body: &str,
) -> (u16, String) {
    let uri = format!(
        "{path}?logGroup={}&logStream={}",
        form_encode(group),
        form_encode(stream)
    );
    let mut request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        request = request.header("authorization", credential);
    }
    let request = request
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed ingestion request");
    let (status, payload) = aws.answered(request).await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// The messages `GetLogEvents` reads back from `group`/`stream`, oldest first.
pub(crate) async fn stored(aws: &Emulator, group: &str, stream: &str) -> Vec<String> {
    let page = aws
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream, "startFromHead": true}),
        )
        .await;
    messages(&page)
}

/// The events `GetLogEvents` reads back, as `(timestamp, message)` pairs.
pub(crate) async fn stored_events(aws: &Emulator, group: &str, stream: &str) -> Vec<(i64, String)> {
    let page = aws
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream, "startFromHead": true}),
        )
        .await;
    page.get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetLogEvents must return events, got {page}"))
        .iter()
        .map(|event| {
            let timestamp = event
                .get("timestamp")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("every event carries a timestamp, got {event}"));
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("every event carries a message, got {event}"))
                .to_string();
            (timestamp, message)
        })
        .collect()
}

/// One row of the acceptance table: the case, the endpoint, the body posted, and the
/// documents that body must be read back as.
type AcceptedRow = (&'static str, &'static str, String, Vec<String>);

/// RULE: a record is stored as the document it arrived as, and a batch every record of
/// which was ingested is answered with an empty JSON object.
///
/// The rows span what the rule generalises over: which endpoint takes the batch, whether
/// the body is one record or an array of them, what the record's event member holds (a
/// string, an object, an array), what else the record carries beside it, and whether the
/// destination's name needs escaping in the query string.
#[tokio::test]
async fn every_accepted_record_is_read_back_as_the_document_it_arrived_as() {
    let aws = Emulator::new();
    let key = api_key(&aws, "hlc-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let stamp = now_ms() - 60 * 1_000;
    let rows: Vec<AcceptedRow> = vec![
        (
            "a string event",
            COLLECTOR_PATH,
            json!({"event": "one"}).to_string(),
            vec![r#"{"event":"one"}"#.to_string()],
        ),
        (
            "an object event beside its own metadata",
            COLLECTOR_PATH,
            r#"{"event": {"level": "WARN", "text": "disk"}, "host": "h1"}"#.to_string(),
            vec![r#"{"event":{"level":"WARN","text":"disk"},"host":"h1"}"#.to_string()],
        ),
        (
            "an array event",
            COLLECTOR_PATH,
            r#"{"event": [{"event": "a"}, {"event": "b"}]}"#.to_string(),
            vec![r#"{"event":[{"event":"a"},{"event":"b"}]}"#.to_string()],
        ),
        (
            "an array of records",
            COLLECTOR_PATH,
            r#"[{"event": "x"}, {"event": "y"}]"#.to_string(),
            vec![
                r#"{"event":"x"}"#.to_string(),
                r#"{"event":"y"}"#.to_string(),
            ],
        ),
        (
            "one structured record",
            STRUCTURED_JSON_PATH,
            format!(r#"{{"timestamp": {stamp}, "message": "m"}}"#),
            vec![format!(r#"{{"timestamp":{stamp},"message":"m"}}"#)],
        ),
        (
            "a structured batch",
            STRUCTURED_JSON_PATH,
            format!(
                r#"[{{"timestamp": {stamp}, "message": "one"}}, {{"timestamp": {}, "message": "two"}}]"#,
                stamp + 1
            ),
            vec![
                format!(r#"{{"timestamp":{stamp},"message":"one"}}"#),
                format!(r#"{{"timestamp":{},"message":"two"}}"#, stamp + 1),
            ],
        ),
        (
            "a destination whose name is escaped in the query",
            STRUCTURED_JSON_PATH,
            format!(r#"{{"timestamp": {stamp}, "message": "escaped"}}"#),
            vec![format!(r#"{{"timestamp":{stamp},"message":"escaped"}}"#)],
        ),
    ];
    for (index, (case, path, body, expected)) in rows.iter().enumerate() {
        // The last row addresses a group whose name carries a separator a client escapes.
        let group = if *case == "a destination whose name is escaped in the query" {
            "/http/ingested".to_string()
        } else {
            format!("ingested-{index}")
        };
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, None).await;
        let (status, answered) =
            post(&aws, path, &group, &stream, Some(&credential), body).await;
        assert_eq!(
            (status, answered.as_str()),
            (200, ACCEPTED),
            "case {case:?}: expected HTTP 200 with {ACCEPTED}, got {status} with {answered:?}"
        );
        let read_back = stored(&aws, &group, &stream).await;
        assert_eq!(
            &read_back, expected,
            "case {case:?}: expected {expected:?} read back, got {read_back:?}"
        );
    }
}

/// RULE: a record is ingested at the instant it states — seconds since the epoch in
/// `time` on the collector endpoint, milliseconds in `timestamp` on the structured JSON
/// one — and a record that states none is ingested at the instant it arrived.
#[tokio::test]
async fn a_record_is_ingested_at_the_instant_it_states() {
    let aws = Emulator::new();
    let key = api_key(&aws, "instant-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let seconds = (now_ms() - 10 * 60 * 1_000) / 1_000;
    let millis = now_ms() - 5 * 60 * 1_000;
    let rows: Vec<(&str, &str, String, Option<i64>)> = vec![
        (
            "seconds in time",
            COLLECTOR_PATH,
            format!(r#"{{"event": "stamped", "time": {seconds}}}"#),
            Some(seconds * 1_000),
        ),
        (
            "fractional seconds in time",
            COLLECTOR_PATH,
            format!(r#"{{"event": "fractional", "time": {seconds}.25}}"#),
            Some(seconds * 1_000 + 250),
        ),
        (
            "milliseconds in timestamp",
            STRUCTURED_JSON_PATH,
            format!(r#"{{"timestamp": {millis}, "message": "stamped"}}"#),
            Some(millis),
        ),
        (
            "no instant at all",
            COLLECTOR_PATH,
            r#"{"event": "unstamped"}"#.to_string(),
            None,
        ),
    ];
    for (index, (case, path, body, expected)) in rows.iter().enumerate() {
        let group = format!("stamped-{index}");
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, None).await;
        let before = now_ms();
        let (status, answered) = post(&aws, path, &group, &stream, Some(&credential), body).await;
        let after = now_ms();
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {answered:?}"
        );
        let events = stored_events(&aws, &group, &stream).await;
        let (stamped, _) = events
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: one record was ingested, got {events:?}"));
        match expected {
            Some(instant) => assert_eq!(
                stamped, instant,
                "case {case:?}: expected the stated instant {instant}, got {stamped}"
            ),
            // A record that states no instant of its own is stamped with the instant the
            // request was served, which this case bounds rather than predicts.
            None => assert!(
                (before..=after).contains(stamped),
                "case {case:?}: expected an instant in {before}..={after}, got {stamped}"
            ),
        }
    }
}

/// One row of the partial-success table: the case, the endpoint, the records posted, the
/// count the answer must report, the per-class counts it must carry, and what is read back.
type PartialRow = (
    &'static str,
    &'static str,
    String,
    usize,
    Value,
    Vec<String>,
    Option<i64>,
);

/// RULE: a batch only some of whose records were ingested is answered with its partial
/// success — the count of the records refused, and a document counting them per class of
/// the ingestion window — while the records it DID accept are stored.
///
/// The rows span the classes the window refuses by (a record ahead of the clock, one past
/// the 14-day horizon, one preceding the group's retention period) and a record that
/// carries no log event at all, which no class of the window refused.
#[tokio::test]
async fn a_batch_only_partly_ingested_reports_what_it_refused() {
    let aws = Emulator::new();
    let key = api_key(&aws, "partial-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let valid = now_ms() - 60 * 1_000;
    // The retention row's own pair: a record INSIDE a one-day retention period and one
    // just outside it, close enough together that the batch spans well under the 24 hours
    // one batch's in-window events may cover.
    let kept_under_retention = now_ms() - 23 * 60 * 60 * 1_000;
    let past_retention = now_ms() - 25 * 60 * 60 * 1_000;
    let rows: Vec<PartialRow> = vec![
        (
            "one record ahead of the clock",
            STRUCTURED_JSON_PATH,
            format!(
                r#"[{{"timestamp": {valid}, "message": "kept"}}, {{"timestamp": {}, "message": "ahead"}}]"#,
                now_ms() + 3 * 60 * 60 * 1_000
            ),
            1,
            json!({"tooNewLogEventCount": 1, "tooOldLogEventCount": 0, "expiredLogEventCount": 0}),
            vec![format!(r#"{{"timestamp":{valid},"message":"kept"}}"#)],
            None,
        ),
        (
            "one record past the ingestion horizon",
            STRUCTURED_JSON_PATH,
            format!(
                r#"[{{"timestamp": {}, "message": "ancient"}}, {{"timestamp": {valid}, "message": "kept"}}]"#,
                now_ms() - 15 * DAY_MS
            ),
            1,
            json!({"tooNewLogEventCount": 0, "tooOldLogEventCount": 1, "expiredLogEventCount": 0}),
            vec![format!(r#"{{"timestamp":{valid},"message":"kept"}}"#)],
            None,
        ),
        (
            "one record preceding the group's retention period",
            STRUCTURED_JSON_PATH,
            format!(
                r#"[{{"timestamp": {past_retention}, "message": "expired"}}, {{"timestamp": {kept_under_retention}, "message": "kept"}}]"#
            ),
            1,
            json!({"tooNewLogEventCount": 0, "tooOldLogEventCount": 0, "expiredLogEventCount": 1}),
            vec![format!(
                r#"{{"timestamp":{kept_under_retention},"message":"kept"}}"#
            )],
            Some(1),
        ),
        (
            "one record carrying no log event",
            COLLECTOR_PATH,
            r#"[{"event": "kept"}, {"host": "nothing to ingest"}]"#.to_string(),
            1,
            json!({"tooNewLogEventCount": 0, "tooOldLogEventCount": 0, "expiredLogEventCount": 0}),
            vec![r#"{"event":"kept"}"#.to_string()],
            None,
        ),
    ];
    for (index, (case, path, body, refused, per_class, kept, retention)) in rows.iter().enumerate() {
        let group = format!("partial-{index}");
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, *retention).await;
        let (status, answered) = post(&aws, path, &group, &stream, Some(&credential), body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a partly ingested batch is accepted, got {status} with {answered:?}"
        );
        let parsed: Value = serde_json::from_str(&answered)
            .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON body, got {answered:?}"));
        let partial = parsed
            .get("partialSuccess")
            .unwrap_or_else(|| panic!("case {case:?}: expected a partial success, got {parsed}"));
        assert_eq!(
            partial.get("rejectedLogRecords").and_then(Value::as_u64),
            Some(*refused as u64),
            "case {case:?}: expected {refused} refused records, got {partial}"
        );
        let reported = partial
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a partial success says why, got {partial}"));
        let counts: Value = serde_json::from_str(reported).unwrap_or_else(|_| {
            panic!("case {case:?}: expected the per-class counts, got {reported:?}")
        });
        assert_eq!(
            &counts, per_class,
            "case {case:?}: expected {per_class}, got {counts}"
        );
        let read_back = stored(&aws, &group, &stream).await;
        assert_eq!(
            &read_back, kept,
            "case {case:?}: expected {kept:?} ingested, got {read_back:?}"
        );
    }
}

/// RULE: a batch no record of which could be ingested is refused — `400` with the one
/// sentence that says so — and nothing of it is stored.
///
/// The rows span the ways a batch can carry nothing to ingest: a record with no log event
/// member, a body that is no JSON at all, an empty batch, every record outside the
/// ingestion window, and a request naming no destination.
#[tokio::test]
async fn a_batch_with_nothing_to_ingest_is_refused() {
    let aws = Emulator::new();
    let key = api_key(&aws, "refused-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let group = "nothing-to-ingest";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a record carrying no event member",
            COLLECTOR_PATH,
            r#"{"host": "h1"}"#.to_string(),
        ),
        (
            "a record carrying no message member",
            STRUCTURED_JSON_PATH,
            format!(r#"{{"timestamp": {}}}"#, now_ms()),
        ),
        (
            "a body that is not JSON",
            COLLECTOR_PATH,
            "event=one".to_string(),
        ),
        ("an empty batch", STRUCTURED_JSON_PATH, "[]".to_string()),
        (
            "every record outside the ingestion window",
            STRUCTURED_JSON_PATH,
            format!(
                r#"[{{"timestamp": {}, "message": "one"}}, {{"timestamp": {}, "message": "two"}}]"#,
                now_ms() - 20 * DAY_MS,
                now_ms() - 19 * DAY_MS
            ),
        ),
    ];
    for (case, path, body) in &rows {
        let (status, answered) = post(&aws, path, group, stream, Some(&credential), body).await;
        assert_eq!(
            (status, answered.as_str()),
            (400, json!({"message": ALL_EVENTS_INVALID}).to_string().as_str()),
            "case {case:?}: expected the all-invalid refusal, got {status} with {answered:?}"
        );
    }
    // A request naming no destination is refused for its ADDRESSING: the refusal names the
    // two query parameters that address one, rather than reporting the batch as empty.
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{COLLECTOR_PATH}?logGroup={group}"))
        .header("content-type", "application/json")
        .header("authorization", &credential)
        .body(bytes::Bytes::from(r#"{"event": "nowhere"}"#))
        .expect("a well-formed ingestion request");
    let (status, payload) = aws.answered(request).await;
    let answered = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        (status, answered.as_str()),
        (
            400,
            json!({
                "message": "The query string must name the destination of the log events: \
                            logGroup and logStream."
            })
            .to_string()
            .as_str()
        ),
        "case a request naming no log stream: got {status} with {answered:?}"
    );
    let read_back = stored(&aws, group, stream).await;
    assert!(
        read_back.is_empty(),
        "case nothing to ingest: expected an empty stream, got {read_back:?}"
    );
    // A destination the account does not hold is NOT FOUND, under the sentence the
    // ingestion itself refuses a missing resource with — not the nothing-to-ingest answer.
    for (case, missing_group, missing_stream) in [
        ("a log stream the group does not hold", group, "absent"),
        ("a log group the account does not hold", "absent", stream),
    ] {
        let (status, answered) = post(
            &aws,
            STRUCTURED_JSON_PATH,
            missing_group,
            missing_stream,
            Some(&credential),
            &format!(r#"{{"timestamp": {}, "message": "nowhere"}}"#, now_ms()),
        )
        .await;
        // A group that is not held has not enabled bearer token authentication either, so
        // that request is forbidden before any destination is resolved; a missing STREAM of
        // a held group is the not-found answer.
        let expected = if missing_group == group { 404 } else { 403 };
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} with {answered:?}"
        );
    }
}

/// RULE: one request carries at most the documented maximum number of log events in a
/// batch; a batch at that maximum is ingested and one past it is refused whole.
#[tokio::test]
async fn a_batch_over_the_documented_maximum_is_refused_whole() {
    let aws = Emulator::new();
    let key = api_key(&aws, "batch-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    // The maximum `PutLogEvents` documents for one batch, stated here independently of the
    // crate under test: "the maximum number of log events in a batch is 10,000", which is
    // also the modelled `@length 1..10000` of its `logEvents`.
    const MAXIMUM: usize = 10_000;
    /// How that documented sentence spells the bound.
    const MAXIMUM_SPELLED: &str = "10,000";
    let stamp = now_ms() - 60 * 1_000;
    let batch = |count: usize| {
        let records: Vec<String> = (0..count)
            .map(|index| format!(r#"{{"timestamp": {stamp}, "message": "m{index}"}}"#))
            .collect();
        format!("[{}]", records.join(","))
    };
    let rows: [(&str, usize, u16); 2] = [
        ("exactly the maximum", MAXIMUM, 200),
        ("one past the maximum", MAXIMUM + 1, 400),
    ];
    for (index, (case, count, expected)) in rows.iter().enumerate() {
        let group = format!("bounded-{index}");
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, None).await;
        let (status, answered) = post(
            &aws,
            STRUCTURED_JSON_PATH,
            &group,
            &stream,
            Some(&credential),
            &batch(*count),
        )
        .await;
        assert_eq!(
            status, *expected,
            "case {case:?}: expected HTTP {expected}, got {status} with {answered:?}"
        );
        let read_back = stored(&aws, &group, &stream).await;
        if *expected == 200 {
            assert_eq!(
                answered, ACCEPTED,
                "case {case:?}: a fully ingested batch answers {ACCEPTED}, got {answered:?}"
            );
            assert!(
                !read_back.is_empty(),
                "case {case:?}: the batch was ingested, got {} events",
                read_back.len()
            );
        } else {
            assert_eq!(
                answered,
                json!({"message": format!("The maximum number of log events in a batch is {MAXIMUM_SPELLED}.")})
                    .to_string(),
                "case {case:?}: expected the documented maximum, got {answered:?}"
            );
            assert!(
                read_back.is_empty(),
                "case {case:?}: a refused batch stores nothing, got {} events",
                read_back.len()
            );
        }
    }
}

/// RULE: only a request presenting an ACTIVE CloudWatch Logs API key the account issued,
/// addressed to a log group with bearer token authentication enabled, may ingest; every
/// other request is `403 {"message":"Forbidden"}` and stores nothing.
///
/// The rows span what can be wrong about the credential (absent, another scheme, a secret
/// the account never issued, a credential of another service, one made inactive, one
/// deleted) and about the destination (a group that never enabled bearer token
/// authentication, and one that has disabled it again), and close with the request that
/// IS admitted, so no row passes merely because nothing is ever ingested.
#[tokio::test]
async fn only_an_active_logs_api_key_on_a_bearer_enabled_group_may_ingest() {
    let aws = Emulator::new();
    let key = api_key(&aws, "gated-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let other = api_key(&aws, "other-writer", OTHER_API_KEY_SERVICE).await;
    let enabled = "gated-enabled";
    let never = "gated-never-enabled";
    let disabled = "gated-disabled";
    let stream = "stream";
    ingestible_group(&aws, enabled, stream, None).await;
    ingestible_group(&aws, disabled, stream, None).await;
    bearer_token_authentication(&aws, disabled, false).await;
    aws.ok("CreateLogGroup", &json!({"logGroupName": never}))
        .await;
    aws.ok(
        "CreateLogStream",
        &json!({"logGroupName": never, "logStreamName": stream}),
    )
    .await;
    let inactive = api_key(&aws, "inactive-writer", LOGS_API_KEY_SERVICE).await;
    aws.query(
        "iam",
        IAM_VERSION,
        "UpdateServiceSpecificCredential",
        &[
            ("UserName", inactive.user.clone()),
            ("ServiceSpecificCredentialId", inactive.id.clone()),
            ("Status", "Inactive".to_string()),
        ],
    )
    .await;
    let deleted = api_key(&aws, "deleted-writer", LOGS_API_KEY_SERVICE).await;
    aws.query(
        "iam",
        IAM_VERSION,
        "DeleteServiceSpecificCredential",
        &[
            ("UserName", deleted.user.clone()),
            ("ServiceSpecificCredentialId", deleted.id.clone()),
        ],
    )
    .await;
    // Three keys whose IAM user holds a different grant: none at all, an explicit deny of
    // the action, and a grant scoped to ANOTHER log group's streams.
    let ungranted = api_key_granted(&aws, "ungranted-writer", LOGS_API_KEY_SERVICE, None).await;
    let denied = api_key_granted(
        &aws,
        "denied-writer",
        LOGS_API_KEY_SERVICE,
        Some(statement("Deny", INGEST_ACTION, "*")),
    )
    .await;
    let elsewhere = api_key_granted(
        &aws,
        "elsewhere-writer",
        LOGS_API_KEY_SERVICE,
        Some(statement(
            "Allow",
            INGEST_ACTION,
            // Another group's streams, in any Region and account: the grant is real, and
            // it does not cover the stream these rows address.
            "arn:aws:logs:*:*:log-group:gated-elsewhere:log-stream:*",
        )),
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
            "a credential of another service",
            enabled,
            Some(format!("Bearer {}", other.secret)),
        ),
        (
            "a credential made inactive",
            enabled,
            Some(format!("Bearer {}", inactive.secret)),
        ),
        (
            "a credential that was deleted",
            enabled,
            Some(format!("Bearer {}", deleted.secret)),
        ),
        (
            "a user whose policies allow the action nowhere",
            enabled,
            Some(format!("Bearer {}", ungranted.secret)),
        ),
        (
            "a user denied the action",
            enabled,
            Some(format!("Bearer {}", denied.secret)),
        ),
        (
            "a user granted it on another group's streams",
            enabled,
            Some(format!("Bearer {}", elsewhere.secret)),
        ),
        (
            "a group that never enabled bearer token authentication",
            never,
            Some(credential.clone()),
        ),
        (
            "a group that has disabled it again",
            disabled,
            Some(credential.clone()),
        ),
    ];
    for (case, group, presented) in &rows {
        let (status, answered) = post(
            &aws,
            COLLECTOR_PATH,
            group,
            stream,
            presented.as_deref(),
            r#"{"event": "refused"}"#,
        )
        .await;
        assert_eq!(
            (status, answered.as_str()),
            (403, json!({"message": FORBIDDEN}).to_string().as_str()),
            "case {case:?}: expected the forbidden refusal, got {status} with {answered:?}"
        );
        let read_back = stored(&aws, group, stream).await;
        assert!(
            read_back.is_empty(),
            "case {case:?}: a refused request stores nothing, got {read_back:?}"
        );
    }
    // The admitted request, for contrast: the same body, on the same group, with the
    // account's own active CloudWatch Logs API key.
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        enabled,
        stream,
        Some(&credential),
        r#"{"event": "admitted"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case the admitted request: got {status} with {answered:?}"
    );
    let read_back = stored(&aws, enabled, stream).await;
    assert_eq!(
        read_back,
        vec![r#"{"event":"admitted"}"#.to_string()],
        "case the admitted request: expected the one record stored, got {read_back:?}"
    );
}

/// RULE: the secret a reset supersedes stops authenticating at once ("resetting the
/// password immediately invalidates the previous password"), the fresh one it reports
/// authenticates in its place, and a credential whose expiry has fallen due authenticates
/// nothing whether or not any IAM call has run since.
#[tokio::test]
async fn a_superseded_or_expired_secret_authenticates_nothing() {
    let aws = Emulator::new();
    let key = api_key(&aws, "rotating-writer", LOGS_API_KEY_SERVICE).await;
    let group = "rotated";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    // The secret the create reported ingests.
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        group,
        stream,
        Some(&format!("Bearer {}", key.secret)),
        r#"{"event": "before the reset"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case the created secret: got {status} with {answered:?}"
    );
    // The reset mints a fresh secret for the SAME credential.
    let reset = aws
        .query(
            "iam",
            IAM_VERSION,
            "ResetServiceSpecificCredential",
            &[
                ("UserName", key.user.clone()),
                ("ServiceSpecificCredentialId", key.id.clone()),
            ],
        )
        .await;
    let fresh = xml_element(&reset, "ServiceCredentialSecret")
        .unwrap_or_else(|| panic!("case the reset: it reports a fresh secret, got {reset}"));
    assert_ne!(
        fresh, key.secret,
        "case the reset: expected a secret other than the superseded one"
    );
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        group,
        stream,
        Some(&format!("Bearer {}", key.secret)),
        r#"{"event": "with the superseded secret"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (403, json!({"message": FORBIDDEN}).to_string().as_str()),
        "case the superseded secret: got {status} with {answered:?}"
    );
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        group,
        stream,
        Some(&format!("Bearer {fresh}")),
        r#"{"event": "after the reset"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case the fresh secret: got {status} with {answered:?}"
    );
    let read_back = stored(&aws, group, stream).await;
    assert_eq!(
        read_back,
        vec![
            r#"{"event":"before the reset"}"#.to_string(),
            r#"{"event":"after the reset"}"#.to_string(),
        ],
        "case the rotation: expected only the two admitted records, got {read_back:?}"
    );
    // And the EXPIRY the clock owes: no create can mint a credential that has already
    // expired (an age is counted forward from the create), so the instant is seeded in IAM's
    // own store and everything else goes over the wire.
    expire_credential(&aws, &key.id);
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        group,
        stream,
        Some(&format!("Bearer {fresh}")),
        r#"{"event": "after the expiry"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (403, json!({"message": FORBIDDEN}).to_string().as_str()),
        "case an expired credential: got {status} with {answered:?}"
    );
    let read_back = stored(&aws, group, stream).await;
    assert_eq!(
        read_back.len(),
        2,
        "case an expired credential: nothing more was ingested, got {read_back:?}"
    );
}

/// Stamp the credential `credential_id` with an `ExpirationDate` that has already passed —
/// the one state no request can produce, seeded in IAM's own store.
fn expire_credential(aws: &Emulator, credential_id: &str) {
    let state = aws
        .engine()
        .state::<service_com_amazonaws_iam_awsidentitymanagementv20100508::state::ComAmazonawsIamAwsidentitymanagementv20100508State>();
    let mut store = state.lock();
    let credential = store
        .service_specific_credentials
        .get_mut(credential_id)
        .unwrap_or_else(|| panic!("the credential {credential_id} is stored"));
    credential.expiration_date = Some(aws_smithy_types::DateTime::from_secs(now_ms() / 1_000 - 60));
}

/// RULE: these endpoints are served on POST, the method a client SHIPS records with; a
/// request of any other method to the same path is not an ingestion and ingests nothing.
#[tokio::test]
async fn only_a_post_to_those_paths_is_an_ingestion() {
    let aws = Emulator::new();
    let key = api_key(&aws, "method-writer", LOGS_API_KEY_SERVICE).await;
    let credential = format!("Bearer {}", key.secret);
    let group = "method-gated";
    let stream = "stream";
    ingestible_group(&aws, group, stream, None).await;
    for method in ["GET", "PUT", "DELETE", "PATCH"] {
        for path in [COLLECTOR_PATH, STRUCTURED_JSON_PATH] {
            let request = http::Request::builder()
                .method(method)
                .uri(format!(
                    "{path}?logGroup={}&logStream={}",
                    form_encode(group),
                    form_encode(stream)
                ))
                .header("content-type", "application/json")
                .header("authorization", &credential)
                .body(bytes::Bytes::from(r#"{"event": "wrong method"}"#))
                .expect("a well-formed request");
            let (status, payload) = aws.answered(request).await;
            let answered = String::from_utf8_lossy(&payload).to_string();
            assert_ne!(
                status, 200,
                "case {method} {path}: expected no ingestion, got {status} with {answered:?}"
            );
        }
    }
    let read_back = stored(&aws, group, stream).await;
    assert!(
        read_back.is_empty(),
        "case a method that is not POST: nothing is ingested, got {read_back:?}"
    );
    // The same path and body on POST, for contrast.
    let (status, answered) = post(
        &aws,
        COLLECTOR_PATH,
        group,
        stream,
        Some(&credential),
        r#"{"event": "right method"}"#,
    )
    .await;
    assert_eq!(
        (status, answered.as_str()),
        (200, ACCEPTED),
        "case POST: got {status} with {answered:?}"
    );
    let read_back = stored(&aws, group, stream).await;
    assert_eq!(
        read_back,
        vec![r#"{"event":"right method"}"#.to_string()],
        "case POST: expected the one record stored, got {read_back:?}"
    );
}
