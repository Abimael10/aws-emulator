// SPDX-License-Identifier: CC0-1.0

//! `DescribeMaintenanceWindows` end to end: the windows "in an Amazon Web Services
//! account" are the ones this endpoint's creates persisted.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every listing follows writes.
//!
//! The rules under test are that the listing reports, for every window this endpoint
//! holds, exactly what a single read of that window reports (the identity is a projection
//! of the stored record, not of the request that asked for it), that it narrows by the
//! documented filter keys — which intersect while each filter's values union — that a page
//! holds at most `MaxResults` windows with a `NextToken` that resumes where the page
//! stopped, and that a window belongs to the endpoint of ONE Region and leaves the listing
//! when it is deleted. The suites are tables whose rows differ along which optional members
//! a create supplies, the form of the schedule, the filter key and the values under it,
//! the number of windows held, and the Region the endpoint is.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a window is shown to be listed at the endpoint of ONE Region.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// The members a `MaintenanceWindowIdentity` shares with a `GetMaintenanceWindow` read of
/// the same window, from `smithy_query(query="shape",shape="MaintenanceWindowIdentity")`.
/// A listing projects the stored record, so each of these must read the same either way.
const SHARED_MEMBERS: &[&str] = &[
    "WindowId",
    "Name",
    "Description",
    "Enabled",
    "Duration",
    "Cutoff",
    "Schedule",
    "ScheduleTimezone",
    "ScheduleOffset",
    "StartDate",
    "EndDate",
    "NextExecutionTime",
];

/// `CreateMaintenanceWindow` at `region`'s endpoint, asserting it succeeded and answering
/// the `WindowId` it minted.
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

/// A create body carrying only the members the model makes required.
fn minimal(name: &str, schedule: &str) -> Value {
    json!({
        "Name": name,
        "Schedule": schedule,
        "Duration": 2,
        "Cutoff": 1,
        "AllowUnassociatedTargets": false,
    })
}

/// `DescribeMaintenanceWindows` at `region`'s endpoint with `body`.
async fn describe(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "DescribeMaintenanceWindows", body).await
}

/// The identities the listing reports at `region`'s endpoint for `body`, asserting it was
/// served.
async fn identities(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> Vec<Value> {
    let (status, response) = describe(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMaintenanceWindows {body} must be served, got {status} {response}"
    );
    response["WindowIdentities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `WindowId`s the listing reports, in the order it reports them.
fn reported_ids(identities: &[Value]) -> Vec<String> {
    identities
        .iter()
        .map(|identity| {
            identity["WindowId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// RULE 1: the listing reports every window this endpoint holds, each projected from the
/// stored record — so every member it shares with a single read of that window reads the
/// same either way. The rows differ along which optional members the create supplies and
/// along the form of the schedule.
#[tokio::test]
async fn the_listing_projects_the_window_each_create_persisted() {
    let router = router();
    // (case, the create body). Every schedule names instants a week or more apart, so the
    // instant reported does not depend on how fast the host runs.
    let rows: [(&str, Value); 5] = [
        (
            "only the required members",
            minimal("only-required", "cron(0 8 ? * MON *)"),
        ),
        (
            "a description and a zone",
            json!({
                "Name": "described-window",
                "Description": "Patch the fleet",
                "Schedule": "cron(0 16 ? * TUE *)",
                "ScheduleTimezone": "America/Los_Angeles",
                "Duration": 4,
                "Cutoff": 0,
                "AllowUnassociatedTargets": true,
            }),
        ),
        (
            "a rate expression and an activation window",
            json!({
                "Name": "rate-window",
                "Schedule": "rate(7 days)",
                "StartDate": "2035-01-01T00:00:00Z",
                "EndDate": "2035-12-31T23:59:59Z",
                "Duration": 24,
                "Cutoff": 23,
                "AllowUnassociatedTargets": false,
            }),
        ),
        (
            "a cron expression waiting whole days",
            json!({
                "Name": "offset-window",
                "Schedule": "cron(30 23 ? * TUE *)",
                "ScheduleOffset": 2,
                "ScheduleTimezone": "Asia/Seoul",
                "Duration": 6,
                "Cutoff": 2,
                "AllowUnassociatedTargets": false,
            }),
        ),
        (
            "a window that has already become inactive",
            json!({
                "Name": "expired-window",
                "Schedule": "cron(0 2 ? * SUN *)",
                "EndDate": "2001-01-01T00:00:00Z",
                "Duration": 1,
                "Cutoff": 0,
                "AllowUnassociatedTargets": false,
            }),
        ),
    ];

    let mut minted: Vec<(&str, String)> = Vec::new();
    for (case, body) in &rows {
        minted.push((case, create_ok(&router, case, REGION, body).await));
    }

    let listed = identities(&router, "the projection table", REGION, &json!({})).await;
    assert_eq!(
        reported_ids(&listed),
        minted
            .iter()
            .map(|(_, id)| id.clone())
            .collect::<Vec<String>>(),
        "case \"the projection table\": expected the listing to report every window created, \
         in the order they were created, got {listed:?}"
    );

    for (case, window_id) in &minted {
        let (status, read) = call_in_region(
            &router,
            REGION,
            "GetMaintenanceWindow",
            &json!({ "WindowId": window_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMaintenanceWindow {window_id:?} must succeed, got {status} {read}"
        );
        let identity = listed
            .iter()
            .find(|identity| identity["WindowId"].as_str() == Some(window_id.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected {window_id} in the listing, got {listed:?}")
            });
        for member in SHARED_MEMBERS {
            assert_eq!(
                identity[member], read[member],
                "case {case:?}: expected the listing's {member} to be the one the read \
                 reports ({}), got {}",
                read[member], identity[member]
            );
        }
    }

    // Nothing was created at the other Region's endpoint, so it lists nothing — and the
    // listing is a READ: running it leaves the windows it reported where they were.
    let elsewhere = identities(&router, "another Region's endpoint", OTHER_REGION, &json!({})).await;
    assert!(
        elsewhere.is_empty(),
        "case \"another Region's endpoint\": expected no window, got {elsewhere:?}"
    );
    let again = identities(&router, "the listing is a read", REGION, &json!({})).await;
    assert_eq!(
        reported_ids(&again),
        reported_ids(&listed),
        "case \"the listing is a read\": expected a second listing to report the same windows, \
         got {again:?}"
    );
}

/// RULE 2: the listing narrows by the documented filter keys (`Name` and `Enabled`, from
/// `DescribeMaintenanceWindowsRequest$Filters`), which intersect while each filter's values
/// union — and a key outside that set is refused before anything is read.
#[tokio::test]
async fn the_listing_narrows_by_the_documented_filter_keys() {
    let router = router();
    let case = "the filter table";
    let first = create_ok(
        &router,
        case,
        REGION,
        &minimal("fleet-patching", "cron(0 8 ? * MON *)"),
    )
    .await;
    let second = create_ok(
        &router,
        case,
        REGION,
        &minimal("database-patching", "rate(7 days)"),
    )
    .await;

    // (case, the filters, the window ids the listing must report, in order)
    let rows: Vec<(&str, Value, Vec<&String>)> = vec![
        (
            "one name",
            json!([{ "Key": "Name", "Values": ["fleet-patching"] }]),
            vec![&first],
        ),
        (
            "either name, whose values union",
            json!([{ "Key": "Name", "Values": ["database-patching", "fleet-patching"] }]),
            vec![&first, &second],
        ),
        (
            "a name no window carries",
            json!([{ "Key": "Name", "Values": ["no-such-window"] }]),
            vec![],
        ),
        (
            // "For example […] Enabled=True": a create mints an enabled window, so every
            // window held here is one.
            "enabled, as the documented example cases it",
            json!([{ "Key": "Enabled", "Values": ["True"] }]),
            vec![&first, &second],
        ),
        (
            "enabled, lowercased",
            json!([{ "Key": "Enabled", "Values": ["true"] }]),
            vec![&first, &second],
        ),
        (
            "not enabled, which no created window is",
            json!([{ "Key": "Enabled", "Values": ["false"] }]),
            vec![],
        ),
        (
            "a value spelling neither boolean",
            json!([{ "Key": "Enabled", "Values": ["maybe"] }]),
            vec![],
        ),
        (
            "a name and the enabled state, which intersect",
            json!([
                { "Key": "Name", "Values": ["database-patching"] },
                { "Key": "Enabled", "Values": ["true"] },
            ]),
            vec![&second],
        ),
        (
            "a name intersected with a state no window is in",
            json!([
                { "Key": "Name", "Values": ["database-patching"] },
                { "Key": "Enabled", "Values": ["false"] },
            ]),
            vec![],
        ),
    ];
    for (why, filters, expected) in rows {
        let reported = reported_ids(&identities(&router, why, REGION, &json!({ "Filters": filters })).await);
        let expected: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {why:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // A key the operation does not narrow by names no property of a window; the key is
    // case-sensitive, so the lowercased spelling of a documented key is not one either.
    for (why, key) in [
        ("a filter key the operation does not take", "NotAKey"),
        ("a documented key, miscased", "name"),
    ] {
        let (status, response) = describe(
            &router,
            REGION,
            &json!({ "Filters": [{ "Key": key, "Values": ["anything"] }] }),
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

/// RULE 3 (paging): a page holds at most `MaxResults` windows and the `NextToken` resumes
/// the listing at the window the page stopped at, so a walk over the pages reports each
/// window once.
#[tokio::test]
async fn a_page_resumes_at_the_window_it_stopped_at() {
    let router = router();
    let case = "the paging case";
    // The smallest page the modelled `MaintenanceWindowMaxResults` admits (range 10..100),
    // with one window more than that, so the walk takes two pages.
    let page_size = 10;
    let mut minted: Vec<String> = Vec::new();
    for index in 0..=page_size {
        minted.push(
            create_ok(
                &router,
                case,
                REGION,
                &minimal(&format!("paged-window-{index}"), "cron(0 8 ? * MON *)"),
            )
            .await,
        );
    }

    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "MaxResults": page_size });
        if let Some(next) = &token {
            body["NextToken"] = Value::String(next.clone());
        }
        let (status, response) = describe(&router, REGION, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: each page must be served, got {status} {response}"
        );
        let page = response["WindowIdentities"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            page.len() <= page_size as usize,
            "case {case:?}: a page holds at most MaxResults windows, got {}",
            page.len()
        );
        walked.extend(reported_ids(&page));
        match response["NextToken"].as_str() {
            Some(next) if !next.is_empty() => token = Some(next.to_string()),
            _ => break,
        }
    }
    assert_eq!(
        walked, minted,
        "case {case:?}: expected the walk to report every window once, in the order they were \
         created"
    );

    // A request naming no `MaxResults` is served every window this endpoint holds, up to the
    // page the operation allows.
    let whole = reported_ids(&identities(&router, case, REGION, &json!({})).await);
    assert_eq!(
        whole, minted,
        "case {case:?}: expected a request naming no MaxResults to report every window"
    );

    // A token this read did not hand out is not a page of it: one that is not shaped like a
    // window id, and one that is but names no window of this result.
    for (why, bad_token) in [
        ("a token this service did not mint", "not-a-token"),
        ("a window id no create minted", UNKNOWN_WINDOW_ID),
    ] {
        let (status, response) = describe(&router, REGION, &json!({ "NextToken": bad_token })).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {why:?}: expected a refusal, got {status} {response}"
        );
    }
}

/// RULE 4: a window is listed at the endpoint of the ONE Region it was created through, and
/// leaves that listing when it is deleted.
#[tokio::test]
async fn a_window_is_listed_at_the_endpoint_of_its_own_region() {
    let router = router();
    let case = "one window per Region";
    let here = create_ok(
        &router,
        case,
        REGION,
        &minimal("window-here", "cron(0 8 ? * MON *)"),
    )
    .await;
    let elsewhere = create_ok(
        &router,
        case,
        OTHER_REGION,
        &minimal("window-elsewhere", "cron(0 8 ? * MON *)"),
    )
    .await;

    for (why, region, expected) in [
        ("the Region the first window was created through", REGION, &here),
        (
            "the Region the second window was created through",
            OTHER_REGION,
            &elsewhere,
        ),
    ] {
        let reported = reported_ids(&identities(&router, why, region, &json!({})).await);
        assert_eq!(
            reported,
            vec![expected.clone()],
            "case {why:?}: expected only that Region's window, got {reported:?}"
        );
    }

    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeleteMaintenanceWindow",
        &json!({ "WindowId": here }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteMaintenanceWindow {here:?} must succeed, got {status} {response}"
    );
    let after = identities(&router, "after the delete", REGION, &json!({})).await;
    assert!(
        after.is_empty(),
        "case \"after the delete\": expected the deleted window to leave the listing, got \
         {after:?}"
    );
    let untouched = reported_ids(&identities(&router, "the other Region", OTHER_REGION, &json!({})).await);
    assert_eq!(
        untouched,
        vec![elsewhere.clone()],
        "case \"the other Region\": expected the delete to leave the other Region's window, got \
         {untouched:?}"
    );
}
