// SPDX-License-Identifier: CC0-1.0

//! What the VALUE half of a boundary constraint clause states, over the whole numeric
//! domain a member's declared `@range` refuses.
//!
//! The rule: a refusal of a numeric value outside its member's modelled `@range` states the
//! member it sat at and the bound it broke, and states no value — whatever the magnitude or
//! sign of the number submitted, nothing is quoted between `Value` and `at`, so the clause a
//! caller reads is about the bound rather than about the one number that missed it.
//!
//! This suite is the domain sweep behind the sibling table in
//! `tests/all/range_refusal_wording.rs`, whose helpers it states the clause with: that table
//! pins the clause (member path, bound, envelope) once per `@range` member and pins the
//! published bounds as still served. Here the member is held fixed and the SUBMITTED VALUE
//! varies — exhaustively through the integers either side of each bound, then out to the
//! extremes an `integer` member admits — at a top-level member of both listings and at a
//! member nested under a list element. The bound check widens a value to compare it, so an
//! extreme magnitude is where a rendering of it could reappear in the sentence (a thousands
//! separator, an exponent, a truncated or re-rounded number) while the neighbourhood of the
//! bound still looked right.
//!
//! Sources for the bounds: `smithy_query(query="closure",
//! operation="ListDashboards,ListScheduledReports,CreateDashboard")` — `MaxResults` range
//! 1..100 on both listings, `WidgetWidth` 2..6 and `WidgetHeight` 4..10 on a widget.
//!
//! Every call goes through the real router, so what these tests assert is what an SDK
//! client sees.

use serde_json::json;

use super::dashboard_lifecycle::{call, error_code, router};
use super::range_refusal_wording::{
    at_least, at_most, error_message, one_error, range_clause, widget, VALUE_HALF_OPENING,
    WELL_FORMED_DASHBOARD_ARN,
};

/// Every integer this sweep submits at a member whose range is `min..max`, paired with the
/// requirement it breaks: exhaustively the four integers below the minimum and the four
/// above the maximum, then the magnitudes an `integer` member reaches at either end.
fn outside(min: i64, max: i64) -> Vec<(i64, String)> {
    let mut cases: Vec<(i64, String)> = Vec::new();
    for step in 1..=4 {
        cases.push((min - step, at_least(min)));
        cases.push((max + step, at_most(max)));
    }
    for far in [
        i64::from(i32::MIN),
        -1_000_000,
        -1_000,
        1_000,
        1_000_000,
        i64::from(i32::MAX),
    ] {
        if far < min {
            cases.push((far, at_least(min)));
        } else if far > max {
            cases.push((far, at_most(max)));
        }
    }
    cases
}

#[tokio::test]
async fn a_page_size_outside_its_range_is_stated_without_the_number_submitted() {
    let router = router();

    // The same member on both listings: the value half is withheld for the class, not for
    // one operation's reading of it.
    for operation in ["ListDashboards", "ListScheduledReports"] {
        for (submitted, requirement) in outside(1, 100) {
            let (status, body) = call(&router, operation, &json!({"maxResults": submitted})).await;
            assert_eq!(
                status, 400,
                "case {operation} maxResults {submitted}: expected HTTP 400, got {status}: \
                 {body}"
            );
            assert!(
                error_code(&body).ends_with("ValidationException"),
                "case {operation} maxResults {submitted}: expected ValidationException, got \
                 {:?}: {body}",
                error_code(&body)
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                one_error(&range_clause("maxResults", &requirement)),
                "case {operation} maxResults {submitted}: expected the clause to state the \
                 member and the bound alone, got {message:?}"
            );
            assert!(
                !body.to_string().contains(VALUE_HALF_OPENING),
                "case {operation} maxResults {submitted}: the refusal must quote no value, \
                 got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_nested_member_outside_its_range_withholds_its_number_too() {
    let router = router();

    // The rule holds wherever a `@range` member sits: a widget's `width` (2..6) and
    // `height` (4..10) are nested under a list element, on a create and on an update.
    let members = [("width", 2_i64, 6_i64), ("height", 4, 10)];
    for (member, min, max) in members {
        for (submitted, requirement) in outside(min, max) {
            let requests = [
                (
                    "CreateDashboard",
                    json!({
                        "name": format!("Nested {member} {submitted}"),
                        "widgets": [widget("Spend", json!({member: submitted}))]
                    }),
                ),
                (
                    "UpdateDashboard",
                    json!({
                        "arn": WELL_FORMED_DASHBOARD_ARN,
                        "name": "Updated Dashboard",
                        "widgets": [widget("Spend", json!({member: submitted}))]
                    }),
                ),
            ];
            for (operation, request) in requests {
                let (status, body) = call(&router, operation, &request).await;
                assert_eq!(
                    status, 400,
                    "case {operation} {member} {submitted}: expected HTTP 400, got {status}: \
                     {body}"
                );
                let message = error_message(&body);
                assert_eq!(
                    message,
                    one_error(&range_clause(
                        &format!("widgets.1.member.{member}"),
                        &requirement
                    )),
                    "case {operation} {member} {submitted}: expected the clause to state the \
                     nested member and the bound alone, got {message:?}"
                );
                assert!(
                    !body.to_string().contains(VALUE_HALF_OPENING),
                    "case {operation} {member} {submitted}: the refusal must quote no value, \
                     got {body}"
                );
            }
        }
    }
}
