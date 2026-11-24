// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT A BATCH posted to one of this service's HTTP ingestion
//! endpoints is answered with, as the number of its records that were not ingested varies.
//!
//! RULE under test: the answer to a batch of records taken over an HTTP ingestion endpoint
//! reports how many of the records THE REQUEST CARRIED were not ingested — a batch none of
//! which was refused is answered `200` with an empty JSON object, one some of which was
//! refused is answered `200` with that batch's partial success, naming the count of the
//! refused records and counting them per class of the ingestion window, and one no record of
//! which could be ingested is refused `400` with the sentence that says so — and exactly the
//! records that WERE ingested are read back through `GetLogEvents`.
//!
//! The rows span what the rule generalises over: which endpoint takes the batch, how many
//! records it carries, how many of them are refused (none, one, two, four, all), WHY each is
//! refused (no log event in the record, an instant ahead of the clock, an instant past the
//! ingestion horizon) and whether one batch mixes several of those reasons at once — so the
//! reported count is the number refused rather than a constant, and the per-class counts are
//! of the whole batch rather than of its first refusal.
//!
//! Each case drives the endpoint the way a client does — an unsigned `POST` carrying an
//! `Authorization: Bearer` credential IAM issued — and reads back what was stored, so a
//! record counted as accepted that was not stored cannot pass.

use serde_json::{json, Value};

use crate::delivery_identity_authorization::{form_encode, Emulator};
use crate::harness::now_ms;
use crate::http_ingestion_endpoints::{
    api_key, ingestible_group, stored, COLLECTOR_PATH, LOGS_API_KEY_SERVICE, STRUCTURED_JSON_PATH,
};

/// The body a batch every record of which was ingested is answered with.
const ACCEPTED: &str = "{}";

/// The whole sentence a batch with nothing to ingest is refused with.
const ALL_EVENTS_INVALID: &str = "All events were invalid";

/// Milliseconds in one day, for the instants the out-of-window rows state.
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Milliseconds in one hour, for the instant the ahead-of-the-clock rows state.
const HOUR_MS: i64 = 60 * 60 * 1_000;

/// What one batch must be answered with: `None` for a batch every record of which was
/// ingested, and otherwise the refused-record count with the per-class counts of the
/// ingestion window.
type Refused = Option<(u64, Value)>;

/// One row: the case, the endpoint, the records posted, what the answer must report, and the
/// documents `GetLogEvents` must read back, oldest first.
struct Row {
    case: &'static str,
    path: &'static str,
    records: Vec<Value>,
    refused: Refused,
    stored: Vec<Value>,
}

/// A record of the collector form carrying `event` as its log event, and the instant it
/// states in SECONDS where it states one.
fn collector(event: Value, at_ms: Option<i64>) -> Value {
    match at_ms {
        Some(at_ms) => json!({"event": event, "time": at_ms / 1_000}),
        None => json!({"event": event}),
    }
}

/// A record of the structured JSON form: a message and the instant it states, in
/// MILLISECONDS.
fn structured(message: &str, at_ms: i64) -> Value {
    json!({"timestamp": at_ms, "message": message})
}

/// The per-class counts of a batch the ingestion window refused `too_new` records ahead of
/// the clock, `too_old` past the horizon and `expired` before the group's retention period
/// in.
fn classes(too_new: u64, too_old: u64, expired: u64) -> Value {
    json!({
        "tooNewLogEventCount": too_new,
        "tooOldLogEventCount": too_old,
        "expiredLogEventCount": expired,
    })
}

/// POST `records` to the endpoint at `path` as the body a client ships — one document for a
/// batch of one, an array for a batch of several — and answer the status and the body
/// exactly as served.
async fn post(
    aws: &Emulator,
    path: &str,
    group: &str,
    stream: &str,
    credential: &str,
    records: &[Value],
) -> (u16, String) {
    let body = match records {
        [only] => only.to_string(),
        many => Value::Array(many.to_vec()).to_string(),
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "{path}?logGroup={}&logStream={}",
            form_encode(group),
            form_encode(stream)
        ))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {credential}"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed ingestion request");
    let (status, payload) = aws.answered(request).await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// RULE: the answer to a batch counts the records of the REQUEST that were not ingested,
/// whatever their number and whichever reasons refused them, and exactly the records that
/// were ingested are read back.
#[tokio::test]
async fn a_batch_reports_how_many_of_its_records_were_refused() {
    let aws = Emulator::new();
    let key = api_key(&aws, "counted-writer", LOGS_API_KEY_SERVICE).await;
    // Inside the ingestion window, and close enough together that no batch's accepted
    // events span more than the 24 hours one batch may cover.
    let valid = now_ms() - 60 * 1_000;
    let ahead = now_ms() + 3 * HOUR_MS;
    let ancient = now_ms() - 20 * DAY_MS;
    let older = now_ms() - 16 * DAY_MS;
    let rows: Vec<Row> = vec![
        Row {
            case: "one record, accepted",
            path: COLLECTOR_PATH,
            records: vec![collector(json!({"level": "INFO"}), Some(valid))],
            refused: None,
            stored: vec![collector(json!({"level": "INFO"}), Some(valid))],
        },
        Row {
            case: "three records, all accepted",
            path: STRUCTURED_JSON_PATH,
            records: vec![
                structured("one", valid),
                structured("two", valid + 1),
                structured("three", valid + 2),
            ],
            refused: None,
            stored: vec![
                structured("one", valid),
                structured("two", valid + 1),
                structured("three", valid + 2),
            ],
        },
        Row {
            case: "one of four refused ahead of the clock",
            path: STRUCTURED_JSON_PATH,
            records: vec![
                structured("kept", valid),
                structured("ahead", ahead),
                structured("kept too", valid + 1),
                structured("kept as well", valid + 2),
            ],
            refused: Some((1, classes(1, 0, 0))),
            stored: vec![
                structured("kept", valid),
                structured("kept too", valid + 1),
                structured("kept as well", valid + 2),
            ],
        },
        Row {
            case: "two of five refused past the ingestion horizon",
            path: STRUCTURED_JSON_PATH,
            records: vec![
                structured("ancient", ancient),
                structured("kept", valid),
                structured("older", older),
                structured("kept too", valid + 1),
                structured("kept as well", valid + 2),
            ],
            refused: Some((2, classes(0, 2, 0))),
            stored: vec![
                structured("kept", valid),
                structured("kept too", valid + 1),
                structured("kept as well", valid + 2),
            ],
        },
        Row {
            case: "four of six refused, two classes in one batch",
            path: STRUCTURED_JSON_PATH,
            records: vec![
                structured("ancient", ancient),
                structured("ahead", ahead),
                structured("kept", valid),
                structured("older", older),
                structured("further ahead", ahead + HOUR_MS),
                structured("kept too", valid + 1),
            ],
            refused: Some((4, classes(2, 2, 0))),
            stored: vec![structured("kept", valid), structured("kept too", valid + 1)],
        },
        Row {
            case: "two of four refused for carrying no log event",
            path: COLLECTOR_PATH,
            records: vec![
                collector(json!("kept"), Some(valid)),
                json!({"host": "no event here"}),
                collector(json!("kept too"), Some(valid + 1_000)),
                json!({"fields": {"host": "nor here"}}),
            ],
            refused: Some((2, classes(0, 0, 0))),
            stored: vec![
                collector(json!("kept"), Some(valid)),
                collector(json!("kept too"), Some(valid + 1_000)),
            ],
        },
        Row {
            case: "two of three refused, one for its instant and one for carrying no event",
            path: COLLECTOR_PATH,
            records: vec![
                collector(json!("ancient"), Some(ancient)),
                collector(json!("kept"), Some(valid)),
                json!({"host": "no event here"}),
            ],
            refused: Some((2, classes(0, 1, 0))),
            stored: vec![collector(json!("kept"), Some(valid))],
        },
        Row {
            case: "every record refused",
            path: STRUCTURED_JSON_PATH,
            records: vec![structured("ancient", ancient), structured("older", older)],
            refused: Some((2, classes(0, 2, 0))),
            stored: Vec::new(),
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            path,
            records,
            refused,
            stored: expected,
        } = row;
        let group = format!("counted-{index}");
        let stream = format!("stream-{index}");
        ingestible_group(&aws, &group, &stream, None).await;
        let (status, answered) =
            post(&aws, path, &group, &stream, &key.secret, records).await;
        match refused {
            // Nothing refused: the batch is accepted whole, under the empty JSON object.
            None => assert_eq!(
                (status, answered.as_str()),
                (200, ACCEPTED),
                "case {case:?}: expected HTTP 200 with {ACCEPTED}, got {status} with {answered:?}"
            ),
            // Every record refused: there was nothing to ingest, which is a refusal of the
            // request rather than a partial success — whatever the per-class counts would be.
            Some((count, per_class)) if *count as usize >= records.len() => assert_eq!(
                (status, answered.as_str()),
                (
                    400,
                    json!({"message": ALL_EVENTS_INVALID}).to_string().as_str()
                ),
                "case {case:?}: expected the all-invalid refusal (per-class {per_class}), \
                 got {status} with {answered:?}"
            ),
            Some((count, per_class)) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a partly ingested batch is accepted, got {status} with \
                     {answered:?}"
                );
                let parsed: Value = serde_json::from_str(&answered).unwrap_or_else(|_| {
                    panic!("case {case:?}: expected a JSON body, got {answered:?}")
                });
                let partial = parsed.get("partialSuccess").unwrap_or_else(|| {
                    panic!("case {case:?}: expected a partial success, got {parsed}")
                });
                assert_eq!(
                    partial.get("rejectedLogRecords").and_then(Value::as_u64),
                    Some(*count),
                    "case {case:?}: expected {count} refused records, got {partial}"
                );
                let reported = partial
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a partial success says why, got {partial}")
                    });
                let counts: Value = serde_json::from_str(reported).unwrap_or_else(|_| {
                    panic!("case {case:?}: expected the per-class counts, got {reported:?}")
                });
                assert_eq!(
                    &counts, per_class,
                    "case {case:?}: expected {per_class}, got {counts}"
                );
            }
        }
        let read_back = stored(&aws, &group, &stream).await;
        let documents: Vec<String> = expected.iter().map(Value::to_string).collect();
        assert_eq!(
            read_back, documents,
            "case {case:?}: expected {documents:?} ingested, got {read_back:?}"
        );
    }
}
