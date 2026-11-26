// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ACCOUNTING of a CloudWatch Logs Insights aggregation: every
//! matched log event is counted in exactly ONE group.
//!
//! The rule under test: a `stats <aggregates> by <keys>` command partitions every matched
//! event — the events carrying no value for the grouping keys are aggregated into one group
//! of their own, whose row carries the aggregate columns and no member for the keys it has no
//! value for — so the counts the rows report, the keyed ones and the keyless one together,
//! sum to the matched events `GetQueryResults` reports under
//! `statistics.recordsMatched` ("The number of log events that matched the query string").
//!
//! The sibling suite `insights_unset_grouping_key.rs` pins WHICH records form that group and
//! what its row carries; this one pins the ARITHMETIC that follows from it, which is what a
//! client cross-checking a rollup against the matched-event count reads.
//!
//! The table spans what the rule generalises over rather than one query, so any one query
//! string an issue happens to mention is a single row:
//!
//! * where the grouping key comes from — a field discovered in a JSON message, a `*` of a glob
//!   `parse`, a named capture of a regular-expression `parse`, a field CloudWatch Logs
//!   generates for every event, and a name no event carries;
//! * how many of the matched events carry it — all of them, most of them, one short, none;
//! * whether the matched events are every event scanned or the subset a `filter` selected, so
//!   the count the rows sum to is the MATCHED events rather than the scanned ones;
//! * how many keys the `by` clause names;
//! * which pipeline vocabulary the query is written in (Logs Insights QL and OpenSearch PPL);
//! * which aggregates ride along beside the count — a sum over a field the keyless events
//!   partly carry, and a distinct count over one they do not carry at all.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::{batch, now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const RESULTS: &str = "GetQueryResults";
const FILTER_EVENTS: &str = "FilterLogEvents";

/// The log group and stream every case queries.
const GROUP: &str = "/insights/keyless-group-accounting";
const STREAM: &str = "stream";

/// The marker the lines a `filter` case selects carry, and which the JSON messages do not.
const MARKER: &str = "tag";

/// The events every case aggregates.
///
/// Four JSON objects carry a `svc` field, one carries it as EMPTY text, one carries none of it;
/// five plain-text lines carry a `svc=<value>` pair a `parse` extracts, and one plain-text line
/// carries no such pair at all — so every case has matched events its grouping key is and is
/// not defined on, and the keyless events of one case are the keyed events of another.
const MESSAGES: [&str; 12] = [
    r#"{"svc":"api","ms":120,"user":"alice"}"#,
    r#"{"svc":"api","ms":80,"user":"bob"}"#,
    r#"{"svc":"db","ms":40,"user":"alice"}"#,
    r#"{"svc":"web","ms":10,"user":"carol"}"#,
    r#"{"svc":"","ms":5,"user":"dave"}"#,
    r#"{"ms":7,"user":"erin"}"#,
    "REQ tag svc=alpha lat=100",
    "REQ tag svc=alpha lat=150",
    "REQ tag svc=alpha lat=170",
    "REQ tag svc=beta lat=200",
    "REQ tag svc=beta lat=210",
    "HEARTBEAT tag no fields here",
];

/// How long a case waits for a started query to finish before giving up — generous room for a
/// loaded host, since what is waited for is the status the query reports and never this bound.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(60);

/// How often that status is looked at again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The Logs Insights QL language name, as `StartQuery$queryLanguage` writes it.
const CWLI: &str = "CWLI";

/// The OpenSearch PPL language name, the second pipeline vocabulary the same rule holds in.
const PPL: &str = "PPL";

/// The aggregate column every case counts its group with.
const COUNT_COLUMN: &str = "n";

/// The regular-expression `parse` whose NAMED capture groups one case groups by, written once
/// here because a raw string is the only way to state it without escaping it twice.
const NAMED_CAPTURE_PARSE: &str = r"/^REQ \S+ svc=(?<rsvc>\S+) lat=(?<rlat>\d+)$/";

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

/// Start `query_string` in `language` and wait until it reports a finished status, answering
/// the whole `GetQueryResults` body — the rows AND the statistics, which is what the rule
/// relates.
///
/// The wait is on the CONDITION the query reaches, looked at again until it holds or
/// [`COMPLETION_DEADLINE`] passes, so nothing here depends on how fast the host runs.
async fn completed(
    logs: &Logs,
    case: &str,
    window: (i64, i64),
    language: &str,
    query: &str,
) -> Value {
    let started = logs
        .ok(
            START,
            &json!({
                "logGroupName": GROUP,
                "startTime": window.0,
                "endTime": window.1,
                "queryLanguage": language,
                "queryString": query,
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
            return answer;
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

/// The count one row reports, read as the number it is written as.
fn counted(case: &str, row: &BTreeMap<String, String>) -> f64 {
    row.get(COUNT_COLUMN)
        .and_then(|count| count.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: every row carries a count, got {row:?}"))
}

/// One row of the table: what the case is, the language its pipeline is written in, the query
/// string, the fields its `by` clause groups by, the matched events the statistics must report,
/// how many rows carry no member for ANY of those fields, and the rows themselves in order —
/// each with EVERY pair it carries, so a row carrying a key member the rule leaves out, or an
/// empty value standing in for one, fails the case.
type Case = (
    &'static str,
    &'static str,
    String,
    &'static [&'static str],
    f64,
    usize,
    Vec<Vec<(&'static str, &'static str)>>,
);

/// RULE: every matched event is counted in exactly one group, so the counts of the keyed rows
/// and of the keyless row sum to `statistics.recordsMatched`.
#[tokio::test]
async fn the_group_counts_sum_to_the_matched_events() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    let cases: Vec<Case> = vec![
        (
            // A field discovered in a JSON message: four events carry it, one carries it as
            // empty text and seven carry none of it. The keyless row sums the `ms` its own
            // events carry, so it is the aggregate of those events rather than a bare count.
            "a discovered field, over every event of the window",
            CWLI,
            "stats count(*) as n, sum(ms) as s by svc | sort svc asc".to_string(),
            &["svc"],
            12.0,
            1,
            vec![
                vec![("svc", ""), ("n", "1"), ("s", "5")],
                vec![("svc", "api"), ("n", "2"), ("s", "200")],
                vec![("svc", "db"), ("n", "1"), ("s", "40")],
                vec![("svc", "web"), ("n", "1"), ("s", "10")],
                vec![("n", "7"), ("s", "7")],
            ],
        ),
        (
            // A key a glob `parse` extracts, over the events a `filter` selected: the line the
            // pattern did not match is matched by the query and counted in the keyless row, so
            // the rows sum to the MATCHED events rather than to the scanned ones.
            "a glob parse over the events a filter selected",
            CWLI,
            format!(
                "filter @message like /{MARKER}/ \
                 | parse @message \"svc=* lat=*\" as psvc, plat \
                 | stats count(*) as n, sum(plat) as s by psvc | sort n desc"
            ),
            &["psvc"],
            6.0,
            1,
            vec![
                vec![("psvc", "alpha"), ("n", "3"), ("s", "420")],
                vec![("psvc", "beta"), ("n", "2"), ("s", "410")],
                vec![("n", "1"), ("s", "0")],
            ],
        ),
        (
            // The same key extracted by a named capture group, with a distinct count over a
            // field the keyless event does not carry either.
            "a named capture over the events a filter selected",
            CWLI,
            format!(
                "filter @message like /{MARKER}/ | parse @message {NAMED_CAPTURE_PARSE} \
                 | stats count(*) as n, count_distinct(rlat) as u by rsvc | sort rsvc asc"
            ),
            &["rsvc"],
            6.0,
            1,
            vec![
                vec![("rsvc", "alpha"), ("n", "3"), ("u", "3")],
                vec![("rsvc", "beta"), ("n", "2"), ("u", "2")],
                vec![("n", "1"), ("u", "0")],
            ],
        ),
        (
            // A field CloudWatch Logs generates for EVERY event: no event lacks it, so there is
            // no keyless row and the one group counts every matched event.
            "a generated field every event carries",
            CWLI,
            "stats count(*) as n by @logStream".to_string(),
            &["@logStream"],
            12.0,
            0,
            vec![vec![("@logStream", STREAM), ("n", "12")]],
        ),
        (
            // The other end of the range: no event carries the key, so every matched event
            // falls in the keyless group and its row is the only one.
            "a key no event carries at all",
            CWLI,
            "stats count(*) as n by absentField".to_string(),
            &["absentField"],
            12.0,
            1,
            vec![vec![("n", "12")]],
        ),
        (
            // Two keys: a group reports the keys it HAS a value for, and the events carrying
            // neither form the one group reporting neither — the counts still sum to the
            // matched events.
            "two keys, with events carrying one, the other and neither",
            CWLI,
            "stats count(*) as n by svc, user | sort n desc, user asc, svc asc".to_string(),
            &["svc", "user"],
            12.0,
            1,
            vec![
                vec![("n", "6")],
                vec![("svc", "api"), ("user", "alice"), ("n", "1")],
                vec![("svc", "db"), ("user", "alice"), ("n", "1")],
                vec![("svc", "api"), ("user", "bob"), ("n", "1")],
                vec![("svc", "web"), ("user", "carol"), ("n", "1")],
                vec![("svc", ""), ("user", "dave"), ("n", "1")],
                vec![("user", "erin"), ("n", "1")],
            ],
        ),
        (
            // The other pipeline vocabulary, over the same events and the same arithmetic.
            "a PPL pipeline grouped by a discovered field",
            PPL,
            format!(
                "source = logGroups(logGroupIdentifier:[\"{GROUP}\"]) \
                 | stats count() as n by svc | sort svc"
            ),
            &["svc"],
            12.0,
            1,
            vec![
                vec![("svc", ""), ("n", "1")],
                vec![("svc", "api"), ("n", "2")],
                vec![("svc", "db"), ("n", "1")],
                vec![("svc", "web"), ("n", "1")],
                vec![("n", "7")],
            ],
        ),
    ];

    for (case, language, query, keys, matched, keyless, expected) in &cases {
        let answer = completed(&logs, case, window, language, query).await;
        let produced = rows(case, &answer);
        assert_eq!(
            produced.len(),
            expected.len(),
            "case {case:?}: expected {} rows, got {produced:?}",
            expected.len()
        );
        for (index, wanted) in expected.iter().enumerate() {
            let carried: BTreeMap<String, String> = wanted
                .iter()
                .map(|(field, value)| ((*field).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                &produced[index], &carried,
                "case {case:?}: row {index} must carry exactly {carried:?}, got {:?}",
                produced[index]
            );
        }

        // The rows carrying no member for ANY of the grouping keys: the ONE group the events
        // without a value for them form, and no second one.
        let unkeyed = produced
            .iter()
            .filter(|row| keys.iter().all(|key| !row.contains_key(*key)))
            .count();
        assert_eq!(
            unkeyed, *keyless,
            "case {case:?}: expected {keyless} row(s) carrying no member for {keys:?}, \
             got {unkeyed} in {produced:?}"
        );

        // The matched events the statistics report, and the sum the rule relates them to.
        let reported = answer["statistics"]["recordsMatched"].as_f64();
        assert_eq!(
            reported,
            Some(*matched),
            "case {case:?}: expected {matched} matched events, got {answer}"
        );
        let total: f64 = produced.iter().map(|row| counted(case, row)).sum();
        assert_eq!(
            Some(total),
            reported,
            "case {case:?}: the group counts must sum to the matched events, \
             got {total} against {reported:?} in {produced:?}"
        );
    }
}

/// RULE: the keyless group holds exactly the matched events the keyed groups do not, so the
/// counts an aggregation reports agree with the events the service itself holds.
#[tokio::test]
async fn the_keyless_group_holds_the_events_no_keyed_group_does() {
    let logs = Logs::new();
    let window = group_with_events(&logs).await;

    // What the store holds, read back through another operation rather than stated here: the
    // events of the group, and which of their messages carry a `svc` member at all.
    let page = logs
        .ok(FILTER_EVENTS, &json!({"logGroupName": GROUP}))
        .await;
    let messages: Vec<String> = page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("FilterLogEvents reports events, got {page}"))
        .iter()
        .map(|event| {
            event["message"]
                .as_str()
                .unwrap_or_else(|| panic!("every event carries a message, got {event}"))
                .to_string()
        })
        .collect();
    let with_key = messages
        .iter()
        .filter(|message| {
            serde_json::from_str::<Value>(message)
                .ok()
                .and_then(|parsed| parsed.get("svc").cloned())
                .is_some()
        })
        .count();
    let without_key = messages.len() - with_key;
    assert!(
        with_key > 0 && without_key > 0,
        "the seeded events must include some carrying svc and some not, got {messages:?}"
    );

    let case = "the keyless group against the events the service holds";
    let answer = completed(
        &logs,
        case,
        window,
        CWLI,
        "stats count(*) as n by svc | sort svc asc",
    )
    .await;
    let produced = rows(case, &answer);
    let keyed: Vec<&BTreeMap<String, String>> = produced
        .iter()
        .filter(|row| row.contains_key("svc"))
        .collect();
    let unkeyed: Vec<&BTreeMap<String, String>> = produced
        .iter()
        .filter(|row| !row.contains_key("svc"))
        .collect();
    assert_eq!(
        unkeyed.len(),
        1,
        "case {case:?}: the events carrying no svc form exactly one group, got {produced:?}"
    );
    assert_eq!(
        counted(case, unkeyed[0]),
        without_key as f64,
        "case {case:?}: the keyless group counts the {without_key} events carrying no svc, \
         got {:?}",
        unkeyed[0]
    );
    let keyed_total: f64 = keyed.iter().map(|row| counted(case, row)).sum();
    assert_eq!(
        keyed_total, with_key as f64,
        "case {case:?}: the keyed groups count the {with_key} events carrying svc, got {keyed:?}"
    );
    assert_eq!(
        answer["statistics"]["recordsMatched"].as_f64(),
        Some(messages.len() as f64),
        "case {case:?}: every event the service holds is a matched event, got {answer}"
    );
}
