// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the COLUMNS a CloudWatch Logs Insights result carries when the
//! pipeline creates fields of its own.
//!
//! The rule under test: a field a pipeline command CREATES on the records — a capture a
//! `parse` extracts, a column a `lookup` joins — is an output column of the result, so it
//! reaches every row `GetQueryResults` reports; the default columns `@timestamp` and `@message`
//! are what a pipeline reporting NONE OF ITS OWN carries, so the first command to produce
//! columns replaces them and a later one adds to what the pipeline has already chosen. A later
//! `fields`/`display` names the columns itself, so a projection after such a stage narrows the
//! result to what it names. A row states only the columns the record it came from carries, so a
//! record a pattern did not match reports none of the created fields rather than empty ones,
//! and every row of log records carries the `@ptr` of its record.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! a handler does not actually compute cannot pass.
//!
//! The table spans what the rule generalises over rather than one query string: which form the
//! `parse` takes (the `"pattern" as a, b, c` glob and the `(?<name>…)` named-capture regex),
//! how many fields it creates, whether a projection precedes or follows it, which command
//! created the field, and whether the pattern matched the record at all. Any one query string
//! an issue happens to quote is a single row.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";

/// The log group every case queries, and the stream its events are written to.
const GROUP: &str = "/insights/created-columns";
const STREAM: &str = "stream-one";

/// The marker every seeded line carries, so a `filter` selects this suite's events and
/// nothing else.
const MARKER: &str = "PROBE";

/// The events every case queries, as `(offset from the base instant, message)` pairs: three
/// unstructured lines a `parse` reads values out of, and one that matches no pattern below —
/// which is what makes a row with none of the created fields a case of its own.
const EVENTS: [(i64, &str); 4] = [
    (1_000, "REQ PROBE svc=alpha lat=100 status=200"),
    (2_000, "REQ PROBE svc=beta lat=200 status=500"),
    (3_000, "REQ PROBE svc=gamma lat=90 status=200"),
    (4_000, "REQ PROBE heartbeat"),
];

/// The field a row of log records carries beyond the pipeline's own columns: "Only the fields
/// requested in the query are returned, along with a @ptr field, which is the identifier for
/// the log record" (`GetQueryResults`).
const POINTER: &str = "@ptr";

/// The columns a result carries before any command of the pipeline produces one of its own:
/// the event's instant and its message. A pipeline that DOES produce its own columns reports
/// those instead, so these are exactly what such a result must not carry.
const DEFAULT_COLUMNS: [&str; 2] = ["@timestamp", "@message"];

/// A log group holding [`EVENTS`], and the window a query of it is started with in the epoch
/// SECONDS `StartQuery` documents for its range.
async fn group_with_events(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    // Aligned on a whole second, because a query's window is stated in whole SECONDS.
    let base = base_ms() / 1_000 * 1_000;
    let events: Vec<(i64, &str)> = EVENTS
        .iter()
        .map(|(offset, message)| (base + offset, *message))
        .collect();
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": STREAM,
            "logEvents": batch(&events),
        }),
    )
    .await;
    (base / 1_000 - 60, now_ms() / 1_000 + 60)
}

/// How long a case waits, in all, for a started query to reach a terminal status: a deadline
/// generous enough for a loaded host, reached by POLLING the query rather than by sleeping for
/// a span the test guessed.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Start `query_string` over the standing group and window, poll it to `Complete` and answer
/// the results it reports.
async fn completed(logs: &Logs, case: &str, window: (i64, i64), query_string: &str) -> Value {
    let answer = logs
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
    let id = answer["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a start answers a queryId, got {answer}"))
        .to_string();
    let deadline = std::time::Instant::now() + COMPLETION_DEADLINE;
    loop {
        let results = logs.ok(RESULTS, &json!({"queryId": id})).await;
        let status = results["status"].as_str().unwrap_or_default().to_string();
        if status == "Complete" {
            return results;
        }
        assert!(
            std::time::Instant::now() < deadline && status != "Failed" && status != "Cancelled",
            "case {case:?}: the query must complete, got status {status:?} with {results}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rows of a `GetQueryResults` answer, each as the field/value pairs it carries.
fn rows(case: &str, answer: &Value) -> Vec<BTreeMap<String, String>> {
    answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: results is a list, got {answer}"))
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a row is a list of fields, got {row}"))
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

/// What one case expects of one row: the columns it must carry with those exact values, and
/// the columns it must NOT carry at all.
struct ExpectedRow<'a> {
    /// `(field, value)` per column the row must report.
    present: Vec<(&'a str, &'a str)>,
    /// Columns the row must report nothing for — a field no command of the pipeline created
    /// for this record, or one a later projection dropped.
    absent: Vec<&'a str>,
}

/// One case of the projection table: what the case is, the query string it starts, and the
/// rows it must produce.
type ProjectionCase<'a> = (&'a str, String, Vec<ExpectedRow<'a>>);

/// A row that must carry these pairs and nothing for the columns in `absent`.
fn carrying_only<'a>(present: &[(&'a str, &'a str)], absent: &[&'a str]) -> ExpectedRow<'a> {
    ExpectedRow {
        present: present.to_vec(),
        absent: absent.to_vec(),
    }
}

/// Assert that `query_string` produces exactly `expected`, row by row and column by column.
async fn assert_projection(
    logs: &Logs,
    window: (i64, i64),
    case: &str,
    query_string: &str,
    expected: &[ExpectedRow<'_>],
) {
    let answer = completed(logs, case, window, query_string).await;
    let produced = rows(case, &answer);
    assert_eq!(
        produced.len(),
        expected.len(),
        "case {case:?}: expected {} rows, got {produced:?}",
        expected.len()
    );
    for (index, wanted) in expected.iter().enumerate() {
        let row = &produced[index];
        for (field, value) in &wanted.present {
            assert_eq!(
                row.get(*field).map(String::as_str),
                Some(*value),
                "case {case:?}: row {index} must carry {field}={value:?}, got {row:?}"
            );
        }
        for field in &wanted.absent {
            assert!(
                !row.contains_key(*field),
                "case {case:?}: row {index} must carry nothing for {field}, got {row:?}"
            );
        }
        // Every row of log records identifies the record it came from.
        assert!(
            row.get(POINTER).is_some_and(|pointer| !pointer.is_empty()),
            "case {case:?}: row {index} must carry a {POINTER}, got {row:?}"
        );
    }
}

/// The fields a `parse` creates are the result's columns, in both forms the command takes and
/// however the rest of the pipeline reads them.
#[tokio::test]
async fn the_fields_a_parse_creates_are_the_results_columns() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    let cases: Vec<ProjectionCase<'_>> = vec![
        (
            "a glob parse naming three fields, with no projection of its own",
            format!(
                "filter @message like /{MARKER}/ | parse @message \"svc=* lat=* status=*\" \
                 as svc, lat, st | sort @timestamp asc"
            ),
            vec![
                // The pipeline produces its own columns, so the default ones it never asked
                // for are not reported beside them.
                carrying_only(
                    &[("lat", "100"), ("st", "200"), ("svc", "alpha")],
                    &DEFAULT_COLUMNS,
                ),
                carrying_only(
                    &[("lat", "200"), ("st", "500"), ("svc", "beta")],
                    &DEFAULT_COLUMNS,
                ),
                carrying_only(
                    &[("lat", "90"), ("st", "200"), ("svc", "gamma")],
                    &DEFAULT_COLUMNS,
                ),
                // The line the pattern does not match: a row of its own, carrying none of
                // the created columns rather than empty ones — and none of the default ones
                // either, so its only field is the pointer to its record.
                carrying_only(&[], &["svc", "lat", "st", "@message", "@timestamp"]),
            ],
        ),
        (
            "a named-capture regex parse, ordered by the parsed value",
            r"filter @message like /lat=/ | parse @message /lat=(?<lat>\d+)/ | sort lat desc"
                .to_string(),
            vec![
                carrying_only(&[("lat", "200")], &DEFAULT_COLUMNS),
                carrying_only(&[("lat", "100")], &DEFAULT_COLUMNS),
                carrying_only(&[("lat", "90")], &DEFAULT_COLUMNS),
            ],
        ),
        (
            "a regex parse naming two capture groups",
            "filter @message like /lat=/ | parse @message /svc=(?<svc>\\w+) \
             lat=(?<lat>\\d+)/ | sort svc asc"
                .to_string(),
            vec![
                carrying_only(&[("lat", "100"), ("svc", "alpha")], &DEFAULT_COLUMNS),
                carrying_only(&[("lat", "200"), ("svc", "beta")], &DEFAULT_COLUMNS),
                carrying_only(&[("lat", "90"), ("svc", "gamma")], &DEFAULT_COLUMNS),
            ],
        ),
        (
            "a glob parse naming one field, whose trailing star takes the rest of the line",
            "filter @message like /lat=/ | parse @message \"lat=*\" as tail \
             | sort @timestamp asc"
                .to_string(),
            vec![
                carrying_only(&[("tail", "100 status=200")], &DEFAULT_COLUMNS),
                carrying_only(&[("tail", "200 status=500")], &DEFAULT_COLUMNS),
                carrying_only(&[("tail", "90 status=200")], &DEFAULT_COLUMNS),
            ],
        ),
        (
            // A projection AFTER the parse names the columns itself, so the result carries
            // what it names and nothing else.
            "a display after the parse, which names the columns itself",
            "filter @message like /lat=/ | parse @message \"svc=* lat=* status=*\" \
             as svc, lat, st | display svc | sort svc asc"
                .to_string(),
            vec![
                carrying_only(&[("svc", "alpha")], &["lat", "st", "@message", "@timestamp"]),
                carrying_only(&[("svc", "beta")], &["lat", "st", "@message", "@timestamp"]),
                carrying_only(&[("svc", "gamma")], &["lat", "st", "@message", "@timestamp"]),
            ],
        ),
        (
            // A projection BEFORE the parse chose the columns, and the created field joins
            // them.
            "a projection before the parse, which the created field joins",
            "filter @message like /lat=/ | fields @message \
             | parse @message /lat=(?<lat>\\d+)/ | sort lat asc"
                .to_string(),
            vec![
                carrying_only(&[("@message", EVENTS[2].1), ("lat", "90")], &["@timestamp"]),
                carrying_only(&[("@message", EVENTS[0].1), ("lat", "100")], &["@timestamp"]),
                carrying_only(&[("@message", EVENTS[1].1), ("lat", "200")], &["@timestamp"]),
            ],
        ),
    ];

    for (case, query_string, expected) in cases {
        assert_projection(&logs, window, case, &query_string, &expected).await;
    }
}

/// A `lookup` joins reference data onto the records, and the columns it joins are the result's
/// columns too — the same rule, for a field another command creates.
#[tokio::test]
async fn the_columns_a_lookup_joins_are_the_results_columns() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;
    const TABLE: &str = "created_columns_teams";
    logs.ok(
        "CreateLookupTable",
        &json!({
            "lookupTableName": TABLE,
            "tableBody": "svc,team\nalpha,platform\nbeta,west\n",
        }),
    )
    .await;

    // `gamma` is in no row of the table, so its record is joined nothing: the rule is that a
    // row states the columns its record carries, not that every row carries every column.
    let query_string = format!(
        "filter @message like /lat=/ | parse @message \"svc=* lat=* status=*\" as svc, lat, st \
         | lookup {TABLE} svc OUTPUT team | sort svc asc"
    );
    assert_projection(
        &logs,
        window,
        "a lookup of a field the pipeline parsed out itself",
        &query_string,
        &[
            carrying_only(
                &[
                    ("lat", "100"),
                    ("st", "200"),
                    ("svc", "alpha"),
                    ("team", "platform"),
                ],
                &DEFAULT_COLUMNS,
            ),
            carrying_only(
                &[
                    ("lat", "200"),
                    ("st", "500"),
                    ("svc", "beta"),
                    ("team", "west"),
                ],
                &DEFAULT_COLUMNS,
            ),
            carrying_only(&[("lat", "90"), ("st", "200"), ("svc", "gamma")], &["team"]),
        ],
    )
    .await;
}

/// RULE, the other way round: a pipeline that produces NO columns of its own reports the
/// default ones, so the columns a result carries are the pipeline's own only where it has them.
#[tokio::test]
async fn a_pipeline_producing_no_columns_of_its_own_reports_the_default_ones() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    // (case, a pipeline whose commands produce no column of their own)
    let cases: [(&str, String); 3] = [
        ("a filter alone", format!("filter @message like /{MARKER}/")),
        (
            "a filter and a sort",
            format!("filter @message like /{MARKER}/ | sort @timestamp desc"),
        ),
        (
            "a filter and a bound on the rows",
            format!("filter @message like /{MARKER}/ | limit 2"),
        ),
    ];
    for (case, query_string) in cases {
        let answer = completed(&logs, case, window, &query_string).await;
        let produced = rows(case, &answer);
        let row = produced
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a row, got {produced:?}"));
        for column in DEFAULT_COLUMNS {
            assert!(
                row.contains_key(column),
                "case {case:?}: a pipeline naming no columns of its own reports {column}, got \
                 {row:?}"
            );
        }
        assert!(
            row.get(POINTER).is_some_and(|pointer| !pointer.is_empty()),
            "case {case:?}: the row identifies the record it came from, got {row:?}"
        );
    }
}
