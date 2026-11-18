// SPDX-License-Identifier: CC0-1.0

//! `DescribeMaintenanceWindowSchedule` end to end: the "upcoming executions of a maintenance
//! window" are the instants the schedule a create persisted names.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through
//! the multi-service router. Nothing reaches into the state struct, so a response that is not
//! computed from persisted state cannot pass: every report follows a create.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **The report is the schedule's own series.** For each window asked about, the
//!    executions reported are the successive instants its stored schedule names inside the
//!    interval its `StartDate` and `EndDate` bound, in ascending order, written in the zone
//!    the window states its schedule in. The rows differ along the form of the schedule
//!    (cron, cron with a whole-day offset, a one-time instant), the zone it is read in and
//!    the bounds the window carries.
//! 2. **The first of them is the window's `NextExecutionTime`.** A single read of a window
//!    and a report of its upcoming executions answer from the ONE series.
//! 3. **What is asked about is what the request states.** A `WindowId` asks about that window
//!    alone (and an id this endpoint holds no window under is refused
//!    `DoesNotExistException`), a request naming none asks about every window the endpoint
//!    holds — their executions merged in the order they are scheduled — and `Targets` ask
//!    about the windows those targets are registered with.
//! 4. **A page holds at most `MaxResults` executions** and the `NextToken` resumes the walk at
//!    the execution the page stopped at, so a walk over the pages reports each execution once.
//! 5. **`Filters` bound the range reported** to the executions scheduled before or after the
//!    instants they state — both ends exclusive — each filter's values unioning and the
//!    filters intersecting, and a key outside that set is refused before anything is read.
//!    Every bound, a filter's and a page's alike, is taken against the instant the window
//!    OPENS at, which a `ScheduleOffset` moves whole days past the instant its expression
//!    named.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a window's executions are shown to be reported at the endpoint of the
/// ONE Region it was created through.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// A node id the suite registers a window's targets against.
const A_NODE: &str = "i-0123456789abcdef0";

/// `CreateMaintenanceWindow` at `region`'s endpoint, asserting it succeeded and answering the
/// `WindowId` it minted.
async fn create_ok(router: &MultiServiceRouter, case: &str, region: &str, body: &Value) -> String {
    let (status, response) = call_in_region(router, region, "CreateMaintenanceWindow", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMaintenanceWindow {body} must succeed, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// A create body carrying the members the model makes required, the schedule stated.
fn minimal(name: &str, schedule: &str) -> Value {
    json!({
        "Name": name,
        "Schedule": schedule,
        "Duration": 2,
        "Cutoff": 1,
        "AllowUnassociatedTargets": false,
    })
}

/// `DescribeMaintenanceWindowSchedule` at `region`'s endpoint with `body`.
async fn schedule(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "DescribeMaintenanceWindowSchedule", body).await
}

/// The executions the report serves at `region`'s endpoint for `body`, asserting it was
/// served.
async fn executions(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> (Vec<Value>, Option<String>) {
    let (status, response) = schedule(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMaintenanceWindowSchedule {body} must be served, got {status} \
         {response}"
    );
    let served = response["ScheduledWindowExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let token = response["NextToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    (served, token)
}

/// The `ExecutionTime`s reported, in the order the report states them.
fn execution_times(served: &[Value]) -> Vec<String> {
    served
        .iter()
        .map(|execution| {
            execution["ExecutionTime"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The `(WindowId, ExecutionTime)` pairs reported, so a merged report is read window by
/// window.
fn reported_pairs(served: &[Value]) -> Vec<(String, String)> {
    served
        .iter()
        .map(|execution| {
            (
                execution["WindowId"].as_str().unwrap_or_default().to_string(),
                execution["ExecutionTime"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// RULE 1: the executions reported for a window are the successive instants its stored
/// schedule names inside the interval it is active over, in ascending order and written in the
/// zone the window states its schedule in.
///
/// Every row activates in 2035, so the instants named are the same however fast the host runs
/// and whenever the suite is run.
#[tokio::test]
async fn the_report_is_the_schedules_own_series() {
    let router = router();
    // (case, the create body, the page asked for, the execution times reported)
    let rows: Vec<(&str, Value, i32, Vec<&str>)> = vec![
        (
            // 2035-01-01 is a Monday, so the series starts the day the window becomes active.
            "a weekly cron window",
            json!({
                "Name": "weekly-window",
                "Schedule": "cron(0 8 ? * MON *)",
                "StartDate": "2035-01-01T00:00:00Z",
                "Duration": 2,
                "Cutoff": 1,
                "AllowUnassociatedTargets": false,
            }),
            3,
            vec![
                "2035-01-01T08:00Z",
                "2035-01-08T08:00Z",
                "2035-01-15T08:00Z",
            ],
        ),
        (
            "a daily cron window read in its own zone",
            json!({
                "Name": "tokyo-window",
                "Schedule": "cron(0 0 ? * * *)",
                "ScheduleTimezone": "Asia/Tokyo",
                "StartDate": "2034-12-31T12:00:00+09:00",
                "Duration": 4,
                "Cutoff": 0,
                "AllowUnassociatedTargets": true,
            }),
            2,
            vec!["2035-01-01T00:00+09:00", "2035-01-02T00:00+09:00"],
        ),
        (
            // "The number of days to wait after the date and time specified by a cron
            // expression before running the maintenance window": the Tuesdays of 2035 are the
            // 2nd and the 9th of January, each waited two whole days on.
            "a cron window waiting whole days",
            json!({
                "Name": "offset-window",
                "Schedule": "cron(30 23 ? * TUE *)",
                "ScheduleOffset": 2,
                "StartDate": "2035-01-01T00:00:00Z",
                "Duration": 6,
                "Cutoff": 2,
                "AllowUnassociatedTargets": false,
            }),
            2,
            vec!["2035-01-04T23:30Z", "2035-01-11T23:30Z"],
        ),
        (
            "a one-time window, which runs once and no more",
            json!({
                "Name": "one-time-window",
                "Schedule": "at(2035-06-01T12:00:00)",
                "Duration": 1,
                "Cutoff": 0,
                "AllowUnassociatedTargets": false,
            }),
            5,
            vec!["2035-06-01T12:00Z"],
        ),
        (
            "a window that becomes inactive before its next instant",
            json!({
                "Name": "bounded-window",
                "Schedule": "cron(0 8 ? * MON *)",
                "StartDate": "2035-01-01T00:00:00Z",
                "EndDate": "2035-01-10T00:00:00Z",
                "Duration": 2,
                "Cutoff": 1,
                "AllowUnassociatedTargets": false,
            }),
            5,
            vec!["2035-01-01T08:00Z", "2035-01-08T08:00Z"],
        ),
    ];

    for (case, body, max_results, expected) in rows {
        let name = body["Name"].as_str().unwrap_or_default().to_string();
        let window_id = create_ok(&router, case, REGION, &body).await;
        let (served, token) = executions(
            &router,
            case,
            REGION,
            &json!({ "WindowId": &window_id, "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            execution_times(&served),
            expected
                .iter()
                .map(|time| (*time).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected {expected:?}, got {served:?}"
        );
        for execution in &served {
            assert_eq!(
                (
                    execution["WindowId"].as_str(),
                    execution["Name"].as_str()
                ),
                (Some(window_id.as_str()), Some(name.as_str())),
                "case {case:?}: expected each execution to name the window it belongs to, got \
                 {execution}"
            );
        }
        // Fewer executions than the page asked for: nothing is waiting past this page.
        if served.len() < max_results as usize {
            assert_eq!(
                token, None,
                "case {case:?}: expected no NextToken past a partial page, got {token:?}"
            );
        }
        // The report is a READ: asking again answers the same series.
        let (again, _) = executions(
            &router,
            case,
            REGION,
            &json!({ "WindowId": &window_id, "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            execution_times(&again),
            execution_times(&served),
            "case {case:?}: expected a second report to answer the same series, got {again:?}"
        );
    }
}

/// RULE 2: the first execution reported for a window is that window's `NextExecutionTime`, so
/// a single read and this report answer from the one series.
///
/// The rows differ along the form of the schedule — including the `rate(...)` series, whose
/// instants are anchored at the create rather than stated — and every one of them names its
/// next instant days away, so the two reads cannot straddle it.
#[tokio::test]
async fn the_first_execution_is_the_windows_next_execution_time() {
    let router = router();
    let rows: Vec<(&str, Value)> = vec![
        (
            "a weekly cron window activating in 2035",
            json!({
                "Name": "next-weekly",
                "Schedule": "cron(0 8 ? * MON *)",
                "StartDate": "2035-01-01T00:00:00Z",
                "Duration": 2,
                "Cutoff": 1,
                "AllowUnassociatedTargets": false,
            }),
        ),
        (
            "a rate window, whose series is anchored at the create",
            minimal("next-rate", "rate(7 days)"),
        ),
        (
            "a one-time window",
            minimal("next-once", "at(2035-06-01T12:00:00)"),
        ),
        (
            "a cron window waiting whole days",
            json!({
                "Name": "next-offset",
                "Schedule": "cron(30 23 ? * TUE *)",
                "ScheduleOffset": 2,
                "ScheduleTimezone": "Asia/Seoul",
                "StartDate": "2035-01-01T00:00:00Z",
                "Duration": 6,
                "Cutoff": 2,
                "AllowUnassociatedTargets": false,
            }),
        ),
    ];

    for (case, body) in rows {
        let window_id = create_ok(&router, case, REGION, &body).await;
        let (status, read) = call_in_region(
            &router,
            REGION,
            "GetMaintenanceWindow",
            &json!({ "WindowId": &window_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMaintenanceWindow {window_id:?} must succeed, got {status} {read}"
        );
        let (served, _) = executions(
            &router,
            case,
            REGION,
            &json!({ "WindowId": &window_id, "MaxResults": 4 }),
        )
        .await;
        let reported = execution_times(&served);
        assert_eq!(
            reported.first().map(String::as_str),
            read["NextExecutionTime"].as_str(),
            "case {case:?}: expected the first execution to be the window's NextExecutionTime \
             ({}), got {reported:?}",
            read["NextExecutionTime"]
        );
        // Successive executions of ONE window are written in the one zone and the one form,
        // so the text orders as the instants do: the series is strictly ascending.
        for pair in reported.windows(2) {
            assert!(
                pair[0] < pair[1],
                "case {case:?}: expected the series to ascend, got {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}

/// RULE 3: the windows asked about are the ones the request states — one `WindowId`, every
/// window of the endpoint when it names none (their executions merged in the order they are
/// scheduled), or the windows the `Targets` stated are registered with.
#[tokio::test]
async fn the_request_states_which_windows_are_asked_about() {
    let router = router();
    let case = "the selection table";
    let weekly = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "selected-weekly",
            "Schedule": "cron(0 8 ? * MON *)",
            "StartDate": "2035-01-01T00:00:00Z",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;
    let once = create_ok(
        &router,
        case,
        REGION,
        &minimal("selected-once", "at(2035-01-05T06:00:00)"),
    )
    .await;

    // A request naming no window asks about every window the endpoint holds, and the
    // executions of two windows interleave in the order they are scheduled.
    let (merged, _) = executions(&router, "every window", REGION, &json!({ "MaxResults": 3 })).await;
    assert_eq!(
        reported_pairs(&merged),
        vec![
            (weekly.clone(), "2035-01-01T08:00Z".to_string()),
            (once.clone(), "2035-01-05T06:00Z".to_string()),
            (weekly.clone(), "2035-01-08T08:00Z".to_string()),
        ],
        "case \"every window\": expected the two windows' executions merged in schedule order, \
         got {merged:?}"
    );

    // A request naming one window asks about that window alone.
    let (one, _) = executions(
        &router,
        "one window",
        REGION,
        &json!({ "WindowId": &once, "MaxResults": 3 }),
    )
    .await;
    assert_eq!(
        reported_pairs(&one),
        vec![(once.clone(), "2035-01-05T06:00Z".to_string())],
        "case \"one window\": expected only the window named, got {one:?}"
    );

    // An id this endpoint holds no window under names no window at all.
    let (status, response) = schedule(&router, REGION, &json!({ "WindowId": UNKNOWN_WINDOW_ID })).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "DoesNotExistException"),
        "case \"an id no create minted\": expected a refusal, got {status} {response}"
    );
    assert!(
        error_message(&response).contains(UNKNOWN_WINDOW_ID),
        "case \"an id no create minted\": expected the refusal to name the window asked for, got \
         {response}"
    );
    // The same id names no window at the other Region's endpoint either, and that endpoint
    // holds no window of its own to report.
    let (status, response) = schedule(&router, OTHER_REGION, &json!({ "WindowId": &weekly })).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "DoesNotExistException"),
        "case \"a window of another Region\": expected a refusal, got {status} {response}"
    );
    let (elsewhere, _) = executions(&router, "another Region", OTHER_REGION, &json!({})).await;
    assert!(
        elsewhere.is_empty(),
        "case \"another Region\": expected no execution, got {elsewhere:?}"
    );
}

/// RULE 3 (targets): a request stating targets asks about the windows those targets are
/// registered with, narrowed by the resource type the registration was made for.
#[tokio::test]
async fn targets_ask_about_the_windows_they_are_registered_with() {
    let router = router();
    let case = "the target table";
    let by_node = create_ok(
        &router,
        case,
        REGION,
        &minimal("target-by-node", "at(2035-01-05T06:00:00)"),
    )
    .await;
    let by_tag = create_ok(
        &router,
        case,
        REGION,
        &minimal("target-by-tag", "at(2035-01-06T06:00:00)"),
    )
    .await;
    for (window_id, targets) in [
        (&by_node, json!([{ "Key": "InstanceIds", "Values": [A_NODE] }])),
        (
            &by_tag,
            json!([{ "Key": "tag:Environment", "Values": ["production"] }]),
        ),
    ] {
        let (status, response) = call_in_region(
            &router,
            REGION,
            "RegisterTargetWithMaintenanceWindow",
            &json!({
                "WindowId": window_id,
                "ResourceType": "INSTANCE",
                "Targets": targets,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the register for {window_id:?} must succeed, got {status} {response}"
        );
    }

    // (case, the request, the windows whose executions it asks about)
    let rows: Vec<(&str, Value, Vec<&String>)> = vec![
        (
            "the node one window is registered against",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [{ "Key": "InstanceIds", "Values": [A_NODE] }],
            }),
            vec![&by_node],
        ),
        (
            "the tag the other window is registered against",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [{ "Key": "tag:Environment", "Values": ["production"] }],
            }),
            vec![&by_tag],
        ),
        (
            "both criteria at once, which union",
            json!({
                "Targets": [
                    { "Key": "InstanceIds", "Values": [A_NODE] },
                    { "Key": "tag:Environment", "Values": ["production"] },
                ],
            }),
            vec![&by_node, &by_tag],
        ),
        (
            "a node no window is registered against",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-00000000000000000"] }],
            }),
            vec![],
        ),
        (
            "a key no registration states",
            json!({
                "Targets": [{ "Key": "tag:Team", "Values": [A_NODE] }],
            }),
            vec![],
        ),
        (
            "the registered node, under the other resource type",
            json!({
                "ResourceType": "RESOURCE_GROUP",
                "Targets": [{ "Key": "InstanceIds", "Values": [A_NODE] }],
            }),
            vec![],
        ),
    ];
    for (why, body, expected) in rows {
        let (served, _) = executions(&router, why, REGION, &body).await;
        let reported: Vec<String> = reported_pairs(&served)
            .into_iter()
            .map(|(window_id, _)| window_id)
            .collect();
        let expected: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {why:?}: expected {expected:?}, got {served:?}"
        );
    }
}

/// RULE 4: a page holds at most `MaxResults` executions and the `NextToken` resumes the walk
/// at the execution the page stopped at, so a walk over the pages reports each execution once.
#[tokio::test]
async fn a_page_resumes_at_the_execution_it_stopped_at() {
    let router = router();
    let case = "the paging case";
    let _weekly = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "paged-window",
            "Schedule": "cron(0 8 ? * MON *)",
            "StartDate": "2035-01-01T00:00:00Z",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;
    // A second window, so the pages walk a MERGED series rather than one window's own.
    let other = create_ok(
        &router,
        case,
        REGION,
        &minimal("paged-once", "at(2035-01-05T06:00:00)"),
    )
    .await;

    let (whole, _) = executions(&router, case, REGION, &json!({ "MaxResults": 6 })).await;
    let whole = reported_pairs(&whole);
    assert_eq!(
        whole.len(),
        6,
        "case {case:?}: expected the page asked for to be filled, got {whole:?}"
    );

    for page_size in [1, 2, 3] {
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        let why = format!("pages of {page_size}");
        // The merged series runs on for as long as the weekly window does, so the walk stops
        // once it has covered the executions one whole page reported.
        while walked.len() < whole.len() {
            let mut body = json!({ "MaxResults": page_size });
            if let Some(next) = &token {
                body["NextToken"] = Value::String(next.clone());
            }
            let (served, next) = executions(&router, &why, REGION, &body).await;
            assert!(
                served.len() <= page_size as usize,
                "case {why:?}: a page holds at most MaxResults executions, got {}",
                served.len()
            );
            walked.extend(reported_pairs(&served));
            token = next;
            assert!(
                token.is_some(),
                "case {why:?}: a schedule that runs on has an execution past every page"
            );
        }
        walked.truncate(whole.len());
        assert_eq!(
            walked, whole,
            "case {why:?}: expected the walk to report each execution once, in schedule order"
        );
    }

    // A single execution and nothing waiting past it: the one-time window alone.
    let (last_page, token) = executions(
        &router,
        case,
        REGION,
        &json!({ "WindowId": &other, "MaxResults": 1 }),
    )
    .await;
    assert_eq!(
        (last_page.len(), token),
        (1, None),
        "case {case:?}: expected a one-time window to fill one page and offer no token"
    );

    // A token this read did not hand out is not a page of it, and a page larger than this
    // report serves is refused rather than served unbounded.
    for (why, token) in [
        ("a token this service did not mint", "not-a-token"),
        ("a position that is no instant", "later/mw-00000000000000000"),
        ("a position that names no window", "1234567890/not-a-window"),
    ] {
        let (status, response) = schedule(&router, REGION, &json!({ "NextToken": token })).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {why:?}: expected a refusal, got {status} {response}"
        );
    }
    let (status, response) = schedule(&router, REGION, &json!({ "MaxResults": 101 })).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "ValidationException"),
        "case \"a page larger than this report serves\": expected a refusal, got {status} \
         {response}"
    );
    assert!(
        error_message(&response).contains("101"),
        "case \"a page larger than this report serves\": expected the refusal to name the page \
         asked for, got {response}"
    );
}

/// RULE 5 (a bound is a bound on the instant the window OPENS at): for a window that waits
/// whole days after each instant its expression names, a filter and a resumed page are taken
/// against the instant it RUNS at, not against the instant the expression fired.
#[tokio::test]
async fn a_bound_is_taken_against_the_instant_the_window_opens_at() {
    let router = router();
    let case = "a window that waits whole days";
    let window_id = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "waiting-window",
            "Schedule": "cron(30 23 ? * TUE *)",
            "ScheduleOffset": 2,
            "StartDate": "2035-01-01T00:00:00Z",
            "Duration": 6,
            "Cutoff": 2,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;

    // The Tuesdays of January 2035 are the 2nd, 9th, 16th and 23rd, so the window opens on the
    // 4th, 11th, 18th and 25th — two whole days after each.
    // (case, the request, the execution times reported)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "after an instant between a named instant and its opening",
            json!({ "Key": "After", "Values": ["2035-01-03T00:00:00Z"] }),
            vec!["2035-01-04T23:30Z", "2035-01-11T23:30Z"],
        ),
        (
            "before an instant between a named instant and its opening",
            json!({ "Key": "Before", "Values": ["2035-01-17T00:00:00Z"] }),
            vec!["2035-01-04T23:30Z", "2035-01-11T23:30Z"],
        ),
        (
            "after an instant standing exactly on an opening",
            json!({ "Key": "After", "Values": ["2035-01-04T23:30:00Z"] }),
            vec!["2035-01-11T23:30Z", "2035-01-18T23:30Z"],
        ),
        (
            "before an instant standing exactly on an opening",
            json!({ "Key": "Before", "Values": ["2035-01-11T23:30:00Z"] }),
            vec!["2035-01-04T23:30Z"],
        ),
    ];
    for (why, filter, expected) in rows {
        let (served, _) = executions(
            &router,
            why,
            REGION,
            &json!({ "WindowId": &window_id, "MaxResults": 2, "Filters": [filter] }),
        )
        .await;
        assert_eq!(
            execution_times(&served),
            expected
                .iter()
                .map(|time| (*time).to_string())
                .collect::<Vec<String>>(),
            "case {why:?}: expected {expected:?}, got {served:?}"
        );
    }

    // A page of this window resumes at its next OPENING.
    let (first, token) = executions(
        &router,
        case,
        REGION,
        &json!({ "WindowId": &window_id, "MaxResults": 1 }),
    )
    .await;
    assert_eq!(
        execution_times(&first),
        vec!["2035-01-04T23:30Z".to_string()],
        "case {case:?}: expected the first opening, got {first:?}"
    );
    let token = token
        .unwrap_or_else(|| panic!("case {case:?}: expected a token past the first page"));
    let (second, _) = executions(
        &router,
        case,
        REGION,
        &json!({ "WindowId": &window_id, "MaxResults": 1, "NextToken": token }),
    )
    .await;
    assert_eq!(
        execution_times(&second),
        vec!["2035-01-11T23:30Z".to_string()],
        "case {case:?}: expected the page to resume at the next opening, got {second:?}"
    );
}

/// RULE 5: the filters bound the range of instants reported — the executions scheduled after
/// an `After` instant and before a `Before` one — each filter's values unioning while the
/// filters intersect, and a key outside that set is refused before anything is read.
#[tokio::test]
async fn the_filters_bound_the_range_of_instants_reported() {
    let router = router();
    let case = "the filter table";
    let window_id = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "filtered-window",
            "Schedule": "cron(0 8 ? * MON *)",
            "StartDate": "2035-01-01T00:00:00Z",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;

    // (case, the filters, the execution times reported for a page of five)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "no filter at all",
            json!([]),
            vec![
                "2035-01-01T08:00Z",
                "2035-01-08T08:00Z",
                "2035-01-15T08:00Z",
                "2035-01-22T08:00Z",
                "2035-01-29T08:00Z",
            ],
        ),
        (
            "after an instant inside the series",
            json!([{ "Key": "After", "Values": ["2035-01-08T12:00:00Z"] }]),
            vec![
                "2035-01-15T08:00Z",
                "2035-01-22T08:00Z",
                "2035-01-29T08:00Z",
                "2035-02-05T08:00Z",
                "2035-02-12T08:00Z",
            ],
        ),
        (
            "before an instant inside the series",
            json!([{ "Key": "Before", "Values": ["2035-01-20T00:00:00Z"] }]),
            vec![
                "2035-01-01T08:00Z",
                "2035-01-08T08:00Z",
                "2035-01-15T08:00Z",
            ],
        ),
        (
            "before an instant written only to the minute, as this service writes one",
            json!([{ "Key": "Before", "Values": ["2035-01-09T00:00Z"] }]),
            vec!["2035-01-01T08:00Z", "2035-01-08T08:00Z"],
        ),
        (
            "both ends, which intersect",
            json!([
                { "Key": "After", "Values": ["2035-01-02T00:00:00Z"] },
                { "Key": "Before", "Values": ["2035-01-20T00:00:00Z"] },
            ]),
            vec!["2035-01-08T08:00Z", "2035-01-15T08:00Z"],
        ),
        (
            "two instants to be before, whose values union on the later one",
            json!([{
                "Key": "Before",
                "Values": ["2035-01-03T00:00:00Z", "2035-01-20T00:00:00Z"],
            }]),
            vec![
                "2035-01-01T08:00Z",
                "2035-01-08T08:00Z",
                "2035-01-15T08:00Z",
            ],
        ),
        (
            "two instants to be after, whose values union on the earlier one",
            json!([{
                "Key": "After",
                "Values": ["2035-01-02T00:00:00Z", "2035-01-20T00:00:00Z"],
            }]),
            vec![
                "2035-01-08T08:00Z",
                "2035-01-15T08:00Z",
                "2035-01-22T08:00Z",
                "2035-01-29T08:00Z",
                "2035-02-05T08:00Z",
            ],
        ),
        (
            "after an instant standing exactly on an execution, which the bound excludes",
            json!([{ "Key": "After", "Values": ["2035-01-08T08:00:00Z"] }]),
            vec![
                "2035-01-15T08:00Z",
                "2035-01-22T08:00Z",
                "2035-01-29T08:00Z",
                "2035-02-05T08:00Z",
                "2035-02-12T08:00Z",
            ],
        ),
        (
            "before an instant standing exactly on an execution, which it excludes too",
            json!([{ "Key": "Before", "Values": ["2035-01-15T08:00:00Z"] }]),
            vec!["2035-01-01T08:00Z", "2035-01-08T08:00Z"],
        ),
        (
            "before the window is even active",
            json!([{ "Key": "Before", "Values": ["2034-01-01T00:00:00Z"] }]),
            vec![],
        ),
        (
            "a value naming no instant, which admits none",
            json!([{ "Key": "Before", "Values": ["whenever"] }]),
            vec![],
        ),
        (
            "a filter carrying no value, which admits none either",
            json!([{ "Key": "After", "Values": [] }]),
            vec![],
        ),
    ];
    for (why, filters, expected) in rows {
        let (served, _) = executions(
            &router,
            why,
            REGION,
            &json!({ "WindowId": &window_id, "MaxResults": 5, "Filters": filters }),
        )
        .await;
        assert_eq!(
            execution_times(&served),
            expected
                .iter()
                .map(|time| (*time).to_string())
                .collect::<Vec<String>>(),
            "case {why:?}: expected {expected:?}, got {served:?}"
        );
    }

    // A key this report does not narrow by names no bound on an execution; the keys are
    // case-sensitive, so a documented key miscased is not one either.
    for (why, key) in [
        ("a filter key the operation does not take", "ExecutedBefore"),
        ("a documented key, miscased", "before"),
    ] {
        let (status, response) = schedule(
            &router,
            REGION,
            &json!({
                "WindowId": &window_id,
                "Filters": [{ "Key": key, "Values": ["2035-01-01T00:00:00Z"] }],
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {why:?}: expected a refusal, got {status} {response}"
        );
        assert!(
            error_message(&response).contains(key),
            "case {why:?}: expected the refusal to name the key submitted, got {response}"
        );
    }
}
