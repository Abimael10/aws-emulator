// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the COLUMN SET a CloudWatch Logs Insights result carries, read off the
//! rows `GetQueryResults` reports.
//!
//! The rule under test: a pipeline command that DEFINES fields — a `parse`'s captures, an
//! `eval`'s assignment, the columns a `lookup` joins — establishes the result's column set from
//! the fields it defined, so the default columns (`@timestamp`, `@message`) survive only for a
//! pipeline that defines and projects nothing at all; "Only the fields requested in the query
//! are returned, along with a @ptr field, which is the identifier for the log record"
//! (`GetQueryResults`). A row then states only the columns its own record carries, so a record
//! a defining command computed nothing for — a line the `parse` pattern does not match, a key
//! no lookup row joins, an expression with nothing to compute on — keeps NONE of those columns,
//! neither empty text nor a null, and all that is left of such a row is its pointer.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! a handler does not actually compute cannot pass. Each case asserts the row's WHOLE field
//! set, not the presence of one column, which is what makes a default column reported beside a
//! defined one a failure.
//!
//! The table spans what the rule generalises over rather than one query string: which command
//! defined the fields (`parse` in both its forms, `eval`, `lookup`), how many it defined (one,
//! two, three), which language the pipeline is written in (Logs Insights QL and OpenSearch
//! PPL), whether a record carries a value for them at all, and — as the counter-case — a
//! pipeline that defines nothing and so does carry the default columns.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";

/// The field a row of log records carries beyond the pipeline's own columns: "Only the fields
/// requested in the query are returned, along with a @ptr field, which is the identifier for
/// the log record" (`GetQueryResults`).
const POINTER: &str = "@ptr";

/// The columns a result carries before any command of the pipeline defines one: the event's
/// instant and its message.
const DEFAULT_COLUMNS: [&str; 2] = ["@timestamp", "@message"];

/// The marker every seeded line carries, so one `filter` selects a suite's own events.
const MARKER: &str = "COLSET";

/// How long a case waits, in all, for a started query to reach a terminal status: a deadline
/// generous enough for a loaded host, reached by POLLING the query rather than by sleeping for
/// a span the test guessed.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Stand up `group` with `stream` holding `events` — each `(offset from the base instant,
/// message)` — and answer the window a query of it is started with, in the epoch SECONDS
/// `StartQuery` documents for its range.
async fn seeded(logs: &Logs, group: &str, stream: &str, events: &[(i64, String)]) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    // Aligned on a whole second, because a query's window is stated in whole SECONDS.
    let base = base_ms() / 1_000 * 1_000;
    let stamped: Vec<(i64, &str)> = events
        .iter()
        .map(|(offset, message)| (base + offset, message.as_str()))
        .collect();
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": batch(&stamped),
        }),
    )
    .await;
    (base / 1_000 - 60, now_ms() / 1_000 + 60)
}

/// Start the query `request` names, poll it to `Complete` and answer the results it reports.
async fn completed(logs: &Logs, case: &str, request: &Value) -> Value {
    let answer = logs.ok(START, request).await;
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
                    // Both members of a ResultField are text, so a value of any other JSON
                    // shape is a defect of the response rather than a value to compare.
                    let read = |member: &str| {
                        field[member]
                            .as_str()
                            .unwrap_or_else(|| {
                                panic!(
                                    "case {case:?}: a result field's {member} is a string, got \
                                     {field}"
                                )
                            })
                            .to_string()
                    };
                    (read("field"), read("value"))
                })
                .collect()
        })
        .collect()
}

/// What one case expects of the whole result: the columns of each row BESIDE the pointer, with
/// the values the pipeline computed for them — so a row expecting nothing but its pointer is
/// written as an empty list, and a column no case names must not be reported at all.
type ExpectedRows<'a> = Vec<Vec<(&'a str, &'a str)>>;

/// Assert that `request`'s query answers exactly `expected`: the same number of rows, and each
/// row carrying its pointer plus precisely the columns and values the case names.
async fn assert_columns(logs: &Logs, case: &str, request: &Value, expected: &ExpectedRows<'_>) {
    let answer = completed(logs, case, request).await;
    let produced = rows(case, &answer);
    assert_eq!(
        produced.len(),
        expected.len(),
        "case {case:?}: expected {} rows, got {produced:?}",
        expected.len()
    );
    for (index, wanted) in expected.iter().enumerate() {
        let row = &produced[index];
        // Every row of log records identifies the record it came from, whatever else it carries.
        assert!(
            row.get(POINTER).is_some_and(|pointer| !pointer.is_empty()),
            "case {case:?}: row {index} must carry a {POINTER}, got {row:?}"
        );
        let carried: BTreeMap<String, String> = row
            .iter()
            .filter(|(field, _)| *field != POINTER)
            .map(|(field, value)| (field.clone(), value.clone()))
            .collect();
        let named: BTreeMap<String, String> = wanted
            .iter()
            .map(|(field, value)| ((*field).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            carried, named,
            "case {case:?}: row {index} must carry exactly {named:?} beside its {POINTER}, got \
             {row:?}"
        );
    }
}

/// A command of a Logs Insights QL pipeline that defines fields establishes the result's
/// columns — in every form the command takes, and whether or not a record carries a value for
/// them.
#[tokio::test]
async fn a_defining_command_establishes_the_results_columns() {
    let logs = Logs::new();
    const GROUP: &str = "/insights/defined-columns/cwli";
    const STREAM: &str = "requests-one";
    /// The lookup table whose key column every record of the group matches.
    const JOINED: &str = "defined_columns_streams";
    /// A lookup table of the same shape that no record's key matches.
    const UNJOINED: &str = "defined_columns_other_streams";

    let messages: Vec<String> = vec![
        format!("REQ {MARKER} svc=alpha lat=100 status=200"),
        format!("REQ {MARKER} svc=beta lat=25 status=404"),
        // A line the same filter selects and no pattern below matches, which is what makes a row
        // carrying none of the defined columns a case of its own.
        format!("HEARTBEAT {MARKER} no fields here"),
    ];
    let events: Vec<(i64, String)> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (1_000 * (index as i64 + 1), message.clone()))
        .collect();
    let window = seeded(&logs, GROUP, STREAM, &events).await;
    logs.ok(
        "CreateLookupTable",
        &json!({
            "lookupTableName": JOINED,
            "tableBody": format!("stream,team\n{STREAM},platform\n"),
        }),
    )
    .await;
    logs.ok(
        "CreateLookupTable",
        &json!({
            "lookupTableName": UNJOINED,
            "tableBody": "stream,team\nno-such-stream,west\n",
        }),
    )
    .await;

    let cases: Vec<(&str, String, ExpectedRows<'_>)> = vec![
        (
            "a glob parse naming three fields",
            format!(
                "filter @message like /{MARKER}/ | parse @message \"svc=* lat=* status=*\" \
                 as svc, lat, st | sort @timestamp asc"
            ),
            vec![
                vec![("svc", "alpha"), ("lat", "100"), ("st", "200")],
                vec![("svc", "beta"), ("lat", "25"), ("st", "404")],
                // The line the pattern does not match: nothing but its pointer.
                vec![],
            ],
        ),
        (
            "a glob parse naming two fields, the second taking the rest of the line",
            format!(
                "filter @message like /{MARKER}/ | parse @message \"svc=* lat=*\" as svc, rest \
                 | sort @timestamp asc"
            ),
            vec![
                vec![("svc", "alpha"), ("rest", "100 status=200")],
                vec![("svc", "beta"), ("rest", "25 status=404")],
                vec![],
            ],
        ),
        (
            "a named-capture regex parse naming one field",
            format!(
                "filter @message like /{MARKER}/ | parse @message /lat=(?<lat>\\d+)/ \
                 | sort @timestamp asc"
            ),
            vec![vec![("lat", "100")], vec![("lat", "25")], vec![]],
        ),
        (
            "a lookup joining one column onto every record, keyed on a generated field",
            format!(
                "filter @message like /{MARKER}/ | lookup {JOINED} @logStream AS stream \
                 OUTPUT team | sort @timestamp asc"
            ),
            // The key the join read is not a column of the result: only the column the lookup
            // DEFINED is.
            vec![
                vec![("team", "platform")],
                vec![("team", "platform")],
                vec![("team", "platform")],
            ],
        ),
        (
            "a lookup whose key joins no row of the table",
            format!(
                "filter @message like /{MARKER}/ | lookup {UNJOINED} @logStream AS stream \
                 OUTPUT team | sort @timestamp asc"
            ),
            // The column is the pipeline's own, so the default ones are gone — and no record
            // joined a value for it, so every row is its pointer alone.
            vec![vec![], vec![], vec![]],
        ),
        (
            "a projection after the parse, which names a default column itself",
            format!(
                "filter @message like /{MARKER}/ | parse @message \"svc=* lat=* status=*\" \
                 as svc, lat, st | display @message, svc | sort @timestamp asc"
            ),
            // A default column is reported where the query REQUESTS it, and the parsed columns
            // the projection does not name are gone.
            vec![
                vec![("@message", messages[0].as_str()), ("svc", "alpha")],
                vec![("@message", messages[1].as_str()), ("svc", "beta")],
                vec![("@message", messages[2].as_str())],
            ],
        ),
    ];

    for (case, query_string, expected) in cases {
        let request = json!({
            "logGroupName": GROUP,
            "startTime": window.0,
            "endTime": window.1,
            "queryString": query_string,
        });
        assert_columns(&logs, case, &request, &expected).await;
    }
}

/// An `eval` defines a field the same way, and defines it on the records its expression
/// computed a value on: a record it computed nothing on carries nothing for that column.
#[tokio::test]
async fn an_eval_defines_the_results_columns_only_where_it_computed_a_value() {
    let logs = Logs::new();
    const GROUP: &str = "/insights/defined-columns/ppl";
    const STREAM: &str = "events-one";

    let events: Vec<(i64, String)> = vec![
        // Two structured events, whose members are fields of the record a query reads.
        (1_000, r#"{"svc":"alpha","lat":"100"}"#.to_string()),
        (2_000, r#"{"svc":"beta","lat":"25"}"#.to_string()),
        // One unstructured line, which carries no `svc` and no `lat` at all.
        (3_000, format!("HEARTBEAT {MARKER} no fields here")),
    ];
    let window = seeded(&logs, GROUP, STREAM, &events).await;

    let cases: Vec<(&str, String, ExpectedRows<'_>)> = vec![
        (
            "an eval of a field two of the three records carry",
            format!("source = `{GROUP}` | eval team = svc | sort @timestamp"),
            vec![vec![("team", "alpha")], vec![("team", "beta")], vec![]],
        ),
        (
            "an eval computing a number from a field",
            format!("source = `{GROUP}` | eval doubled = lat * 2 | sort @timestamp"),
            vec![vec![("doubled", "200")], vec![("doubled", "50")], vec![]],
        ),
        (
            "two evals, one of a field and one answering on every record",
            format!(
                "source = `{GROUP}` | eval team = svc | eval present = ispresent(svc) \
                 | sort @timestamp"
            ),
            vec![
                vec![("team", "alpha"), ("present", "true")],
                vec![("team", "beta"), ("present", "true")],
                // The expression that answers on EVERY record defines its column on every row;
                // the one that answered nothing for this record defines none.
                vec![("present", "false")],
            ],
        ),
        (
            "an eval over a generated field every record carries",
            format!("source = `{GROUP}` | eval stream_name = upper(@logStream) | sort @timestamp"),
            vec![
                vec![("stream_name", "EVENTS-ONE")],
                vec![("stream_name", "EVENTS-ONE")],
                vec![("stream_name", "EVENTS-ONE")],
            ],
        ),
    ];

    for (case, query_string, expected) in cases {
        // The PPL query names the log group it reads itself, so the request names none.
        let request = json!({
            "queryLanguage": "PPL",
            "startTime": window.0,
            "endTime": window.1,
            "queryString": query_string,
        });
        assert_columns(&logs, case, &request, &expected).await;
    }
}

/// RULE, the other way round: a pipeline that defines and projects NOTHING carries the default
/// columns, in both languages — so the default projection survives exactly where the pipeline
/// has no columns of its own.
#[tokio::test]
async fn a_pipeline_defining_nothing_carries_the_default_columns() {
    let logs = Logs::new();
    const GROUP: &str = "/insights/defined-columns/defaults";
    const STREAM: &str = "lines-one";

    let messages: Vec<String> = vec![
        format!("REQ {MARKER} svc=alpha lat=100 status=200"),
        format!("HEARTBEAT {MARKER} no fields here"),
    ];
    let events: Vec<(i64, String)> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (1_000 * (index as i64 + 1), message.clone()))
        .collect();
    let window = seeded(&logs, GROUP, STREAM, &events).await;

    // (case, the language the pipeline is written in, a pipeline defining no column of its own)
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "a filter alone",
            "CWLI",
            format!("filter @message like /{MARKER}/"),
        ),
        (
            "a filter and a sort",
            "CWLI",
            format!("filter @message like /{MARKER}/ | sort @timestamp asc"),
        ),
        (
            "a filter and a deduplication",
            "CWLI",
            format!("filter @message like /{MARKER}/ | dedup @logStream | limit 1"),
        ),
        (
            "a PPL pipeline selecting its own log group",
            "PPL",
            format!("source = `{GROUP}` | sort @timestamp"),
        ),
    ];

    for (case, language, query_string) in cases {
        let mut request = json!({
            "queryLanguage": language,
            "startTime": window.0,
            "endTime": window.1,
            "queryString": query_string,
        });
        if language == "CWLI" {
            request["logGroupName"] = json!(GROUP);
        }
        let answer = completed(&logs, case, &request).await;
        let produced = rows(case, &answer);
        assert!(
            !produced.is_empty(),
            "case {case:?}: expected a row, got {produced:?}"
        );
        for (index, row) in produced.iter().enumerate() {
            let mut columns: Vec<&str> = row.keys().map(String::as_str).collect();
            columns.sort_unstable();
            let mut wanted: Vec<&str> = DEFAULT_COLUMNS.to_vec();
            wanted.push(POINTER);
            wanted.sort_unstable();
            assert_eq!(
                columns, wanted,
                "case {case:?}: row {index} of a pipeline defining no column of its own carries \
                 the default columns and its pointer, got {row:?}"
            );
        }
    }
}
