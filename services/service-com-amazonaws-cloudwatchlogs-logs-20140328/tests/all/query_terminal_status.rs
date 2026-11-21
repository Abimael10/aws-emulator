// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the status a started CloudWatch Logs Insights query reaches, as a
//! client that POLLS `GetQueryResults` observes it.
//!
//! The rule under test: a started query always leaves the non-terminal statuses behind on the
//! clock — polling `GetQueryResults` reports one of the terminal `QueryStatus` values within a
//! bounded span, `Complete` when the engine ran the submitted query string and `Failed` when it
//! could not, in every query language and wherever the log groups it reads are named — so a
//! client waiting for a terminal status always gets one, and the rows and aggregates of that
//! answer are computed from the events the queried groups hold.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! a handler does not actually compute cannot pass.
//!
//! The table spans what the rule generalises over rather than one query: which language the
//! query is written in, whether the log groups are named by a request member or by the query
//! string's own `logGroups(…)` selector, whether a field reference is back-quoted (so the
//! quoting resolves a name the grammar would otherwise read as an expression), and whether the
//! engine can run the string at all. Any one query string an issue happens to mention is a
//! single row.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    QUERY_STATUS_CANCELLED, QUERY_STATUS_COMPLETE, QUERY_STATUS_FAILED, QUERY_STATUS_RUNNING,
    QUERY_STATUS_SCHEDULED,
};

use crate::harness::{base_ms, batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";

/// The log group every case queries, and the two streams its events are written to — two, so a
/// condition on the generated `@logStream` field selects part of the window.
const GROUP: &str = "/insights/terminal-status";
const FIRST_STREAM: &str = "alpha";
const SECOND_STREAM: &str = "beta";

/// The statuses a POLL may still see a query in, as the model's `QueryStatus` enum spells them
/// (`smithy_query(query="shape",shape="QueryStatus")` lists Scheduled, Running, Complete,
/// Failed, Cancelled, Timeout and Unknown): these two are the ones a query is still owed
/// something in, so a poll looks again.
const NON_TERMINAL: [&str; 2] = [QUERY_STATUS_SCHEDULED, QUERY_STATUS_RUNNING];

/// The statuses that END a query, from that same enum: the ones a client polling for a verdict
/// is waiting for.
const TERMINAL: [&str; 4] = [
    QUERY_STATUS_COMPLETE,
    QUERY_STATUS_FAILED,
    QUERY_STATUS_CANCELLED,
    // The enum's seventh value, `Unknown`, is no verdict and this service reports no query in
    // it; `Timeout` is the terminal one a run that outlived its span reaches.
    "Timeout",
];

/// How long a case waits, in all, for a started query to reach a terminal status: a deadline
/// generous enough for a loaded host, reached by POLLING the query — looking again until the
/// CONDITION holds — rather than by sleeping for a span the test guessed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// How long such a poll waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The events the cases read, per stream, as `(offset from the base instant, message)` pairs.
///
/// Every message is a JSON object, so the keys CloudWatch Logs discovers in it are queryable —
/// and two of those keys carry a HYPHEN, which the grammar reads as subtraction unless the
/// reference is back-quoted.
const FIRST_EVENTS: [(i64, &str); 4] = [
    (1_000, r#"{"svc":"api","ms":10,"status-code":500,"region-name":"eu"}"#),
    (2_000, r#"{"svc":"api","ms":20,"status-code":500,"region-name":"eu"}"#),
    (3_000, r#"{"svc":"db","ms":100,"status-code":500,"region-name":"us"}"#),
    (4_000, r#"{"svc":"api","ms":30,"status-code":200,"region-name":"eu"}"#),
];

/// See [`FIRST_EVENTS`]: the events of the second stream.
const SECOND_EVENTS: [(i64, &str); 2] = [
    (5_000, r#"{"svc":"api","ms":40,"status-code":500,"region-name":"us"}"#),
    (6_000, r#"{"svc":"db","ms":50,"status-code":500,"region-name":"us"}"#),
];

/// One case: what it is, the request members that name the query (its language, its string and
/// wherever it names the log groups), the terminal status it must reach, and the groups its
/// aggregation must report as `(svc, n, total)`.
type Case<'a> = (&'a str, Value, &'a str, Vec<(&'a str, &'a str, &'a str)>);

/// The log group holding both streams' events, and the window a query of it is started with in
/// the epoch SECONDS `StartQuery` documents for its range.
async fn group_with_two_streams(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    // Aligned on a whole second, because a query's window is stated in whole SECONDS: an event
    // stamped on a second boundary then falls inside or outside the window by the rule under
    // test rather than by where the test's own clock landed inside a second.
    let base = base_ms() / 1_000 * 1_000;
    for (stream, events) in [
        (FIRST_STREAM, FIRST_EVENTS.as_slice()),
        (SECOND_STREAM, SECOND_EVENTS.as_slice()),
    ] {
        logs.ok(
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": stream}),
        )
        .await;
        let stamped: Vec<(i64, &str)> = events
            .iter()
            .map(|(offset, message)| (base + offset, *message))
            .collect();
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": stream,
                "logEvents": batch(&stamped),
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

/// Start `request` and POLL its results until the query reports a status that ENDS it,
/// answering the id the start minted and that terminal answer.
///
/// Every status seen on the way is asserted to be one the model declares and one a query is
/// still owed something in, so a query parked in a status no client can act on fails the case
/// instead of being waited out.
async fn polled_to_a_terminal_status(logs: &Logs, case: &str, request: &Value) -> (String, Value) {
    let answer = logs.ok(START, request).await;
    let id = answer["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartQuery answers a queryId, got {answer}"))
        .to_string();
    let deadline = std::time::Instant::now() + TERMINAL_DEADLINE;
    loop {
        let answer = logs.ok(RESULTS, &json!({"queryId": id})).await;
        let status = answer["status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&status.as_str()) {
            return (id, answer);
        }
        assert!(
            NON_TERMINAL.contains(&status.as_str()),
            "case {case:?}: expected a status the model declares, got {status:?} with {answer}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the query to reach one of {TERMINAL:?}, got {status:?} with \
             {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: every started query reaches a terminal status on the clock — `Complete` when the
/// engine ran its string, `Failed` when it could not — and its answer is computed from the
/// events the log groups it selected hold.
#[tokio::test]
async fn a_started_query_always_reaches_a_terminal_status_a_poll_can_act_on() {
    let logs = Logs::new();
    let (start_time, end_time) = group_with_two_streams(&logs).await;
    let by_identifier = format!("logGroups(logGroupIdentifier: ['{GROUP}'])");

    // Hand-computed from [`FIRST_EVENTS`] and [`SECOND_EVENTS`]: every event of the window,
    // grouped by service — api holds 10, 20, 30 and 40, db holds 100 and 50.
    let whole_window: Vec<(&str, &str, &str)> = vec![("api", "4", "100"), ("db", "2", "150")];

    let cases: Vec<Case<'_>> = vec![
        (
            // The default language, naming its log group as a request member: the baseline the
            // other rows vary from.
            "a Logs Insights QL aggregation over a log group named as a request member",
            json!({
                "logGroupName": GROUP,
                "queryString": "stats count(*) as n, sum(ms) as total by svc",
            }),
            QUERY_STATUS_COMPLETE,
            whole_window.clone(),
        ),
        (
            // The hyphenated key is back-quoted, so the reference resolves the field of that
            // name: the three events of either stream carrying 500, grouped by service.
            "a PPL pipeline whose selector names the group and whose hyphenated field is \
             back-quoted",
            json!({
                "queryLanguage": "PPL",
                "queryString": format!(
                    "source = {by_identifier} | where `status-code` = 500 \
                     | stats count() as n, sum(ms) as total by svc"
                ),
            }),
            QUERY_STATUS_COMPLETE,
            vec![("api", "3", "70"), ("db", "2", "150")],
        ),
        (
            // A back-quoted GENERATED field, which selects one of the two streams.
            "a PPL pipeline filtering on the back-quoted generated stream field",
            json!({
                "queryLanguage": "PPL",
                "queryString": format!(
                    "source = `{by_identifier}` \
                     | where `@logStream` = '{SECOND_STREAM}' and `status-code` = 500 \
                     | stats count() as n, sum(ms) as total by svc"
                ),
            }),
            QUERY_STATUS_COMPLETE,
            vec![("api", "1", "40"), ("db", "1", "50")],
        ),
        (
            // Without the back quotes the hyphenated name is no field reference at all — the
            // grammar reads it as the expression `status - code`, which no event carries a
            // value for — so the condition selects nothing and the aggregation reports no
            // group. The query still COMPLETES: nothing about it failed to run.
            "the same pipeline with the hyphenated field written without back quotes",
            json!({
                "queryLanguage": "PPL",
                "queryString": format!(
                    "source = {by_identifier} | where status-code = 500 \
                     | stats count() as n, sum(ms) as total by svc"
                ),
            }),
            QUERY_STATUS_COMPLETE,
            Vec::new(),
        ),
        (
            // The selector may name the groups by PREFIX instead of by identifier, and the
            // second hyphenated key selects the events of one region.
            "a PPL pipeline selecting its log groups by name prefix",
            json!({
                "queryLanguage": "PPL",
                "queryString":
                    "source = logGroups(namePrefix: ['/insights/terminal-']) \
                     | where `region-name` = 'us' | stats count() as n, sum(ms) as total by svc",
            }),
            QUERY_STATUS_COMPLETE,
            vec![("api", "1", "40"), ("db", "2", "150")],
        ),
        (
            // "you specify the log group names inside the querystring instead of here"
            // (`StartQuery$logGroupName`): the statement's own FROM clause names them.
            "a SQL statement whose FROM clause names the log group it reads",
            json!({
                "queryLanguage": "SQL",
                "queryString": format!(
                    "SELECT `svc`, count(*) AS n, sum(`ms`) AS total FROM `{GROUP}` \
                     GROUP BY `svc`"
                ),
            }),
            QUERY_STATUS_COMPLETE,
            whole_window.clone(),
        ),
        (
            // A string the engine cannot run ends the query too: the terminal status of an
            // execution that produced nothing, never a query left non-terminal.
            "a PPL pipeline truncated at its where command",
            json!({
                "queryLanguage": "PPL",
                "queryString": format!("source = {by_identifier} | where"),
            }),
            QUERY_STATUS_FAILED,
            Vec::new(),
        ),
        (
            "a SQL statement whose aggregate is a window function",
            json!({
                "queryLanguage": "SQL",
                "queryString": format!(
                    "SELECT `svc`, count(*) OVER (PARTITION BY `svc`) AS n FROM `{GROUP}`"
                ),
            }),
            QUERY_STATUS_FAILED,
            Vec::new(),
        ),
    ];

    for (case, request, expected_status, wanted) in cases {
        let mut body = request;
        for (member, instant) in [("startTime", start_time), ("endTime", end_time)] {
            body[member] = json!(instant);
        }
        let answer = polled_to_a_terminal_status(&logs, case, &body).await.1;
        assert_eq!(
            answer["status"].as_str(),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {answer}"
        );
        let produced = rows(&answer);
        assert_eq!(
            produced.len(),
            wanted.len(),
            "case {case:?}: expected {} groups, got {produced:?}",
            wanted.len()
        );
        for (service, count, total) in wanted {
            let row = produced
                .iter()
                .find(|row| value(row, "svc") == service)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a group for {service}, got {produced:?}")
                });
            assert_eq!(
                (value(row, "n"), value(row, "total")),
                (count.to_string(), total.to_string()),
                "case {case:?}: the {service} group must carry n={count} and total={total}, got \
                 {row:?}"
            );
        }
    }
}

/// RULE, read from the other side: the terminal answer a client polled to is FIXED — reading
/// the same query's results again reports the same status and the same rows, and an event
/// ingested after the query ended changes neither.
#[tokio::test]
async fn the_terminal_answer_a_poll_arrived_at_is_what_every_later_read_reports() {
    let logs = Logs::new();
    let (start_time, end_time) = group_with_two_streams(&logs).await;
    const CASE: &str = "a PPL aggregation grouped by a back-quoted hyphenated field";
    let (id, answer) = polled_to_a_terminal_status(
        &logs,
        CASE,
        &json!({
            "queryLanguage": "PPL",
            "startTime": start_time,
            "endTime": end_time,
            "queryString": format!(
                "source = logGroups(logGroupIdentifier: ['{GROUP}']) \
                 | where `status-code` = 500 | stats count() as n by `region-name`"
            ),
        }),
    )
    .await;
    assert_eq!(
        answer["status"].as_str(),
        Some(QUERY_STATUS_COMPLETE),
        "case {CASE:?}: expected {QUERY_STATUS_COMPLETE}, got {answer}"
    );
    // Hand-computed from the two tables: of the five events carrying 500, two are stamped `eu`
    // and three `us`.
    let produced = rows(&answer);
    let counts: BTreeMap<String, String> = produced
        .iter()
        .map(|row| (value(row, "region-name"), value(row, "n")))
        .collect();
    let expected: BTreeMap<String, String> = [("eu", "2"), ("us", "3")]
        .into_iter()
        .map(|(region, count)| (region.to_string(), count.to_string()))
        .collect();
    assert_eq!(
        counts, expected,
        "case {CASE:?}: each region's bucket counts its own events, got {produced:?}"
    );

    // An event ingested AFTER the query reached its terminal status changes neither the rows
    // nor the status it answers: the results are the ones that run produced.
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": FIRST_STREAM,
            "logEvents": batch(&[(
                now_ms(),
                r#"{"svc":"api","ms":1,"status-code":500,"region-name":"eu"}"#,
            )]),
        }),
    )
    .await;
    let again = logs.ok(RESULTS, &json!({"queryId": id})).await;
    assert_eq!(
        again, answer,
        "case {CASE:?}: reading the results again reports the answer the query produced"
    );
}
