// SPDX-License-Identifier: CC0-1.0

//! What `UpdateMaintenanceWindow` persists is what every LATER READER of that window answers
//! from — including the readers that do not report the stored members themselves but compute
//! a series from them: `DescribeMaintenanceWindowSchedule`'s upcoming executions, the
//! `NextExecutionTime` a single read reports and the one a listing reports.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a report that
//! is not computed from what the update persisted cannot pass: every series is read twice,
//! once as the create left the window and once as the update left it.
//!
//! The rule under test, asserted over a TABLE whose rows differ along which member of the
//! window the update writes — the expression, the time of day it names, the day of the week,
//! the zone it is read in, the whole days it waits, the instants the window is active over,
//! the form the schedule takes and whether the window is enabled at all:
//!
//! **A window's upcoming executions are read from the schedule, the zone, the wait and the
//! activation interval the window CURRENTLY holds.** An update writes those members, so the
//! series every reader computes after an update is the one the updated window names and no
//! longer the one the create named, and the first of that series is the `NextExecutionTime`
//! both a single read and a listing report.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, router, REGION};

/// `CreateMaintenanceWindow` at the Region's endpoint with `body`, asserting it succeeded and
/// answering the `WindowId` it minted.
async fn create(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, response) = call_in_region(router, REGION, "CreateMaintenanceWindow", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMaintenanceWindow {body} must succeed, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// `UpdateMaintenanceWindow` for `window_id` carrying `body`'s members, asserting it was
/// served, and answering what it reported.
async fn update(router: &MultiServiceRouter, case: &str, window_id: &str, body: &Value) -> Value {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    let (status, response) =
        call_in_region(router, REGION, "UpdateMaintenanceWindow", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update {request} must be served, got {status} {response}"
    );
    response
}

/// The `ExecutionTime`s `DescribeMaintenanceWindowSchedule` reports for `window_id`, in the
/// order it states them, asserting each execution names the window it belongs to.
async fn series(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    max_results: i32,
) -> Vec<String> {
    let body = json!({ "WindowId": window_id, "MaxResults": max_results });
    let (status, response) =
        call_in_region(router, REGION, "DescribeMaintenanceWindowSchedule", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMaintenanceWindowSchedule {body} must be served, got {status} \
         {response}"
    );
    let served = response["ScheduledWindowExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    served
        .iter()
        .map(|execution| {
            assert_eq!(
                execution["WindowId"].as_str(),
                Some(window_id),
                "case {case:?}: expected each execution to name the window asked about, got \
                 {execution}"
            );
            execution["ExecutionTime"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// What `GetMaintenanceWindow` reports for `window_id`.
async fn read(router: &MultiServiceRouter, case: &str, window_id: &str) -> Value {
    let (status, response) = call_in_region(
        router,
        REGION,
        "GetMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMaintenanceWindow {window_id:?} must succeed, got {status} {response}"
    );
    response
}

/// The `MaintenanceWindowIdentity` `DescribeMaintenanceWindows` reports for `window_id`.
async fn identity(router: &MultiServiceRouter, case: &str, window_id: &str) -> Value {
    let (status, response) =
        call_in_region(router, REGION, "DescribeMaintenanceWindows", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMaintenanceWindows must be served, got {status} {response}"
    );
    response["WindowIdentities"]
        .as_array()
        .expect("a listing reports WindowIdentities")
        .iter()
        .find(|identity| identity["WindowId"].as_str() == Some(window_id))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {window_id:?} among the windows, got {response}")
        })
}

/// Assert that the next instant `window_id` is reported to open at, by a single read and by a
/// listing alike, is `expected`.
async fn assert_next_execution(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    expected: Option<&str>,
) {
    let held = read(router, case, window_id).await;
    assert_eq!(
        held["NextExecutionTime"].as_str(),
        expected,
        "case {case:?}: expected the read to report NextExecutionTime {expected:?}, got {held}"
    );
    let listed = identity(router, case, window_id).await;
    assert_eq!(
        listed["NextExecutionTime"].as_str(),
        expected,
        "case {case:?}: expected the listing to report NextExecutionTime {expected:?}, got \
         {listed}"
    );
}

/// Assert that `window_id` is reported by a listing to carry every member `expected` names,
/// and nothing for a member `expected` states as null.
async fn assert_listed_members(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    expected: &Value,
) {
    let listed = identity(router, case, window_id).await;
    for (member, value) in expected.as_object().expect("the expected members") {
        if value.is_null() {
            assert!(
                listed.get(member.as_str()).is_none_or(Value::is_null),
                "case {case:?}: expected no {member} to be carried, got {listed}"
            );
        } else {
            assert_eq!(
                &listed[member.as_str()], value,
                "case {case:?}: expected {member} {value}, got {listed}"
            );
        }
    }
}

/// A create body whose window opens weekly, well in the future, so the instants its schedule
/// names are the same however fast the host runs and whenever the suite is run.
fn weekly(name: &str) -> Value {
    json!({
        "Name": name,
        "Schedule": "cron(0 8 ? * MON *)",
        "StartDate": "2035-01-01T00:00:00Z",
        "Duration": 2,
        "Cutoff": 1,
        "AllowUnassociatedTargets": false,
    })
}

/// The first instants the window [`weekly`] mints opens at: 2035-01-01 is a Monday, so the
/// series starts the day the window becomes active.
fn weekly_series() -> Vec<String> {
    ["2035-01-01T08:00Z", "2035-01-08T08:00Z", "2035-01-15T08:00Z"]
        .iter()
        .map(|instant| (*instant).to_string())
        .collect()
}

/// THE RULE: the executions a window is reported to be scheduled for are read from the
/// schedule, the zone, the wait and the activation interval it CURRENTLY holds — so an update
/// of any of them moves the series every reader computes, and the first of the moved series is
/// the `NextExecutionTime` a read and a listing report.
#[tokio::test]
async fn an_update_moves_the_series_every_reader_computes() {
    let router = router();

    // (case, the update's members, the executions reported afterwards)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "a daily expression in place of the weekly one",
            json!({ "Schedule": "cron(0 8 ? * * *)" }),
            vec![
                "2035-01-01T08:00Z",
                "2035-01-02T08:00Z",
                "2035-01-03T08:00Z",
            ],
        ),
        (
            // The Tuesdays of January 2035 are the 2nd, the 9th and the 16th.
            "another day of the week",
            json!({ "Schedule": "cron(0 8 ? * TUE *)" }),
            vec![
                "2035-01-02T08:00Z",
                "2035-01-09T08:00Z",
                "2035-01-16T08:00Z",
            ],
        ),
        (
            "another time of day",
            json!({ "Schedule": "cron(45 17 ? * MON *)" }),
            vec![
                "2035-01-01T17:45Z",
                "2035-01-08T17:45Z",
                "2035-01-15T17:45Z",
            ],
        ),
        (
            // The same expression read in a zone behind UTC: each instant it names stands
            // eight hours later than it did, and is reported in that zone.
            "the same expression read in another zone",
            json!({ "ScheduleTimezone": "America/Los_Angeles" }),
            vec![
                "2035-01-01T08:00-08:00",
                "2035-01-08T08:00-08:00",
                "2035-01-15T08:00-08:00",
            ],
        ),
        (
            // "The number of days to wait after the date and time specified by a cron
            // expression before running the maintenance window."
            "whole days waited after each instant the expression names",
            json!({ "ScheduleOffset": 3 }),
            vec![
                "2035-01-04T08:00Z",
                "2035-01-11T08:00Z",
                "2035-01-18T08:00Z",
            ],
        ),
        (
            "an inactivation before the third of the instants",
            json!({ "EndDate": "2035-01-10T00:00:00Z" }),
            vec!["2035-01-01T08:00Z", "2035-01-08T08:00Z"],
        ),
        (
            // Mondays from the 1st of February 2035 on: the 5th, the 12th and the 19th.
            "an activation delayed into another month",
            json!({ "StartDate": "2035-02-01T00:00:00Z" }),
            vec![
                "2035-02-05T08:00Z",
                "2035-02-12T08:00Z",
                "2035-02-19T08:00Z",
            ],
        ),
        (
            "a one-time instant in place of the recurring series",
            json!({ "Schedule": "at(2035-06-01T12:00:00)" }),
            vec!["2035-06-01T12:00Z"],
        ),
    ];

    for (why, body, after) in rows {
        let window_id = create(&router, why, &weekly("updated-series")).await;

        // As the create left the window, which every row below moves away from.
        assert_eq!(
            series(&router, why, &window_id, 3).await,
            weekly_series(),
            "case {why:?}: expected the created window's own series before the update"
        );

        let reported = update(&router, why, &window_id, &body).await;
        assert_eq!(
            reported["WindowId"].as_str(),
            Some(window_id.as_str()),
            "case {why:?}: expected the update to report the window it was asked about, got \
             {reported}"
        );

        let expected: Vec<String> = after.iter().map(|at| (*at).to_string()).collect();
        assert_eq!(
            series(&router, why, &window_id, 3).await,
            expected,
            "case {why:?}: expected the updated window's own series"
        );

        // The first of that series is what a single read and a listing report as the next
        // instant the window will open at.
        assert_next_execution(&router, why, &window_id, Some(expected[0].as_str())).await;

        // And the members the series was computed from are the ones the update wrote.
        assert_listed_members(&router, why, &window_id, &body).await;
    }
}

/// THE RULE, where what the update writes is whether the window runs at all: "whether the
/// maintenance window is enabled" — a disabled window is scheduled to open at nothing, and
/// enabling it again answers the series its stored schedule names.
#[tokio::test]
async fn a_disabled_window_is_scheduled_for_nothing_until_it_is_enabled_again() {
    let router = router();
    let case = "the window disabled and enabled again";
    let window_id = create(&router, case, &weekly("enablement-series")).await;
    assert_eq!(
        series(&router, case, &window_id, 3).await,
        weekly_series(),
        "case {case:?}: expected the created window's own series"
    );

    update(&router, case, &window_id, &json!({ "Enabled": false })).await;
    assert_eq!(
        series(&router, case, &window_id, 3).await,
        Vec::<String>::new(),
        "case {case:?}: expected a disabled window to be scheduled for nothing"
    );
    assert_next_execution(&router, case, &window_id, None).await;

    update(&router, case, &window_id, &json!({ "Enabled": true })).await;
    assert_eq!(
        series(&router, case, &window_id, 3).await,
        weekly_series(),
        "case {case:?}: expected the window enabled again to answer its stored schedule"
    );
    assert_next_execution(&router, case, &window_id, Some("2035-01-01T08:00Z")).await;
}

/// THE RULE, where the update CLEARS what the series was read with: under `Replace` "optional
/// fields that aren't specified are set to null", so a replacement that states no zone and no
/// wait leaves a window whose series is the one its expression names read at UTC.
#[tokio::test]
async fn a_replacing_update_clears_what_the_series_was_read_with() {
    let router = router();
    let case = "a replacement stating no zone and no wait";

    // A window whose series is read in a zone behind UTC and waits whole days on each instant
    // its expression names: the Mondays of January 2035 at 08:00 Los Angeles, waited three
    // days on.
    let mut created = weekly("replaced-series");
    created["ScheduleTimezone"] = json!("America/Los_Angeles");
    created["ScheduleOffset"] = json!(3);
    let window_id = create(&router, case, &created).await;
    assert_eq!(
        series(&router, case, &window_id, 3).await,
        vec![
            "2035-01-04T08:00-08:00".to_string(),
            "2035-01-11T08:00-08:00".to_string(),
            "2035-01-18T08:00-08:00".to_string(),
        ],
        "case {case:?}: expected the created window's own series"
    );

    update(
        &router,
        case,
        &window_id,
        &json!({
            "Replace": true,
            "Name": "replaced-series",
            "Schedule": "cron(0 8 ? * MON *)",
            "StartDate": "2035-01-01T00:00:00Z",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;

    assert_eq!(
        series(&router, case, &window_id, 3).await,
        weekly_series(),
        "case {case:?}: expected the replaced window's series to be read at UTC with no wait"
    );
    assert_next_execution(&router, case, &window_id, Some("2035-01-01T08:00Z")).await;
    assert_listed_members(
        &router,
        case,
        &window_id,
        &json!({
            "Schedule": "cron(0 8 ? * MON *)",
            "StartDate": "2035-01-01T00:00:00Z",
            "ScheduleTimezone": null,
            "ScheduleOffset": null,
        }),
    )
    .await;
}
