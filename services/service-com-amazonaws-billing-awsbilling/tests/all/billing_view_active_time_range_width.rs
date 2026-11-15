// SPDX-License-Identifier: CC0-1.0

//! The width bound the `ListBillingViews` `activeTimeRange` member documents,
//! end to end over the awsJson1_0 wire.
//!
//! The rule every assertion here is about: a request time range whose own
//! modelled doc states a maximum width is answered only while it stays inside
//! that width, measured on the UTC civil calendar, and a wider one is refused
//! with the modelled `ValidationException` (400) naming the range member. For
//! this member the documented width is one calendar month
//! (`smithy_query(query="doc", shape="ListBillingViewsRequest",
//! member="activeTimeRange")`: "The time range must be within one calendar
//! month."), so the bound sits 28, 29, 30 or 31 days after the range's start
//! according to the month it starts in — never a fixed span.
//!
//! The rows differ along what the rule generalises over: the width of the
//! window (zero, one second, most of a month, exactly the bound, one second
//! past it, a year, a decade), the length of the calendar month the window
//! starts in (31 days, a leap February, a common February), and the day of
//! month it starts on (the 1st, a day the following month does not have).
//! Every calendar date below is UTC, named in the comment beside it.
//!
//! The harness (`call`, `emulator_with_engine`, …) is the one `billing_views.rs`
//! owns.

use serde_json::{json, Value};

use crate::billing_view_active_time_range::listed_arns;
use crate::billing_views::{call, emulator_with_engine, error_code, PRIMARY_ARN};

/// A UTC calendar instant as seconds since the epoch, so each row below reads
/// as the date it is about.
fn secs(instant: &str) -> i64 {
    ::aws_smithy_types::DateTime::from_str(instant, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| panic!("{instant:?} is an RFC 3339 instant: {error}"))
        .secs()
}

/// One `activeTimeRange` request, as the wire carries it.
fn range_request(after: i64, before: i64) -> Value {
    json!({
        "activeTimeRange": { "activeAfterInclusive": after, "activeBeforeInclusive": before }
    })
}

#[tokio::test]
async fn a_window_within_one_calendar_month_is_answered() {
    let (router, _engine) = emulator_with_engine();

    // (case, activeAfterInclusive, activeBeforeInclusive)
    let accepted: Vec<(&str, i64, i64)> = vec![
        (
            "a zero-width window",
            secs("2024-01-01T00:00:00Z"),
            secs("2024-01-01T00:00:00Z"),
        ),
        (
            "a one-second window",
            secs("2024-01-01T00:00:00Z"),
            secs("2024-01-01T00:00:01Z"),
        ),
        (
            "27 days inside one calendar month",
            secs("2024-01-01T00:00:00Z"),
            secs("2024-01-28T00:00:00Z"),
        ),
        (
            "exactly one calendar month from the 1st of a 31-day month",
            secs("2024-01-01T00:00:00Z"),
            secs("2024-02-01T00:00:00Z"),
        ),
        (
            "exactly one calendar month from the 1st of a leap February (29 days)",
            secs("2024-02-01T00:00:00Z"),
            secs("2024-03-01T00:00:00Z"),
        ),
        (
            "exactly one calendar month from the 1st of a common February (28 days)",
            secs("2023-02-01T00:00:00Z"),
            secs("2023-03-01T00:00:00Z"),
        ),
        (
            // One calendar month after 31 January is the last day of February,
            // the nearest day that month has.
            "one calendar month from the 31st, clamped to the short month's last day",
            secs("2024-01-31T00:00:00Z"),
            secs("2024-02-29T00:00:00Z"),
        ),
        (
            "a window inside a common February, to its last second",
            secs("2023-02-02T12:00:00Z"),
            secs("2023-02-28T23:59:59Z"),
        ),
        (
            // The bound is about the window, not about where it sits: a window
            // whose start carries a time of day keeps the same width.
            "exactly one calendar month from a mid-month afternoon",
            secs("2026-07-15T14:30:00Z"),
            secs("2026-08-15T14:30:00Z"),
        ),
    ];

    for (case, after, before) in accepted {
        let (status, body) = call(&router, "ListBillingViews", &range_request(after, before)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ListBillingViews, got {status} {body:?}"
        );
        // The filter is still applied, not bypassed: the account's default view
        // is listed for every range it is asked about.
        let listed = listed_arns(&body);
        assert_eq!(
            listed,
            vec![PRIMARY_ARN.to_string()],
            "case {case:?}: expected the account's default view to be listed, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_window_wider_than_one_calendar_month_is_refused() {
    let (router, _engine) = emulator_with_engine();

    // (case, activeAfterInclusive, activeBeforeInclusive)
    let refused: Vec<(&str, i64, i64)> = vec![
        (
            "one second past one calendar month from the 1st of a 31-day month",
            secs("2024-01-01T00:00:00Z"),
            secs("2024-02-01T00:00:01Z"),
        ),
        (
            // 30 days is past the bound in a 28-day February, so the bound is
            // the calendar's, not a fixed span.
            "30 days from the 1st of a common February",
            secs("2023-02-01T00:00:00Z"),
            secs("2023-03-03T00:00:00Z"),
        ),
        (
            "one day past the clamped month end from the 31st",
            secs("2024-01-31T00:00:00Z"),
            secs("2024-03-01T00:00:00Z"),
        ),
        (
            "one second past one calendar month from a mid-month afternoon",
            secs("2026-07-15T14:30:00Z"),
            secs("2026-08-15T14:30:01Z"),
        ),
        (
            "a twelve-month window",
            secs("2024-01-01T00:00:00Z"),
            secs("2025-01-01T00:00:00Z"),
        ),
        (
            "a decade-wide window",
            secs("2024-01-01T00:00:00Z"),
            secs("2034-01-01T00:00:00Z"),
        ),
        (
            "a window from the epoch to the far future",
            0,
            secs("2096-10-02T00:00:00Z"),
        ),
    ];

    for (case, after, before) in refused {
        let (status, body) = call(&router, "ListBillingViews", &range_request(after, before)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400 refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("within one calendar month"),
            "case {case:?}: expected the refusal to state the documented bound, got {message:?}"
        );
        let field = body["fieldList"][0]["name"].as_str().unwrap_or_default();
        assert_eq!(
            field, "activeTimeRange",
            "case {case:?}: expected the refusal to name activeTimeRange, got {body:?}"
        );
    }

    // A refused read reports nothing and leaves the store as it was: the
    // unfiltered list still answers with the account's default view.
    let (status, listed) = call(&router, "ListBillingViews", &json!({})).await;
    let arns = listed_arns(&listed);
    assert_eq!(
        (status, arns.clone()),
        (200, vec![PRIMARY_ARN.to_string()]),
        "case \"after every refusal\": expected the default view still listed, got {status} {arns:?}"
    );
}

#[tokio::test]
async fn a_window_that_runs_backwards_is_refused() {
    let (router, _engine) = emulator_with_engine();

    // The other relation between the two bounds: the range runs forwards. Each
    // row is inside one calendar month, so only the ordering can refuse it.
    let backwards: Vec<(&str, i64, i64)> = vec![
        (
            "one second backwards",
            secs("2024-01-02T00:00:00Z"),
            secs("2024-01-01T23:59:59Z"),
        ),
        (
            "a day backwards",
            secs("2024-01-10T00:00:00Z"),
            secs("2024-01-09T00:00:00Z"),
        ),
        (
            "backwards across a month boundary",
            secs("2024-02-01T00:00:00Z"),
            secs("2024-01-20T00:00:00Z"),
        ),
    ];

    for (case, after, before) in backwards {
        let (status, body) = call(&router, "ListBillingViews", &range_request(after, before)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400 refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        let field = body["fieldList"][0]["name"].as_str().unwrap_or_default();
        assert_eq!(
            field, "activeTimeRange",
            "case {case:?}: expected the refusal to name activeTimeRange, got {body:?}"
        );
    }
}
