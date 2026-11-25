// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RANGE the effective query string of a started Insights query
//! states, and for the text a query that names its own sources is reported with.
//!
//! Two rules, each pinned over a table of windows rather than one recorded request:
//!
//! * CloudWatch Logs Insights names the log groups, the range and the row limit INSIDE a Logs
//!   Insights QL query, so a start that names them as parameters is reported as the `SOURCE`
//!   command they stand for — and that command's arguments are stated in epoch MILLISECONDS,
//!   the unit a pipelined query bounds its sources in, while `StartQuery`'s range is "epoch
//!   time, the number of seconds since January 1, 1970". The range therefore opens at the
//!   FIRST millisecond of the second the start named and, because "the range is inclusive, so
//!   the specified end time is included in the query", closes at the LAST millisecond of the
//!   second it ended at;
//! * a query string that names its own sources is the query, so it is reported BYTE FOR BYTE as
//!   submitted — which is what lets a client read a query back and re-submit it and get the
//!   same text again, millisecond bounds included.
//!
//! The rows span what the rules generalise over: the magnitude of each end of the window (the
//! epoch itself, a recent instant, a far-future one), the width of the window (zero seconds to
//! decades), how many log groups the start names, whether it names a row limit, and whether the
//! submitted text carries the `SOURCE` command itself.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! a handler does not compute from the shared store cannot pass.

use serde_json::{json, Value};

use crate::harness::{now_ms, Logs};

/// The operations every case calls.
const START: &str = "StartQuery";
const DESCRIBE: &str = "DescribeQueries";

/// The log groups the cases query.
const GROUP: &str = "/source-range/orders";
const OTHER_GROUP: &str = "/source-range/payments";

/// The pipeline every row submits once the sources are decided.
const PIPELINE: &str = "fields @message | filter @message like /level/";

/// The milliseconds one second holds, which is the magnitude the `SOURCE` command's arguments
/// are stated in.
const MILLIS_PER_SECOND: i64 = 1_000;

/// Both log groups.
async fn account_with_groups(logs: &Logs) {
    for group in [GROUP, OTHER_GROUP] {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
    }
}

/// Start `request` and answer the `queryId` it minted.
async fn started(logs: &Logs, case: &str, request: &Value) -> String {
    let answer = logs.ok(START, request).await;
    answer["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {START} answers a queryId, got {answer}"))
        .to_string()
}

/// The `queryString` `DescribeQueries` reports for the query under `id`.
async fn reported_query_string(logs: &Logs, case: &str, id: &str) -> String {
    let page = logs.ok(DESCRIBE, &json!({})).await;
    let query = page["queries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {DESCRIBE} reports a queries list, got {page}"))
        .iter()
        .find(|query| query["queryId"].as_str() == Some(id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the started query must be reported, got {page}"));
    query["queryString"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a reported query carries a queryString, got {query}"))
        .to_string()
}

/// The `SOURCE` command a start over `groups` and `window` stands for, as the rule states it:
/// the groups it reads, then the range in epoch milliseconds.
fn source_command(groups: &[&str], window: (i64, i64)) -> String {
    let sources: Vec<String> = groups.iter().map(|name| format!("\"{name}\"")).collect();
    format!(
        "SOURCE {} START={} END={}",
        sources.join(", "),
        window.0 * MILLIS_PER_SECOND,
        window.1 * MILLIS_PER_SECOND + MILLIS_PER_SECOND - 1
    )
}

/// A window the whole table can be built on: one ending now, so every row's end is at or after
/// the second the log groups were created in and the query is admitted.
fn now_secs() -> i64 {
    now_ms() / MILLIS_PER_SECOND + 1
}

/// One row of the window table: what it varies, the window the start names, the log groups it
/// names them by, and the row limit it names, if any.
struct WindowCase {
    case: &'static str,
    window: (i64, i64),
    groups: Vec<&'static str>,
    limit: Option<i64>,
}

/// The range the reported `SOURCE` command states is the window the start named, in epoch
/// milliseconds: the first millisecond of its start second and the last of its end second.
#[tokio::test]
async fn the_reported_source_command_states_the_window_in_milliseconds() {
    let logs = Logs::new();
    account_with_groups(&logs).await;
    let now = now_secs();

    let rows: Vec<WindowCase> = vec![
        WindowCase {
            case: "from the epoch itself to an instant far in the future",
            window: (0, 4_102_444_800),
            groups: vec![GROUP],
            limit: None,
        },
        WindowCase {
            case: "an hour ending now",
            window: (now - 60 * 60, now),
            groups: vec![GROUP],
            limit: None,
        },
        WindowCase {
            // Both ends name the same second, which the inclusive range still covers: a
            // millisecond-wide range would be empty, a second-wide one is not.
            case: "a window of one second",
            window: (now, now),
            groups: vec![GROUP],
            limit: None,
        },
        WindowCase {
            case: "a window opening one second after the epoch",
            window: (1, now),
            groups: vec![GROUP],
            limit: None,
        },
        WindowCase {
            case: "two log groups and a row limit",
            window: (now - 24 * 60 * 60, now + 1),
            groups: vec![GROUP, OTHER_GROUP],
            limit: Some(20),
        },
        WindowCase {
            case: "an end decades ahead",
            window: (now - 1, 253_402_300_799),
            groups: vec![GROUP, OTHER_GROUP],
            limit: None,
        },
    ];

    for WindowCase {
        case,
        window,
        groups,
        limit,
    } in rows
    {
        let mut request = json!({
            "logGroupIdentifiers": groups,
            "startTime": window.0,
            "endTime": window.1,
            "queryString": PIPELINE,
        });
        if let Some(limit) = limit {
            request["limit"] = json!(limit);
        }
        let id = started(&logs, case, &request).await;
        let reported = reported_query_string(&logs, case, &id).await;

        let expected = match limit {
            Some(limit) => format!(
                "{} | {PIPELINE} | limit {limit}",
                source_command(&groups, window)
            ),
            None => format!("{} | {PIPELINE}", source_command(&groups, window)),
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the window in epoch milliseconds, got {reported:?}"
        );
        // The end is the LAST millisecond of the second the start named, so it states that
        // second and not the one after it.
        let stated_end = reported
            .split_once("END=")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|end| end.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: the command states an end, got {reported:?}"));
        assert_eq!(
            stated_end / MILLIS_PER_SECOND,
            window.1,
            "case {case:?}: the stated end lies in the second the start named, got {stated_end}"
        );
    }
}

/// A query string that names its own sources is reported BYTE FOR BYTE as submitted, and
/// re-submitting the reported text reports that same text again — so a client can read a query
/// back and run it as it stands.
#[tokio::test]
async fn a_query_that_names_its_own_sources_is_reported_as_submitted() {
    let logs = Logs::new();
    account_with_groups(&logs).await;
    let now = now_secs();
    let window = (now - 60 * 60, now);

    // (case, the submitted query string)
    let rows: Vec<(&str, String)> = vec![
        (
            // The text the rule above produces, fed back in: its bounds are the milliseconds
            // the service reported, and nothing of them is re-stated.
            "the effective query of an earlier start, re-submitted",
            format!("{} | {PIPELINE}", source_command(&[GROUP], (0, 4_102_444_800))),
        ),
        (
            "a SOURCE command with millisecond bounds and a row limit",
            format!("SOURCE \"{GROUP}\" START=0 END=4102444800999 | {PIPELINE} | limit 12"),
        ),
        (
            "a SOURCE command naming two log groups",
            format!(
                "SOURCE \"{GROUP}\", \"{OTHER_GROUP}\" START=1 END=1700000000999 | {PIPELINE}"
            ),
        ),
        (
            "a SOURCE command with a selection instead of names",
            format!(
                "SOURCE logGroups(namePrefix: [\"/source-range/\"]) | {PIPELINE} | limit 5"
            ),
        ),
    ];

    for (case, submitted) in rows {
        let request = json!({
            "startTime": window.0,
            "endTime": window.1,
            "queryString": submitted,
        });
        let id = started(&logs, case, &request).await;
        let reported = reported_query_string(&logs, case, &id).await;
        assert_eq!(
            reported, submitted,
            "case {case:?}: expected the submitted text reported verbatim, got {reported:?}"
        );

        // Round trip: the reported text, submitted again, is reported again unchanged.
        let again = started(
            &logs,
            case,
            &json!({
                "startTime": window.0,
                "endTime": window.1,
                "queryString": reported,
            }),
        )
        .await;
        let reported_again = reported_query_string(&logs, case, &again).await;
        assert_eq!(
            reported_again, submitted,
            "case {case:?}: expected the echoed query to survive a re-submission, got \
             {reported_again:?}"
        );
    }
}
