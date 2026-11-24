// SPDX-License-Identifier: CC0-1.0

//! The MAGNITUDE the two ends of a `GetScheduledQueryHistory` window are stated in, and the
//! one a reported record states its own instant in.
//!
//! The rule under test: the window a read states selects the runs whose instant it covers in
//! whichever of the two epoch magnitudes its ends are written in, and a reported record states
//! its run's instant in epoch SECONDS.
//!
//! `startTime`, `endTime` and `triggeredTimestamp` are all the model's `Timestamp` rendered as a
//! plain `long` with no `timestampFormat`, documented only as being "in Unix epoch format"
//! (`smithy_query(query="shape",shape="GetScheduledQueryHistoryRequest,TriggerHistoryRecord")`),
//! so no sentence of theirs states a unit: neither the seconds this service states a query's own
//! window in (`StartQuery$startTime`, "the number of seconds since January 1, 1970") nor the
//! milliseconds its log events and resource instants are stated in is excluded. An instant of
//! this era in milliseconds and the same instant in seconds are a thousand apart, so the two
//! magnitudes name disjoint sets of values and each end is read in the one it states; a bound
//! stated in seconds names that whole second.
//!
//! The runs are the CLOCK's rather than any call's, so the rows below are recorded at instants
//! this suite chooses — each with a millisecond part of its own — and each window is then read
//! back over them. The rows differ along what the rule generalises over: the magnitude of each
//! end (both in milliseconds, both in seconds, one of each), which runs the window covers (all,
//! some, one, none), and whether the end lands before, on or after the instant of a run.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it.

use serde_json::{json, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    ComAmazonawsCloudwatchlogsLogs20140328State, ScheduledQueryExecution,
};

use crate::harness::{group_with_stream, now_ms, Logs};

/// The operations under test.
const HISTORY: &str = "GetScheduledQueryHistory";
const CREATE: &str = "CreateScheduledQuery";

/// The log group the query reads, which has to be one the account holds.
const GROUP: &str = "/aws/emulator/history-instant-magnitudes";

/// The IAM role the query executes under: "The ARN of the IAM role used to execute the query
/// and deliver results."
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-execution";

/// The pipeline the query runs.
const QUERY_STRING: &str = "fields @timestamp, @message | filter @message like /ERROR/";

/// The query the rows read the history of. Its own schedule names its next run a day away, so
/// the only runs in its history are the ones the case recorded.
const NAME: &str = "history-in-either-magnitude";

/// The milliseconds one second holds, which is what restates a bound in the other magnitude.
const MILLIS_PER_SECOND: i64 = 1_000;

/// The ids of the recorded runs, oldest first.
const RUN_IDS: [&str; 3] = ["run-oldest", "run-middle", "run-newest"];

/// The instants the recorded runs fall at: a minute apart inside the hour before now, each with
/// a millisecond part of its own, so a window stated in seconds covers a run whose instant is
/// not on a second boundary.
fn run_instants() -> [i64; 3] {
    let newest = (now_ms() - 5 * 60 * 1_000) / MILLIS_PER_SECOND * MILLIS_PER_SECOND + 457;
    [newest - 2 * 60 * 1_000, newest - 60 * 1_000, newest]
}

/// Create the query and record a run at each of `instants`, as already landed — what its
/// schedule makes due once those instants have passed, and what a read then reports.
async fn query_with_runs(logs: &Logs, instants: &[i64; 3]) {
    group_with_stream(logs, GROUP, "runs", None).await;
    logs.ok(
        CREATE,
        &json!({
            "name": NAME,
            "queryLanguage": "CWLI",
            "queryString": QUERY_STRING,
            "scheduleExpression": "rate(1 day)",
            "executionRoleArn": EXECUTION_ROLE,
            "logGroupIdentifiers": [GROUP],
            "startTimeOffset": 7_200,
        }),
    )
    .await;
    let state = logs
        .engine()
        .state::<ComAmazonawsCloudwatchlogsLogs20140328State>();
    let mut store = state.lock();
    let query = store
        .scheduled_query_mut(NAME)
        .unwrap_or_else(|| panic!("the scheduled query {NAME} is stored"));
    for (query_id, triggered_ms) in RUN_IDS.iter().zip(instants.iter()) {
        let mut execution = ScheduledQueryExecution::new((*query_id).to_string(), *triggered_ms, None);
        execution.status = "Complete".to_string();
        execution.run_landed = true;
        query.record_execution(execution);
    }
}

/// The `triggerHistory` one read over the window `(start, end)` reports.
async fn history_over(logs: &Logs, start: i64, end: i64) -> Vec<Value> {
    let page = logs
        .ok(
            HISTORY,
            &json!({"identifier": NAME, "startTime": start, "endTime": end}),
        )
        .await;
    page["triggerHistory"]
        .as_array()
        .unwrap_or_else(|| panic!("a history page carries triggerHistory, got {page}"))
        .clone()
}

/// One row: what it is a case of, the window's two ends built from the recorded instants, and
/// the ids the read must report, newest run first.
struct Window {
    case: &'static str,
    ends: fn(&[i64; 3]) -> (i64, i64),
    expected: Vec<&'static str>,
}

/// The ids of `found`, in the order the page reported them.
fn reported(found: &[Value]) -> Vec<String> {
    found
        .iter()
        .filter_map(|record| record["queryId"].as_str().map(str::to_string))
        .collect()
}

/// RULE: the window selects the runs whose instant it covers, read in the magnitude each of its
/// ends states — so one window and the same window restated in the other magnitude select the
/// same runs.
#[tokio::test]
async fn a_history_window_selects_the_same_runs_in_either_magnitude() {
    let logs = Logs::new();
    let instants = run_instants();
    query_with_runs(&logs, &instants).await;
    let newest_first: Vec<&str> = RUN_IDS.iter().rev().copied().collect();

    let seconds = |instant: i64| instant / MILLIS_PER_SECOND;
    let rows: Vec<Window> = vec![
        Window {
            case: "every run, in milliseconds",
            ends: |runs| (runs[0] - 1, runs[2] + 1),
            expected: newest_first.clone(),
        },
        Window {
            case: "every run, in seconds",
            ends: |runs| (runs[0] / 1_000 - 1, runs[2] / 1_000 + 1),
            expected: newest_first.clone(),
        },
        Window {
            case: "every run, from the epoch to a second past the newest",
            ends: |runs| (0, runs[2] / 1_000 + 1),
            expected: newest_first.clone(),
        },
        Window {
            case: "the one millisecond the oldest run was triggered at",
            ends: |runs| (runs[0], runs[0]),
            expected: vec![RUN_IDS[0]],
        },
        Window {
            case: "the one second the oldest run was triggered in",
            ends: |runs| (runs[0] / 1_000, runs[0] / 1_000),
            expected: vec![RUN_IDS[0]],
        },
        Window {
            case: "the middle run alone, in seconds",
            ends: |runs| (runs[1] / 1_000, runs[1] / 1_000),
            expected: vec![RUN_IDS[1]],
        },
        Window {
            case: "a start stated in seconds and an end stated in milliseconds",
            ends: |runs| (runs[1] / 1_000, runs[2] + 1),
            expected: vec![RUN_IDS[2], RUN_IDS[1]],
        },
        Window {
            case: "a start stated in milliseconds and an end stated in seconds",
            ends: |runs| (runs[0], runs[1] / 1_000),
            expected: vec![RUN_IDS[1], RUN_IDS[0]],
        },
        Window {
            case: "the second before the oldest run, in seconds",
            ends: |runs| (runs[0] / 1_000 - 1, runs[0] / 1_000 - 1),
            expected: Vec::new(),
        },
        Window {
            case: "the second after the newest run, in seconds",
            ends: |runs| (runs[2] / 1_000 + 1, runs[2] / 1_000 + 2),
            expected: Vec::new(),
        },
        Window {
            case: "the millisecond after the newest run",
            ends: |runs| (runs[2] + 1, runs[2] + 2),
            expected: Vec::new(),
        },
    ];

    for Window {
        case,
        ends,
        expected,
    } in rows
    {
        let (start, end) = ends(&instants);
        let found = history_over(&logs, start, end).await;
        let ids = reported(&found);
        assert_eq!(
            ids, expected,
            "case {case:?}: the window [{start}, {end}] over the runs {instants:?} must report \
             {expected:?}, got {ids:?}"
        );
    }

    // A read reports what happened and changes nothing a later caller sees: the same window
    // read again reports the same runs.
    let (start, end) = (seconds(instants[0]) - 1, seconds(instants[2]) + 1);
    let again = reported(&history_over(&logs, start, end).await);
    assert_eq!(
        again, newest_first,
        "case \"the same window read twice\": expected {newest_first:?}, got {again:?}"
    );
}

/// RULE: the two ends are compared as the INSTANTS they name, so a window is reversed when the
/// instant its end names precedes the instant its start names — whichever magnitudes they are
/// written in — and only then.
#[tokio::test]
async fn a_window_is_reversed_by_the_instants_its_ends_name() {
    let logs = Logs::new();
    let instants = run_instants();
    query_with_runs(&logs, &instants).await;

    // Each row: the case, the window's two ends, and whether the read is refused as a reversed
    // time range. The rows put the same pair of instants in both magnitudes and across them.
    let rows: Vec<(&str, i64, i64, bool)> = vec![
        (
            "both ends in milliseconds, in order",
            instants[0],
            instants[2],
            false,
        ),
        (
            "both ends in milliseconds, out of order",
            instants[2],
            instants[0],
            true,
        ),
        (
            "both ends in seconds, in order",
            instants[0] / MILLIS_PER_SECOND,
            instants[2] / MILLIS_PER_SECOND,
            false,
        ),
        (
            "both ends in seconds, out of order",
            instants[2] / MILLIS_PER_SECOND,
            instants[0] / MILLIS_PER_SECOND,
            true,
        ),
        (
            "a start in seconds before an end in milliseconds",
            instants[0] / MILLIS_PER_SECOND,
            instants[2],
            false,
        ),
        (
            "a start in milliseconds before an end in seconds",
            instants[0],
            instants[2] / MILLIS_PER_SECOND,
            false,
        ),
        (
            "a start in milliseconds after an end in seconds",
            instants[2],
            instants[0] / MILLIS_PER_SECOND,
            true,
        ),
    ];

    for (case, start, end, reversed) in rows {
        let (status, body) = logs
            .call(
                HISTORY,
                &json!({"identifier": NAME, "startTime": start, "endTime": end}),
            )
            .await;
        if reversed {
            assert_eq!(
                (status, crate::harness::error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: the window [{start}, {end}] names no time range, so it is \
                 refused, got {status} {body}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: the window [{start}, {end}] names a time range, so it is read, \
                 got {status} {body}"
            );
        }
    }
}

/// RULE: a reported record states its run's instant in epoch SECONDS — the second the run was
/// triggered in, whatever millisecond part the instant carries.
#[tokio::test]
async fn a_reported_record_states_its_instant_in_epoch_seconds() {
    let logs = Logs::new();
    let instants = run_instants();
    query_with_runs(&logs, &instants).await;

    let found = history_over(&logs, 0, now_ms()).await;
    assert_eq!(
        found.len(),
        RUN_IDS.len(),
        "expected every recorded run over a window covering them all, got {found:?}"
    );
    for record in &found {
        let id = record["queryId"]
            .as_str()
            .unwrap_or_else(|| panic!("a record carries queryId, got {record}"));
        let position = RUN_IDS
            .iter()
            .position(|named| *named == id)
            .unwrap_or_else(|| panic!("the record {id:?} is one of the recorded runs"));
        let expected = instants[position] / MILLIS_PER_SECOND;
        let stated = record["triggeredTimestamp"]
            .as_i64()
            .unwrap_or_else(|| panic!("a record is triggered at an instant, got {record}"));
        assert_eq!(
            stated, expected,
            "case {id:?}: a run triggered at {} ms is reported at the second {expected}, got \
             {stated}",
            instants[position]
        );
    }
}
