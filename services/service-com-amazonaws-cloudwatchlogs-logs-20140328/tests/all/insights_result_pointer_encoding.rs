// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ENCODING of the `@ptr` a CloudWatch Logs Insights result row
//! carries: "the identifier for the log record. You can use the value of @ptr in a
//! GetLogRecord operation to get the full log record" (`GetQueryResults`).
//!
//! The rules under test:
//!
//! * a `@ptr` is written in the published pointer encoding — the `hotstore:` scheme prefix
//!   followed by the pointer's payload in base64 — so what a client reads off the wire carries
//!   no readable structure of the service holding the record;
//! * it is nevertheless a WORKING handle: each row's `@ptr` is its own, and
//!   `GetLogRecord` reads the whole record of that one event back through it;
//! * only that form is a pointer: the same payload presented without the scheme prefix, or in
//!   another encoding, addresses no record and is refused as a parameter specified incorrectly.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a value a
//! handler does not actually compute cannot pass.
//!
//! The table spans what the rule generalises over rather than one query: which stream of the
//! group the event was ingested into, the FORM of the ingested message (a JSON object and bare
//! text), and which columns the query that produced the row projected (an explicit `fields`
//! list, a `filter` over the raw message, and a query selecting no column at all).

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";
const RECORD: &str = "GetLogRecord";

/// The scheme every log record pointer is written under, which is the first half of the
/// encoding under test.
const POINTER_SCHEME: &str = "hotstore:";

/// The modelled error code a pointer in no such form is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The status a query that has finished reports (`QueryStatus`).
const COMPLETE: &str = "Complete";

/// The log group every case queries, and the two streams of it.
const GROUP: &str = "/insights/pointer-encoding";
const STREAM_A: &str = "stream-one";
const STREAM_B: &str = "stream-two";

/// The events the queries read, as `(stream, offset from the base instant, message)`: two forms
/// of message in two streams, so a row's pointer has to address one of four events.
const EVENTS: [(&str, i64, &str); 4] = [
    (STREAM_A, 1_000, r#"{"level":"ERROR","user":"amy"}"#),
    (STREAM_A, 2_000, "plain text WARN 42"),
    (STREAM_B, 3_000, r#"{"level":"INFO","user":"bob"}"#),
    (STREAM_B, 4_000, "plain text ERROR 7"),
];

/// How long one poll of a started query waits before looking again. A query advances on the
/// clock, so the test looks for the CONDITION rather than sleeping for a span.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a query is polled for before the test gives up — generous, because the only limit
/// under test is the encoding of a value, never how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// The log group holding [`EVENTS`], and the window a query of it is started with, in the epoch
/// SECONDS `StartQuery` documents for its range.
async fn group_with_events(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    // Aligned on a whole second, because a query's window is stated in whole SECONDS.
    let base = base_ms() / 1_000 * 1_000;
    for stream in [STREAM_A, STREAM_B] {
        logs.ok(
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": stream}),
        )
        .await;
        let events: Vec<(i64, &str)> = EVENTS
            .iter()
            .filter(|(into, _, _)| *into == stream)
            .map(|(_, offset, message)| (base + offset, *message))
            .collect();
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": stream,
                "logEvents": batch(&events),
            }),
        )
        .await;
    }
    (base / 1_000 - 60, now_ms() / 1_000 + 60)
}

/// The rows of a `GetQueryResults` answer, each as the field/value pairs it carries.
fn rows(answer: &Value) -> Vec<BTreeMap<String, String>> {
    answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("GetQueryResults answers a results list, got {answer}"))
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap_or_else(|| panic!("every result row is a list of fields, got {row}"))
                .iter()
                .map(|field| {
                    (
                        field["field"].as_str().unwrap_or_default().to_string(),
                        field["value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .collect()
}

/// The value of one field of one row, or the empty string when the row does not carry it.
fn value(row: &BTreeMap<String, String>, field: &str) -> String {
    row.get(field).cloned().unwrap_or_default()
}

/// Start `query_string` over [`GROUP`], poll until the query has COMPLETED and answer its
/// results.
async fn completed(case: &str, logs: &Logs, window: (i64, i64), query_string: &str) -> Value {
    let started = logs
        .ok(
            START,
            &json!({
                "logGroupName": GROUP,
                "startTime": window.0,
                "endTime": window.1,
                "queryString": query_string,
            }),
        )
        .await;
    let query_id = started["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartQuery answers a queryId, got {started}"))
        .to_string();

    // A query reaches `Complete` on the clock, so the answer is read once it HAS: look again
    // until it has, rather than waiting for a span and hoping.
    let giving_up_at = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let answer = logs.ok(RESULTS, &json!({"queryId": query_id})).await;
        if answer["status"].as_str() == Some(COMPLETE) {
            return answer;
        }
        assert!(
            std::time::Instant::now() < giving_up_at,
            "case {case:?}: the started query reached {COMPLETE}, got {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `body` is a base64 payload: it is read with the same maintained decoder a client
/// would use, so what the test accepts is what a client can decode.
fn is_base64(body: &str) -> bool {
    !body.is_empty() && ::aws_smithy_types::base64::decode(body).is_ok()
}

/// Every row's `@ptr` is a `hotstore:`-prefixed base64 token, and every one of them retrieves
/// the whole record of its own event.
#[tokio::test]
async fn a_result_rows_pointer_is_a_scheme_prefixed_base64_token() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    // (case, the query string, how many rows of log records it must produce)
    let cases: Vec<(&str, &str, usize)> = vec![
        (
            "a query projecting the message and the stream",
            "fields @message, @logStream",
            EVENTS.len(),
        ),
        (
            "a query projecting only the instant of the event",
            "fields @timestamp",
            EVENTS.len(),
        ),
        (
            "a filter over the raw message",
            r"filter @message like /ERROR/",
            2,
        ),
        (
            "a filter on a field only the JSON events carry",
            r#"filter level = "INFO""#,
            1,
        ),
    ];

    let mut seen: Vec<String> = Vec::new();
    for (case, query_string, expected_rows) in cases {
        let answer = completed(case, &logs, window, query_string).await;
        let produced = rows(&answer);
        assert_eq!(
            produced.len(),
            expected_rows,
            "case {case:?}: expected {expected_rows} rows of log records, got {answer}"
        );

        let mut in_this_answer: Vec<String> = Vec::new();
        for (index, row) in produced.iter().enumerate() {
            let pointer = value(row, "@ptr");
            let body = pointer.strip_prefix(POINTER_SCHEME).unwrap_or_else(|| {
                panic!(
                    "case {case:?} row {index}: a @ptr is written under the {POINTER_SCHEME:?} \
                     scheme, got {pointer:?}"
                )
            });
            assert!(
                is_base64(body),
                "case {case:?} row {index}: a @ptr's payload is base64, got {pointer:?}"
            );
            assert!(
                !in_this_answer.contains(&pointer),
                "case {case:?} row {index}: every row carries its OWN @ptr, got {pointer:?} \
                 twice in {produced:?}"
            );
            in_this_answer.push(pointer.clone());

            // The pointer is a working handle: it retrieves the whole record of its event, and
            // that record carries the same pointer.
            let retrieved = logs
                .ok(RECORD, &json!({"logRecordPointer": pointer.clone()}))
                .await;
            let fields: BTreeMap<String, String> = retrieved["logRecord"]
                .as_object()
                .unwrap_or_else(|| {
                    panic!("case {case:?} row {index}: a record is a field map, got {retrieved}")
                })
                .iter()
                .map(|(field, held)| (field.clone(), held.as_str().unwrap_or_default().to_string()))
                .collect();
            assert_eq!(
                value(&fields, "@ptr"),
                pointer,
                "case {case:?} row {index}: the record carries the pointer it was reached by, \
                 got {retrieved}"
            );
            // "@log — a log group identifier … in the form of account-id:log-group-name"
            // (`GetLogGroupFields`), so the record names the group the query read.
            let held_in = value(&fields, "@log");
            assert!(
                held_in.ends_with(GROUP),
                "case {case:?} row {index}: the record is one of the queried group's, got \
                 {retrieved}"
            );
            let message = value(&fields, "@message");
            assert!(
                EVENTS
                    .iter()
                    .any(|(stream, _, ingested)| *ingested == message
                        && *stream == value(&fields, "@logStream")),
                "case {case:?} row {index}: the record is one of the ingested events, got \
                 {retrieved}"
            );
            // A row that projected the message names the same event as the record does.
            let projected = value(row, "@message");
            assert!(
                projected.is_empty() || projected == message,
                "case {case:?} row {index}: the row and its record carry one @message, got \
                 {projected:?} and {message:?}"
            );
            seen.push(pointer);
        }
    }

    // The queries above overlap, so the same event is pointed at from several answers: a
    // pointer is the EVENT's, not the answer's.
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        EVENTS.len(),
        "case the pointers of every answer: one pointer per ingested event, got {seen:?}"
    );
}

/// A value carrying the same payload in another form is no pointer: the scheme is part of the
/// encoding, and nothing else is read as one.
#[tokio::test]
async fn a_payload_outside_that_encoding_is_not_a_pointer() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;
    let case = "the pointer forms";
    let answer = completed(case, &logs, window, "fields @message").await;
    let pointer = rows(&answer)
        .first()
        .map(|row| value(row, "@ptr"))
        .unwrap_or_else(|| panic!("case {case:?}: the query produced a row, got {answer}"));
    let body = pointer
        .strip_prefix(POINTER_SCHEME)
        .unwrap_or_else(|| panic!("case {case:?}: a @ptr carries the scheme, got {pointer:?}"));

    // (case, the value presented as a pointer) — each a value a client could form from the one
    // the service issued, and none of them the encoding it issues.
    let forms: Vec<(&str, String)> = vec![
        ("the payload without the scheme", body.to_string()),
        (
            "the payload under another scheme",
            format!("coldstore:{body}"),
        ),
        (
            "the scheme twice over",
            format!("{POINTER_SCHEME}{pointer}"),
        ),
        (
            "the payload hex-encoded instead of base64",
            format!("{POINTER_SCHEME}{}", ::hex::encode(body)),
        ),
        ("the scheme with no payload", POINTER_SCHEME.to_string()),
        (
            "the scheme with a payload outside the base64 alphabet",
            format!("{POINTER_SCHEME}not a payload!"),
        ),
    ];
    for (form, presented) in forms {
        let (code, message) = logs
            .refusal(RECORD, &json!({"logRecordPointer": presented.clone()}))
            .await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {form:?}: expected {INVALID_PARAMETER}, got {code} {message:?} for \
             {presented:?}"
        );
    }

    // ... and the form the service DOES issue still resolves, so the refusals above are about
    // the form and not about the operation.
    let retrieved = logs
        .ok(RECORD, &json!({"logRecordPointer": pointer.clone()}))
        .await;
    assert!(
        retrieved["logRecord"].is_object(),
        "case the issued pointer: it retrieves the record, got {retrieved}"
    );
}
