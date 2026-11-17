// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH columns a Logs Insights query's results carry when the pipeline
//! DEFINES fields of its own.
//!
//! The rule under test: an ephemeral field a stage defines — the captures of a `parse`, the
//! columns a `lookup` joins — is a field of the records that stage produces, so the rows the
//! query answers carry it; the default columns `@timestamp` and `@message` are what a pipeline
//! producing NONE OF ITS OWN carries, so the first stage to produce columns replaces them and a
//! later one adds to what the pipeline has already chosen. A stage that CHOOSES the columns
//! (`fields`, `display`, an aggregation) decides them for everything after it, so a projection
//! placed after such a stage narrows the result to what it names.
//!
//! The table spans what the rule generalises over: which form the `parse` takes (a glob naming
//! several fields, a named capture group), whether the pipeline projected columns before the
//! defining stage, whether a projection follows it, and whether the defining stage is a `parse`
//! or a `lookup`.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";

/// The log group and stream every case queries.
const GROUP: &str = "/insights/parsed-columns";
const STREAM: &str = "stream";

/// The lookup table one case joins, and the column it joins on.
const TABLE: &str = "parsed_columns_owners";

/// The events every case queries: the fields a `parse` extracts out of them are the subject.
const MESSAGES: [&str; 2] = [
    "svc=alpha lat=100 status=ok owner=u1",
    "svc=beta lat=200 status=ok owner=u2",
];

/// How long a case waits for a started query to finish before giving up — generous room for a
/// loaded host, since what is waited for is the status the query reports and never this bound.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that status is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The log group holding [`MESSAGES`], a lookup table to join, and the query window in the
/// epoch SECONDS `StartQuery`'s range is specified in.
async fn group_with_events(logs: &Logs) -> (i64, i64) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    logs.ok(
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
    )
    .await;
    logs.ok(
        "CreateLookupTable",
        &json!({
            "lookupTableName": TABLE,
            "tableBody": "owner,team\nu1,alpha-team\nu2,beta-team\n",
        }),
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

/// Start `query_string`, wait until it reports a finished status, and answer its rows as
/// field/value maps.
///
/// The wait is on the CONDITION the query reaches, looked at again until it holds or
/// [`COMPLETION_DEADLINE`] passes, so nothing here depends on how fast the host runs.
async fn completed_rows(
    logs: &Logs,
    case: &str,
    window: (i64, i64),
    query_string: &str,
) -> Vec<BTreeMap<String, String>> {
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

/// One row of the table: what the case is, the query string it starts, the `(field, value)`
/// pairs every row of its result must carry, in row order, and the columns those rows must NOT
/// carry at all.
type Case = (
    &'static str,
    String,
    Vec<Vec<(&'static str, &'static str)>>,
    Vec<&'static str>,
);

/// RULE: the fields a pipeline DEFINES are reported in its rows.
#[tokio::test]
async fn the_fields_a_pipeline_defines_are_reported_in_its_rows() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    let cases: Vec<Case> = vec![
        (
            "a glob parse naming three fields, with no projection at all",
            r#"parse @message "svc=* lat=* status=*" as svc, lat, st | sort @timestamp asc"#
                .to_string(),
            vec![
                vec![("svc", "alpha"), ("lat", "100"), ("st", "ok owner=u1")],
                vec![("svc", "beta"), ("lat", "200"), ("st", "ok owner=u2")],
            ],
            // The pipeline produced its own columns, so the default projection it never asked
            // for is not reported beside them.
            vec!["@timestamp", "@message"],
        ),
        (
            "a named capture group, with no projection at all",
            r"parse @message /lat=(?<latency>\d+)/ | sort @timestamp asc".to_string(),
            vec![vec![("latency", "100")], vec![("latency", "200")]],
            vec!["@timestamp", "@message"],
        ),
        (
            "a projection BEFORE the defining stage, which the parse adds to",
            r#"fields @timestamp, @message | parse @message "svc=* lat=*" as svc, lat | sort @timestamp asc"#
                .to_string(),
            vec![vec![("svc", "alpha"), ("lat", "100 status=ok owner=u1")]],
            // The pipeline chose these columns itself, so they are reported.
            Vec::new(),
        ),
        (
            "a lookup joining a column onto each record",
            format!(
                r#"parse @message "svc=* lat=* status=* owner=*" as svc, lat, st, owner | lookup {TABLE} owner OUTPUT team | sort @timestamp asc"#
            ),
            vec![vec![("team", "alpha-team")], vec![("team", "beta-team")]],
            vec!["@timestamp", "@message"],
        ),
    ];

    for (case, query_string, expected, absent) in &cases {
        let produced = completed_rows(&logs, case, window, query_string).await;
        for (index, wanted) in expected.iter().enumerate() {
            let row = produced
                .get(index)
                .unwrap_or_else(|| panic!("case {case:?}: expected a row {index}, got {produced:?}"));
            for (field, value) in wanted {
                assert_eq!(
                    row.get(*field).map(String::as_str),
                    Some(*value),
                    "case {case:?}: row {index} must carry {field}={value}, got {row:?}"
                );
            }
            // The columns the pipeline did not produce are not reported at all.
            for dropped in absent {
                assert!(
                    !row.contains_key(*dropped),
                    "case {case:?}: row {index} must carry nothing for {dropped}, got {row:?}"
                );
            }
            // Every row of log records identifies the record it came from.
            assert!(
                row.contains_key("@ptr"),
                "case {case:?}: row {index} still carries @ptr, got {row:?}"
            );
        }
    }
}

/// RULE: a projection placed AFTER the defining stage decides the columns, so the result
/// carries what that projection names and not every field the pipeline computed.
#[tokio::test]
async fn a_projection_after_the_defining_stage_narrows_the_result() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;
    let case = "a display of one of two parsed fields";
    let produced = completed_rows(
        &logs,
        case,
        window,
        r#"parse @message "svc=* lat=* status=*" as svc, lat, st | sort @timestamp asc | display svc"#,
    )
    .await;
    let row = produced
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a row, got {produced:?}"));
    assert_eq!(
        row.get("svc").map(String::as_str),
        Some("alpha"),
        "case {case:?}: the projected field is reported, got {row:?}"
    );
    for dropped in ["lat", "st", "@message"] {
        assert!(
            !row.contains_key(dropped),
            "case {case:?}: the projection leaves {dropped} out, got {row:?}"
        );
    }
}
