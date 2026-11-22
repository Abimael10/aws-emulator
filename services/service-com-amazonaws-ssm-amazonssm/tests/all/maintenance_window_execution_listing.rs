// SPDX-License-Identifier: CC0-1.0

//! `DescribeMaintenanceWindowExecutions` end to end: which runs of a maintenance window one
//! listing reports, in what order, narrowed by what and bounded by how much.
//!
//! "Lists the executions of a maintenance window"
//! (`smithy_query(query="doc",shape="DescribeMaintenanceWindowExecutions")`): the listing
//! reports the runs THIS endpoint holds for the `WindowId` it names, in the order they
//! started, narrowed by the two documented filter keys (`ExecutedBefore` and `ExecutedAfter`,
//! `DescribeMaintenanceWindowExecutionsRequest$Filters`) and bounded by the modelled
//! `MaintenanceWindowMaxResults` range. It models no not-found error, so a window that has run
//! nothing — including a `WindowId` nothing created — is reported as holding no execution
//! (HTTP 200 with an empty list) rather than refused.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router_with_engine`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. The ONE thing seeded through the engine is a run of a
//! window, because nothing in this emulator picks up a maintenance window's tasks — the same
//! reading Run Command takes of a command's invocations — so no call of this service records
//! one.
//!
//! The suites are tables: the rows differ along what the account holds, the window asked for,
//! the Region asked through, how many runs a listing walks, how large a page it asks for,
//! which instants it bounds by and how many values a filter carries.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::maintenance_window_executions::MaintenanceWindowExecutionRecord;
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router_with_engine, REGION,
};

/// A second Region, to pin that a run is reached through the endpoint of the Region its
/// window lives in.
const OTHER_REGION: &str = "eu-central-1";

/// A well-formed `MaintenanceWindowId` (len 20, `^mw-[0-9a-f]{17}$`) nothing created, for the
/// rows that ask about a window this account does not hold.
const UNCREATED_WINDOW: &str = "mw-aaaaaaaaaaaaaaaaa";

/// The instant the first seeded run started, as a whole number of seconds since the epoch
/// (2030-01-01T00:00:00Z).
const FIRST_START: u64 = 1_893_456_000;

/// How far apart the seeded runs start, in seconds.
const RUN_SPACING: u64 = 3_600;

/// The page size the modelled `MaintenanceWindowMaxResults` admits at its low end
/// (`range 10..100`), which is the smallest page this listing serves.
const MIN_PAGE: usize = 10;

/// A clock reading `seconds` after the epoch.
fn epoch(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

/// `seconds` after the epoch as a date/time string in the ISO 8601 format a filter value is
/// written in ("such as 2024-11-04T05:00:00Z",
/// `DescribeMaintenanceWindowExecutionsRequest$Filters`).
fn iso8601(seconds: u64) -> String {
    ::chrono::DateTime::from_timestamp(seconds as i64, 0)
        .expect("the seconds name an instant")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// The `MaintenanceWindowExecutionId` of the run at `ordinal`, in the modelled hyphenated
/// hexadecimal form (len 36).
fn run_id(ordinal: usize) -> String {
    format!("{ordinal:08x}-1111-2222-3333-444444444444")
}

/// What one seeded run holds beyond its ids: the status it reached, when it started and —
/// once it is over — ended, and the detail that status carries.
struct SeededRun<'a> {
    status: &'a str,
    started: u64,
    ended: Option<u64>,
    status_details: Option<&'a str>,
}

/// Record one run of `window_id` in `region`, the way the service picking the window's tasks
/// up would.
///
/// Seeded through the engine because no call of this service records a run; every member a
/// listing reports is set here, so a listing that invents one is visible.
fn seed_run(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    window_id: &str,
    window_execution_id: &str,
    run: SeededRun<'_>,
) {
    let record = MaintenanceWindowExecutionRecord {
        window_execution_id: window_execution_id.to_string(),
        window_id: window_id.to_string(),
        region: region.to_string(),
        started_at: epoch(run.started),
        ended_at: run.ended.map(epoch),
        status: run.status.to_string(),
        status_details: run.status_details.map(str::to_string),
        task_execution_ids: Vec::new(),
    };
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .insert_maintenance_window_execution(record);
}

/// Create a maintenance window at `REGION`'s endpoint, returning the `WindowId` the create
/// minted.
async fn create_window(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateMaintenanceWindow",
        &json!({
            "Name": name,
            "Schedule": "rate(30 days)",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must be created, got {status} {created}"
    );
    created["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a WindowId, got {created}"))
        .to_string()
}

/// One listing at `region`'s endpoint, returning `(status, body)`.
async fn listing(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "DescribeMaintenanceWindowExecutions",
        body,
    )
    .await
}

/// The `WindowExecutionId`s a listing reported, in the order it reported them.
fn listed_ids(response: &Value) -> Vec<String> {
    response["WindowExecutions"]
        .as_array()
        .map(|executions| {
            executions
                .iter()
                .map(|execution| {
                    execution["WindowExecutionId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a listing reports the runs this endpoint holds for the window it names — so a window
/// holding none is reported as an EMPTY list with HTTP 200, never refused. The rows differ
/// along what the account holds, the window asked for and the Region asked through.
#[tokio::test]
async fn a_window_holding_no_run_is_listed_empty() {
    let (router, engine) = router_with_engine();

    // Before anything is held at all.
    for (case, window) in [
        ("a fresh account", UNCREATED_WINDOW),
        ("another well-formed id", "mw-0123456789abcdef0"),
    ] {
        let (status, response) = listing(&router, REGION, &json!({ "WindowId": window })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a listing of no run succeeds, got {status} {response}"
        );
        assert_eq!(
            listed_ids(&response),
            Vec::<String>::new(),
            "case {case:?}: expected an empty execution list, got {response}"
        );
        assert!(
            response["NextToken"].is_null(),
            "case {case:?}: an empty page continues nowhere, got {response}"
        );
    }

    // Holding a window — tasks and all — records no run of it: a window is a schedule, and the
    // executions this listing reports are the runs something picked up.
    let window = create_window(&router, "execution-listing-window").await;
    let (status, registered) = call(
        &router,
        "RegisterTaskWithMaintenanceWindow",
        &json!({
            "WindowId": window,
            "TaskArn": "AWS-RunShellScript",
            "TaskType": "RUN_COMMAND",
            "Priority": 1,
            "MaxConcurrency": "1",
            "MaxErrors": "1",
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the task must be registered, got {status} {registered}"
    );
    let (status, response) = listing(&router, REGION, &json!({ "WindowId": window })).await;
    assert_eq!(
        status, 200,
        "a window records no run, got {status} {response}"
    );
    assert!(
        listed_ids(&response).is_empty(),
        "expected an empty execution list once a window exists, got {response}"
    );

    // Now hold one run, and ask for the window and Region it is NOT held under.
    seed_run(
        &engine,
        REGION,
        &window,
        &run_id(0),
        SeededRun {
            status: "SUCCESS",
            started: FIRST_START,
            ended: Some(FIRST_START + 60),
            status_details: None,
        },
    );

    // (case, the Region asked through, the window asked for)
    for (case, region, asked) in [
        ("another window of the same Region", REGION, UNCREATED_WINDOW),
        (
            "the holding window at another Region's endpoint",
            OTHER_REGION,
            window.as_str(),
        ),
    ] {
        let (status, response) = listing(&router, region, &json!({ "WindowId": asked })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an empty listing, got {status} {response}"
        );
        assert!(
            listed_ids(&response).is_empty(),
            "case {case:?}: expected an empty execution list, got {response}"
        );
    }
}

/// RULE: what a listing reports comes off the records the endpoint holds, member by member —
/// and the single read of the same run reports the same values, so a caller that lists and
/// then reads sees ONE execution. Listing twice reports the same thing: a listing changes
/// nothing a later caller sees. The rows differ along whether the run has finished and whether
/// its status carries a detail.
#[tokio::test]
async fn a_held_run_is_listed_with_what_the_record_holds() {
    let (router, engine) = router_with_engine();
    let window = create_window(&router, "execution-projection-window").await;

    // (ordinal, status, when it ended, the detail it carries)
    let seeded: [(usize, &str, Option<u64>, Option<&str>); 3] = [
        (0, "SUCCESS", Some(FIRST_START + 120), None),
        (1, "IN_PROGRESS", None, None),
        (
            2,
            "FAILED",
            Some(FIRST_START + 2 * RUN_SPACING + 30),
            Some("One or more tasks failed"),
        ),
    ];
    for (ordinal, status, ended, details) in seeded {
        seed_run(
            &engine,
            REGION,
            &window,
            &run_id(ordinal),
            SeededRun {
                status,
                started: FIRST_START + ordinal as u64 * RUN_SPACING,
                ended,
                status_details: details,
            },
        );
    }

    for round in 1..=2 {
        let (status, response) = listing(&router, REGION, &json!({ "WindowId": window })).await;
        assert_eq!(
            status, 200,
            "round {round}: the listing must succeed, got {status} {response}"
        );
        assert_eq!(
            listed_ids(&response),
            (0..seeded.len()).map(run_id).collect::<Vec<String>>(),
            "round {round}: expected the runs in the order they started, got {response}"
        );

        for (ordinal, expected_status, ended, details) in seeded {
            let reported = &response["WindowExecutions"][ordinal];
            let case = format!("the run at {ordinal}");
            assert_eq!(
                reported["WindowId"].as_str(),
                Some(window.as_str()),
                "case {case:?}: expected the window the record holds, got {reported}"
            );
            assert_eq!(
                reported["Status"].as_str(),
                Some(expected_status),
                "case {case:?}: expected the status the record holds, got {reported}"
            );
            assert_eq!(
                reported["StatusDetails"].as_str(),
                details,
                "case {case:?}: expected the detail the record holds, got {reported}"
            );
            assert_eq!(
                reported["StartTime"].as_f64(),
                Some((FIRST_START + ordinal as u64 * RUN_SPACING) as f64),
                "case {case:?}: expected the instant the record holds, got {reported}"
            );
            assert_eq!(
                reported["EndTime"].as_f64(),
                ended.map(|at| at as f64),
                "case {case:?}: expected the end the record holds, got {reported}"
            );
        }
    }

    // The single read of one of those runs agrees with the listing, member for member.
    let (status, read) = call(
        &router,
        "GetMaintenanceWindowExecution",
        &json!({ "WindowExecutionId": run_id(2) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the held run must be readable, got {status} {read}"
    );
    assert_eq!(
        read["Status"].as_str(),
        Some("FAILED"),
        "expected the status the listing reported, got {read}"
    );
    assert_eq!(
        read["StatusDetails"].as_str(),
        Some("One or more tasks failed"),
        "expected the detail the listing reported, got {read}"
    );
    assert_eq!(
        read["StartTime"].as_f64(),
        Some((FIRST_START + 2 * RUN_SPACING) as f64),
        "expected the instant the listing reported, got {read}"
    );
}

/// RULE: a listing walks the runs of the window it names, bounded by the page size asked for
/// and resumed at the id it handed back. The rows differ along the page size and how far into
/// the records the page starts.
#[tokio::test]
async fn a_listing_pages_by_the_id_it_resumes_at() {
    let (router, engine) = router_with_engine();
    let window = create_window(&router, "execution-paging-window").await;

    // One more record than the smallest page the modelled range admits, so a page leaves a
    // remainder — seeded out of order, so the reported order is the records' own.
    let held = MIN_PAGE + 1;
    for ordinal in (0..held).rev() {
        seed_run(
            &engine,
            REGION,
            &window,
            &run_id(ordinal),
            SeededRun {
                status: "SUCCESS",
                started: FIRST_START + ordinal as u64 * RUN_SPACING,
                ended: None,
                status_details: None,
            },
        );
    }

    // (case, MaxResults, how many runs the page carries, whether it continues)
    let rows: [(&str, Option<usize>, usize, bool); 3] = [
        ("no page size asked for", None, held, false),
        (
            "the smallest page the range admits",
            Some(MIN_PAGE),
            MIN_PAGE,
            true,
        ),
        ("a page larger than the records held", Some(50), held, false),
    ];

    for (case, max_results, expected, continues) in rows {
        let mut body = json!({ "WindowId": window });
        if let Some(max_results) = max_results {
            body["MaxResults"] = json!(max_results);
        }
        let (status, response) = listing(&router, REGION, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must succeed, got {status} {response}"
        );
        let reported = listed_ids(&response);
        assert_eq!(
            reported,
            (0..expected).map(run_id).collect::<Vec<String>>(),
            "case {case:?}: expected {expected} runs in the order they started, got {reported:?}"
        );
        assert_eq!(
            !response["NextToken"].is_null(),
            continues,
            "case {case:?}: expected continues={continues}, got {response}"
        );

        // A page that continues resumes at the id it handed back, and the two pages together
        // are every record held, each once.
        if continues {
            let token = response["NextToken"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                token,
                run_id(expected),
                "case {case:?}: expected the id the next page starts at, got {response}"
            );
            let (status, rest) = listing(
                &router,
                REGION,
                &json!({ "WindowId": window, "NextToken": token }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the resumed page must succeed, got {status} {rest}"
            );
            assert_eq!(
                listed_ids(&rest),
                (expected..held).map(run_id).collect::<Vec<String>>(),
                "case {case:?}: expected the runs past the page, got {rest}"
            );
        }
    }

    // A token naming no run of this result is not one this listing handed out.
    for (case, token) in [
        ("an id nothing recorded", run_id(99)),
        ("a value of no shape at all", "not-a-token".to_string()),
    ] {
        let (status, response) = listing(
            &router,
            REGION,
            &json!({ "WindowId": window, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the token refusal, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the token refusal, got {response}"
        );
    }

    // A page size outside the modelled range never reaches the listing.
    for (case, max_results) in [("one below the minimum", 9), ("one above the maximum", 101)] {
        let (status, response) = listing(
            &router,
            REGION,
            &json!({ "WindowId": window, "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {max_results} is outside the modelled range, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the boundary refusal, got {response}"
        );
    }
}

/// RULE: the two documented filter keys narrow a listing by the instant a run started — the
/// bound included — and a filter's values union while filters intersect; a key outside the
/// documented set, or a value naming no instant, narrows nothing, so the request is refused
/// rather than served everything. The rows differ along the key, the bounds asked for and how
/// many values each filter carries.
#[tokio::test]
async fn a_listing_narrows_by_the_documented_instant_filters() {
    let (router, engine) = router_with_engine();
    let window = create_window(&router, "execution-filter-window").await;

    // Four runs, one every hour from the first.
    let held = 4;
    for ordinal in 0..held {
        seed_run(
            &engine,
            REGION,
            &window,
            &run_id(ordinal),
            SeededRun {
                status: "SUCCESS",
                started: FIRST_START + ordinal as u64 * RUN_SPACING,
                ended: None,
                status_details: None,
            },
        );
    }
    let at = |ordinal: u64| iso8601(FIRST_START + ordinal * RUN_SPACING);

    // (case, the filters sent, the ordinals selected)
    let rows: [(&str, Value, Vec<usize>); 7] = [
        (
            "every run started before a bound past all of them",
            json!([{ "Key": "ExecutedBefore", "Values": [iso8601(FIRST_START + 10 * RUN_SPACING)] }]),
            (0..held).collect(),
        ),
        (
            "the runs started before a bound inside the series",
            json!([{ "Key": "ExecutedBefore", "Values": [at(1)] }]),
            vec![0, 1],
        ),
        (
            "the runs started after a bound inside the series",
            json!([{ "Key": "ExecutedAfter", "Values": [at(2)] }]),
            vec![2, 3],
        ),
        (
            "the two bounds together, which intersect",
            json!([
                { "Key": "ExecutedAfter", "Values": [at(1)] },
                { "Key": "ExecutedBefore", "Values": [at(2)] },
            ]),
            vec![1, 2],
        ),
        (
            "two values of one key, which union to the widest of them",
            json!([{ "Key": "ExecutedBefore", "Values": [at(0), at(2)] }]),
            vec![0, 1, 2],
        ),
        (
            "a bound no run reaches",
            json!([{ "Key": "ExecutedBefore", "Values": [iso8601(FIRST_START - RUN_SPACING)] }]),
            Vec::new(),
        ),
        (
            "bounds that select nothing between them",
            json!([
                { "Key": "ExecutedAfter", "Values": [at(3)] },
                { "Key": "ExecutedBefore", "Values": [at(0)] },
            ]),
            Vec::new(),
        ),
    ];

    for (case, filters, selected) in rows {
        let (status, response) = listing(
            &router,
            REGION,
            &json!({ "WindowId": window, "Filters": filters }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must succeed, got {status} {response}"
        );
        assert_eq!(
            listed_ids(&response),
            selected
                .iter()
                .copied()
                .map(run_id)
                .collect::<Vec<String>>(),
            "case {case:?}: expected the runs {filters} selects, got {response}"
        );
    }

    // A key outside the documented set names no property of a run.
    for (case, key) in [
        ("the run's own id", "WindowExecutionId"),
        ("a key of the sibling listing", "STATUS"),
        ("a documented key in another case", "executedbefore"),
    ] {
        let (status, response) = listing(
            &router,
            REGION,
            &json!({
                "WindowId": window,
                "Filters": [{ "Key": key, "Values": [at(0)] }],
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {key} narrows nothing, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the filter refusal, got {response}"
        );
        assert!(
            error_message(&response).contains(key),
            "case {case:?}: the refusal names the key it refused, got {response}"
        );
    }

    // A value of a documented key has to name an instant.
    for (case, value) in [
        ("text naming no instant", "yesterday"),
        ("a date with no time of day", "2030-01-01"),
        ("the instant as epoch seconds", "1893456000"),
    ] {
        let (status, response) = listing(
            &router,
            REGION,
            &json!({
                "WindowId": window,
                "Filters": [{ "Key": "ExecutedAfter", "Values": [value] }],
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {value:?} names no instant, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the filter refusal, got {response}"
        );
        assert!(
            error_message(&response).contains(value),
            "case {case:?}: the refusal names the value it refused, got {response}"
        );
    }
}

/// A value that is not shaped like a `MaintenanceWindowId` never reaches the listing: the
/// member's length and pattern are MODELLED, so the boundary walk refuses it. The rows differ
/// along which part of the shape they break.
#[tokio::test]
async fn a_value_shaped_like_no_window_id_is_refused_by_the_boundary() {
    let router = router_with_engine().0;

    // (case, the value sent)
    let rows: [(&str, &str); 5] = [
        ("a value of no prefix", "0123456789abcdef0123"),
        ("a value too short", "mw-0123456789abcdef"),
        ("a value too long", "mw-0123456789abcdef01"),
        (
            "hexadecimal digits in upper case, which the pattern excludes",
            "mw-0123456789ABCDEF0",
        ),
        (
            "an execution id, which is another id space",
            "11111111-2222-3333-4444-555555555555",
        ),
    ];

    for (case, value) in rows {
        let (status, response) = listing(&router, REGION, &json!({ "WindowId": value })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {value:?} is no window id, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the boundary refusal, got {response}"
        );
    }
}
