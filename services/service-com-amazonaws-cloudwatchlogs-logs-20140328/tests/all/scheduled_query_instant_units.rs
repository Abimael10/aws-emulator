// SPDX-License-Identifier: CC0-1.0

//! The UNIT the scheduled-query surface states an epoch instant in.
//!
//! The rule under test: an epoch instant of this service is stated in MILLISECONDS unless the
//! member itself says it is a number of seconds. The scheduled-query members say only "in Unix
//! epoch format"
//! (`smithy_query(query="shape",shape="GetScheduledQueryHistoryRequest,TriggerHistoryRecord,CreateScheduledQueryRequest")`:
//! `startTime`, `endTime`, `triggeredTimestamp`, `scheduleStartTime`, `scheduleEndTime`, all
//! `Timestamp` rendered `long` with no `timestampFormat`), so every one of them carries the
//! milliseconds the resource's own instants (`creationTime`, `lastUpdatedTime`,
//! `lastTriggeredTime`) and every log event of this service carry. Only a member that states a
//! number of seconds is seconds — the lookback offsets `startTimeOffset`/`endTimeOffset` ("the
//! time offset in seconds") and `StartQuery$startTime` ("the number of seconds since January 1,
//! 1970") — and none of those is read here.
//!
//! The tables span what the rule generalises over rather than one request: windows a whole
//! minute either side of a run, windows of the single millisecond it was triggered at, windows
//! one millisecond before and after it — the resolution a seconds-based reading cannot tell
//! apart — and the same windows restated in the seconds magnitude, which name instants of 1970
//! and therefore select nothing; and, for the schedule window, an end stated in milliseconds
//! that is still ahead against the same end restated in seconds, which is already long past.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a response
//! that is not computed from the shared store cannot pass.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::harness::{batch, group_with_stream, now_ms, Logs};

/// The operations under test.
const HISTORY: &str = "GetScheduledQueryHistory";
const CREATE: &str = "CreateScheduledQuery";
const GET: &str = "GetScheduledQuery";
const RESULTS: &str = "GetQueryResults";

/// The IAM role every case executes under: "The ARN of the IAM role used to execute the query
/// and deliver results."
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-execution";

/// The pipeline every case runs: the ERROR events of the window, as rows.
const QUERY_STRING: &str = "fields @timestamp, @message | filter @message like /ERROR/";

/// The one event every case seeds, which the pipeline above selects.
const MESSAGE: &str = "ERROR order 1 failed";

/// The lookback every case states, wide enough to hold the seeded event however long the test
/// takes to get from stamping it to the run.
const LOOKBACK_SECS: i64 = 7_200;

/// How long a read of the history is given to report a run the clock has made due. Generous on
/// purpose: the test waits for the CONDITION — a record being there — and never on a fixed span,
/// so a loaded and a fast host both reach the same verdict.
const HISTORY_DEADLINE: Duration = Duration::from_secs(180);

/// How long to wait between two looks while polling for one.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Seed `group` with [`MESSAGE`], stamped an hour back so every run's lookback holds it.
async fn seed_events(logs: &Logs, group: &str) {
    group_with_stream(logs, group, "runs", None).await;
    let events: [(i64, &str); 1] = [(now_ms() - 60 * 60 * 1_000, MESSAGE)];
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": "runs",
            "logEvents": batch(&events),
        }),
    )
    .await;
}

/// Create an ENABLED scheduled query named `name` whose schedule names its next run a day away,
/// so the only run in its history is its first — plus whatever `extras` the case states.
async fn create_query(logs: &Logs, name: &str, group: &str, extras: &Value) {
    let mut request = json!({
        "name": name,
        "queryLanguage": "CWLI",
        "queryString": QUERY_STRING,
        "scheduleExpression": "rate(1 day)",
        "executionRoleArn": EXECUTION_ROLE,
        "logGroupIdentifiers": [group],
        "startTimeOffset": LOOKBACK_SECS,
        "state": "ENABLED",
    });
    for (key, value) in extras.as_object().into_iter().flatten() {
        request[key.as_str()] = value.clone();
    }
    logs.ok(CREATE, &request).await;
}

/// The `triggerHistory` one read of `name`'s history over the window `(start, end)` reports.
async fn history_over(logs: &Logs, name: &str, start: i64, end: i64) -> Vec<Value> {
    let page = logs
        .ok(
            HISTORY,
            &json!({"identifier": name, "startTime": start, "endTime": end}),
        )
        .await;
    page["triggerHistory"]
        .as_array()
        .unwrap_or_else(|| panic!("a history page carries triggerHistory, got {page}"))
        .clone()
}

/// Poll `name`'s history over the window a client asks for — the milliseconds around now, as the
/// SDKs' own examples pass them — until it reports a run, answering that record.
///
/// The runs are owed by the clock, so the condition — not a span — is what is waited for.
async fn first_run_over_a_millisecond_window(logs: &Logs, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + HISTORY_DEADLINE;
    loop {
        let start = now_ms() - 60 * 1_000;
        let end = now_ms() + 60 * 1_000;
        // The history is reported newest run first, so the oldest is the last entry.
        if let Some(oldest) = history_over(logs, name, start, end).await.last() {
            return oldest.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the run the schedule made due to be reported over the \
             millisecond window [{start}, {end}] of {name}, got no record at all"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the window table: how the read states its window relative to the instant the run
/// was triggered at, and whether that window selects the run.
struct Window {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The window's two ends, built from the instant the run was triggered at (in epoch
    /// milliseconds).
    ends: fn(i64) -> (i64, i64),
    /// Whether the run falls inside that window.
    selects: bool,
}

/// The window a read states selects the runs of THAT window, read in epoch milliseconds: a
/// client that asks for the milliseconds around now sees its run, the single millisecond the run
/// was triggered at selects it, a millisecond either side of it does not, and the very same
/// window restated in the seconds magnitude names instants of 1970 and selects nothing.
#[tokio::test]
async fn the_history_window_is_read_in_epoch_milliseconds() {
    let logs = Logs::new();
    let group = "/aws/emulator/instant-units/window";
    let name = "window-in-milliseconds";
    seed_events(&logs, group).await;
    let created_at = now_ms();
    create_query(&logs, name, group, &json!({})).await;

    let record = first_run_over_a_millisecond_window(&logs, name, name).await;
    assert_eq!(
        record["executionStatus"].as_str(),
        Some("Complete"),
        "a run that produced its results is complete, got {record}"
    );
    let triggered = record["triggeredTimestamp"]
        .as_i64()
        .unwrap_or_else(|| panic!("a record is triggered at an instant, got {record}"));

    // The instant reported is the MILLISECOND the run was triggered at: it lies inside the
    // milliseconds this test spans, which a value in seconds (a thousandth of it) cannot.
    let now = now_ms();
    assert!(
        triggered >= created_at && triggered <= now,
        "expected the triggered instant to be the milliseconds between the create \
         ({created_at}) and now ({now}), got {triggered}"
    );

    // The run's own query, which the record is a handle on, really answered the window's rows.
    let query_id = record["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("a record carries queryId, got {record}"))
        .to_string();
    let results = logs.ok(RESULTS, &json!({"queryId": query_id})).await;
    assert_eq!(
        results["status"].as_str(),
        Some("Complete"),
        "the run's query has run, got {results}"
    );

    // One instant of one run, reported by the history and by the query itself: both state it in
    // the same epoch milliseconds, so the two values are equal rather than a factor apart.
    let reported = logs.ok(GET, &json!({"identifier": name})).await;
    assert_eq!(
        reported["lastTriggeredTime"].as_i64(),
        Some(triggered),
        "the query's last triggered instant is that run's, in the same unit, got {reported}"
    );

    let rows: [Window; 8] = [
        Window {
            case: "a minute either side of the run, as a client asks for it",
            ends: |triggered| (triggered - 60 * 1_000, triggered + 60 * 1_000),
            selects: true,
        },
        Window {
            case: "the single millisecond the run was triggered at",
            ends: |triggered| (triggered, triggered),
            selects: true,
        },
        Window {
            case: "a window ending at the millisecond the run was triggered at",
            ends: |triggered| (triggered - 1, triggered),
            selects: true,
        },
        Window {
            case: "a window starting at the millisecond the run was triggered at",
            ends: |triggered| (triggered, triggered + 1),
            selects: true,
        },
        Window {
            case: "the single millisecond after the run",
            ends: |triggered| (triggered + 1, triggered + 2),
            selects: false,
        },
        Window {
            case: "the single millisecond before the run",
            ends: |triggered| (triggered - 2, triggered - 1),
            selects: false,
        },
        Window {
            case: "the same minute either side of the run, restated in the seconds magnitude",
            ends: |triggered| (triggered / 1_000 - 60, triggered / 1_000 + 60),
            selects: false,
        },
        Window {
            case: "the whole of 1970 up to the run's instant in the seconds magnitude",
            ends: |triggered| (0, triggered / 1_000),
            selects: false,
        },
    ];

    for Window {
        case,
        ends,
        selects,
    } in rows
    {
        let (start, end) = ends(triggered);
        let found = history_over(&logs, name, start, end).await;
        let reported: Vec<&str> = found
            .iter()
            .filter_map(|record| record["queryId"].as_str())
            .collect();
        let expected: Vec<&str> = if selects {
            vec![query_id.as_str()]
        } else {
            Vec::new()
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: the window [{start}, {end}] of the run triggered at {triggered} \
             must report {expected:?}, got {found:?}"
        );
    }
}

/// One row of the schedule-window table: how the query's own `scheduleEndTime` is stated, and
/// whether the schedule still owes it a run.
struct Schedule {
    /// What this row varies, named in every assertion.
    case: &'static str,
    /// The name the query is created under.
    name: &'static str,
    /// Its `scheduleEndTime`, from the instant the row is built at (in epoch milliseconds).
    end: fn(i64) -> i64,
    /// Whether the schedule owes this query a run by now.
    due: bool,
}

/// A query's own schedule window is read in the same epoch milliseconds: "The query will stop
/// executing after this time", so an end stated in milliseconds that is still ahead leaves a run
/// due, while the very same end restated in the seconds magnitude is an instant of 1970 — long
/// past — and owes nothing.
#[tokio::test]
async fn the_schedule_window_is_read_in_epoch_milliseconds() {
    let logs = Logs::new();
    let group = "/aws/emulator/instant-units/schedule";
    seed_events(&logs, group).await;
    let now = now_ms();

    let rows: [Schedule; 3] = [
        Schedule {
            case: "a schedule end in milliseconds that is still ahead",
            name: "schedule-end-ahead-in-millis",
            end: |now| now + 24 * 60 * 60 * 1_000,
            due: true,
        },
        Schedule {
            case: "the same schedule end restated in the seconds magnitude",
            name: "schedule-end-ahead-in-seconds",
            end: |now| (now + 24 * 60 * 60 * 1_000) / 1_000,
            due: false,
        },
        Schedule {
            case: "a schedule end in milliseconds that has passed",
            name: "schedule-end-passed-in-millis",
            end: |now| now - 60 * 1_000,
            due: false,
        },
    ];

    for row in &rows {
        create_query(
            &logs,
            row.name,
            group,
            &json!({"scheduleEndTime": (row.end)(now)}),
        )
        .await;
    }

    // The query whose window is still open is the one whose run falls due; every other query has
    // had the same chance of the clock by the time that run is recorded, so one that still
    // reports nothing is one the schedule owes nothing.
    let eligible = &rows[0];
    let record = first_run_over_a_millisecond_window(&logs, eligible.case, eligible.name).await;
    assert_eq!(
        record["executionStatus"].as_str(),
        Some("Complete"),
        "case {:?}: a run that produced its results is complete, got {record}",
        eligible.case
    );

    for row in rows.iter().filter(|row| !row.due) {
        let found = history_over(&logs, row.name, 0, now_ms() + 24 * 60 * 60 * 1_000).await;
        assert!(
            found.is_empty(),
            "case {:?}: a query whose schedule window ended at {} owes no run, got {found:?}",
            row.case,
            (row.end)(now)
        );
    }
}
