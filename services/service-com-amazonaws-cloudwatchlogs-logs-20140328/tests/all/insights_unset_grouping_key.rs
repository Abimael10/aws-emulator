// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH records a CloudWatch Logs Insights aggregation groups.
//!
//! The rule under test: a `stats` command groups only the records its grouping keys are
//! DEFINED on — a record carrying no value for one of them belongs to no group, so it
//! contributes to no aggregate and forms no group of its own — while a record whose key is
//! present and EMPTY text is grouped under that empty text. A `parse` binds its fields only on
//! the records its pattern matches, so the records it did not match are exactly the records a
//! later grouping by one of those fields leaves out.
//!
//! The table spans what the rule generalises over rather than one query, so any one query
//! string an issue happens to mention is a single row:
//!
//! * where the grouping key COMES FROM — a named capture group of a `parse`, a `*` of a glob
//!   `parse`, a field discovered in a JSON message, and a name no record carries at all;
//! * how many of the scanned records carry it — all of them, some of them, none of them;
//! * whether the value is absent or present and empty;
//! * how many keys the `by` clause names, so a record missing ANY one of them is left out;
//! * which query language's vocabulary the pipeline is written in (Logs Insights QL and
//!   OpenSearch PPL);
//! * an aggregation naming NO grouping key, which leaves no record out.
//!
//! The last case closes the loop the way a client does: the same lines read through a
//! positional metric filter, whose extracted values must agree with the rows the aggregation
//! answers.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";
const TEST_FILTER: &str = "TestMetricFilter";

/// The log group and stream every case queries.
const GROUP: &str = "/insights/unset-grouping-key";
const STREAM: &str = "stream";

/// The events every case aggregates.
///
/// Three lines carry the transfer shape a `parse` pattern below matches, two carry a shape it
/// does not, and three are JSON objects whose discovered `svc` field is present, present and
/// EMPTY, and absent — so each case has records its grouping key is and is not defined on.
const MESSAGES: [&str; 8] = [
    "XFER alice 30",
    "XFER bob 5",
    "XFER alice 12",
    "SKIP carol 99",
    "RETRY alice 1000",
    r#"{"svc":"api","ms":7}"#,
    r#"{"svc":"","ms":9}"#,
    r#"{"ms":11}"#,
];

/// The positional filter pattern that reads the same transfer lines, naming the same two
/// values the `parse` patterns capture.
const TRANSFER_FILTER: &str = "[kind=XFER, user, nbytes]";

/// How long a case waits for a started query to finish before giving up — generous room for a
/// loaded host, since what is waited for is the status the query reports and never this bound.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that status is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The log group holding [`MESSAGES`], and the query window in the epoch SECONDS
/// `StartQuery`'s range is specified in.
async fn group_with_events(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    let base = now_ms() / 1_000 * 1_000 - 60 * 1_000;
    let events: Vec<(i64, &str)> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| (base + index as i64 * 1_000, *message))
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
    let now_secs = now_ms() / 1_000;
    (now_secs - 3_600, now_secs + 60)
}

/// Start `query_string` in `language`, wait until it reports a finished status, and answer its
/// rows as field/value maps.
///
/// The wait is on the CONDITION the query reaches, looked at again until it holds or
/// [`COMPLETION_DEADLINE`] passes, so nothing here depends on how fast the host runs.
async fn completed_rows(
    logs: &Logs,
    case: &str,
    window: (i64, i64),
    language: &str,
    query_string: &str,
) -> Vec<BTreeMap<String, String>> {
    let started = logs
        .ok(
            START,
            &json!({
                "logGroupName": GROUP,
                "startTime": window.0,
                "endTime": window.1,
                "queryLanguage": language,
                "queryString": query_string,
            }),
        )
        .await;
    let id = started["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a start answers a queryId, got {started}"))
        .to_string();
    let deadline = std::time::Instant::now() + COMPLETION_DEADLINE;
    loop {
        let answer = logs.ok(RESULTS, &json!({"queryId": id})).await;
        if answer["status"].as_str() == Some("Complete") {
            return rows(case, &answer);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the query never completed, last answer {answer}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rows of a `GetQueryResults` answer, each as the field/value pairs it carries.
fn rows(case: &str, answer: &Value) -> Vec<BTreeMap<String, String>> {
    answer["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: results are a list, got {answer}"))
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a row is a list, got {row}"))
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

/// One row of the table: what the case is, the language its pipeline is written in, the query
/// string, and the rows it must answer, in order, each as the `(field, value)` pairs it carries
/// — every pair of them, so a row carrying a key member the rule leaves out fails the case.
type Case = (
    &'static str,
    &'static str,
    String,
    Vec<Vec<(&'static str, &'static str)>>,
);

/// The Logs Insights QL language name, as `StartQuery$queryLanguage` writes it.
const CWLI: &str = "CWLI";

/// The OpenSearch PPL language name, the second pipeline vocabulary the same rule holds in.
const PPL: &str = "PPL";

/// RULE: an aggregation groups only the records its grouping keys are defined on.
#[tokio::test]
async fn a_record_with_no_value_for_a_grouping_key_joins_no_group() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    let cases: Vec<Case> = vec![
        (
            // The issue's own shape: a named capture binds `user` only on the lines the
            // pattern matches, so the five records it did not match form no group.
            "a named capture group, grouped by the captured field",
            CWLI,
            r"parse @message /^XFER (?<user>\S+) (?<nbytes>\d+)$/ | stats sum(nbytes) as total, count(*) as n by user | sort user asc"
                .to_string(),
            vec![
                vec![("user", "alice"), ("total", "42"), ("n", "2")],
                vec![("user", "bob"), ("total", "5"), ("n", "1")],
            ],
        ),
        (
            // The same grouping key, extracted by the glob form of the same command.
            "a glob parse naming the captured fields, grouped by the first",
            CWLI,
            r#"parse @message "XFER * *" as user, nbytes | stats sum(nbytes) as total, count(*) as n by user | sort user asc"#
                .to_string(),
            vec![
                vec![("user", "alice"), ("total", "42"), ("n", "2")],
                vec![("user", "bob"), ("total", "5"), ("n", "1")],
            ],
        ),
        (
            // A field discovered in a JSON message: present, present and EMPTY, and absent.
            // The record whose `svc` is empty text keeps a group of its own, keyed by that
            // empty text; the records carrying no `svc` at all form none.
            "a discovered field, present on one record, empty on another and absent on the rest",
            CWLI,
            "stats count(*) as n by svc | sort svc asc".to_string(),
            vec![
                vec![("svc", ""), ("n", "1")],
                vec![("svc", "api"), ("n", "1")],
            ],
        ),
        (
            // Two grouping keys no single record carries both of: a record missing EITHER one
            // joins no group, so the aggregation answers nothing.
            "two grouping keys, no record carrying both",
            CWLI,
            r"parse @message /^XFER (?<user>\S+) (?<nbytes>\d+)$/ | stats count(*) as n by user, svc"
                .to_string(),
            vec![],
        ),
        (
            "a grouping key no record carries at all",
            CWLI,
            "stats count(*) as n by absentField".to_string(),
            vec![],
        ),
        (
            // An aggregation naming NO grouping key leaves no record out: every scanned record
            // is folded into the one group, whatever fields it carries.
            "an aggregation naming no grouping key",
            CWLI,
            "stats count(*) as n".to_string(),
            vec![vec![("n", "8")]],
        ),
        (
            // The span grouping is the same rule over an instant: a record whose timestamp
            // field is absent has no instant to be grouped by, so it joins no span.
            "a span taken over a field no record carries",
            CWLI,
            "stats count(*) as n by bin(absentTime, 1m)".to_string(),
            vec![],
        ),
        (
            // The sibling clustering command follows the same rule: a record carrying no text
            // under the field has no shape to cluster by, so it joins no pattern — while the
            // share each pattern reports stays a share of the records ANALYSED, which is what
            // `@ratio` is documented to be.
            "a pattern command clustering a field only some records carry",
            CWLI,
            "pattern svc".to_string(),
            vec![
                vec![("@pattern", ""), ("@sampleCount", "1"), ("@ratio", "0.125")],
                vec![
                    ("@pattern", "api"),
                    ("@sampleCount", "1"),
                    ("@ratio", "0.125"),
                ],
            ],
        ),
        (
            // The other pipeline vocabulary, over the same records and the same rule.
            "a PPL pipeline grouped by a discovered field",
            PPL,
            format!(
                "source = logGroups(logGroupIdentifier:[\"{GROUP}\"]) \
                 | stats count() as n by svc | sort svc"
            ),
            vec![
                vec![("svc", ""), ("n", "1")],
                vec![("svc", "api"), ("n", "1")],
            ],
        ),
    ];

    for (case, language, query_string, expected) in &cases {
        let produced = completed_rows(&logs, case, window, language, query_string).await;
        assert_eq!(
            produced.len(),
            expected.len(),
            "case {case:?}: expected {} rows, got {produced:?}",
            expected.len()
        );
        for (index, wanted) in expected.iter().enumerate() {
            let row = &produced[index];
            let carried: BTreeMap<String, String> = wanted
                .iter()
                .map(|(field, value)| ((*field).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                row, &carried,
                "case {case:?}: row {index} must carry exactly {carried:?}, got {row:?}"
            );
        }
    }
}

/// RULE: the records a `parse` did not match contribute to no group, so an aggregation over a
/// captured field agrees with a positional metric filter reading the same lines — the records
/// one instrument extracts values from are the records the other groups.
#[tokio::test]
async fn an_aggregation_over_a_captured_field_agrees_with_a_metric_filter() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    // What the metric filter extracts, per user: the summed byte count and how many lines it
    // matched — computed here from the values the service reports, not stated.
    let reported = logs
        .ok(
            TEST_FILTER,
            &json!({"filterPattern": TRANSFER_FILTER, "logEventMessages": MESSAGES}),
        )
        .await;
    let matched = reported["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("TestMetricFilter reports matches, got {reported}"));
    let mut by_filter: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for entry in matched {
        let extracted = &entry["extractedValues"];
        let user = extracted["$user"]
            .as_str()
            .unwrap_or_else(|| panic!("a match extracts $user, got {entry}"))
            .to_string();
        let bytes: f64 = extracted["$nbytes"]
            .as_str()
            .unwrap_or_else(|| panic!("a match extracts $nbytes, got {entry}"))
            .parse()
            .unwrap_or_else(|_| panic!("the extracted $nbytes is a number, got {entry}"));
        let held = by_filter.entry(user).or_insert((0.0, 0));
        held.0 += bytes;
        held.1 += 1;
    }
    assert!(
        !by_filter.is_empty(),
        "the filter pattern {TRANSFER_FILTER:?} must match the transfer lines, got {reported}"
    );

    // The same lines through the aggregation, keyed the same way.
    let case = "a parse and stats over the lines a positional metric filter reads";
    let produced = completed_rows(
        &logs,
        case,
        window,
        CWLI,
        r"parse @message /^XFER (?<user>\S+) (?<nbytes>\d+)$/ | stats sum(nbytes) as total, count(*) as n by user | sort user asc",
    )
    .await;
    let by_insights: BTreeMap<String, (f64, usize)> = produced
        .iter()
        .map(|row| {
            let user = row
                .get("user")
                .unwrap_or_else(|| panic!("case {case:?}: every row carries user, got {row:?}"))
                .clone();
            let total: f64 = row
                .get("total")
                .and_then(|total| total.parse().ok())
                .unwrap_or_else(|| panic!("case {case:?}: every row carries total, got {row:?}"));
            let count: usize = row
                .get("n")
                .and_then(|count| count.parse().ok())
                .unwrap_or_else(|| panic!("case {case:?}: every row carries n, got {row:?}"));
            (user, (total, count))
        })
        .collect();
    assert_eq!(
        by_insights, by_filter,
        "case {case:?}: the aggregation must agree with the metric filter over the same lines, \
         got {by_insights:?} against {by_filter:?} in {produced:?}"
    );
}
