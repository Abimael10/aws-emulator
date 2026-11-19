// SPDX-License-Identifier: CC0-1.0

//! The granularity a `PutAlarmMuteRule` request may state its window boundaries at.
//!
//! ONE rule, asserted over a table: **a mute rule's windows open and close on the MINUTE, so a
//! `StartDate` or `ExpireDate` that names an instant inside a minute is refused — with the
//! sentence CloudWatch states that class in, naming the member that has to change — and nothing
//! is stored; a boundary that names a whole minute is accepted and reads back as the exact
//! instant submitted.**
//!
//! Both members are plain timestamps carrying no modelled constraint
//! (`smithy_query(query="operation",operation="PutAlarmMuteRule")`), so what makes a minute the
//! granularity is the schedule they bound: "Format: `at(yyyy-MM-ddThh:mm)`" for a one-time window
//! and "Format: `cron(Minutes Hours Day-of-month Month Day-of-week)`" for a recurring one
//! (`smithy_query(query="shape",shape="Schedule")`) both state their instants at the minute and no
//! finer, so a boundary stated to the second names an instant no window of the rule can reach.
//!
//! Rows differ along what the rule generalises over: which of the two members carries the finer
//! instant, how far off the minute it lies (a second either side, half a minute, a fraction of a
//! second), whether that instant is ahead of or behind the clock, and — on the accepting side —
//! which boundaries a request carries and how far apart they are, the whole minute itself being
//! the bound each refusal sits just off.

use crate::alarm_mute_rules::{minute_aligned, put_request, with_members};
use crate::harness::{build_router, call, error_code, error_message, ok};
use ::chrono::{TimeDelta, Utc};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A schedule whose windows recur, so no row's acceptance depends on a one-time expression (which
/// carries window boundaries of its own and is refused when given more).
const RECURRING: (&str, &str) = ("cron(0 2 * * *)", "PT2H");

/// How CloudWatch words the refusal of a boundary stated at a finer granularity than a mute rule's
/// windows: one sentence naming the member, carrying nothing of the instant submitted.
fn minute_precision(member: &str) -> String {
    format!(
        "The Alarm Mute Rule {member} only supports minute-level precision. Please provide the \
         time without seconds."
    )
}

/// Assert that `name` answers to no rule — what a refused write has to leave behind.
async fn assert_absent(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = call(
        router,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (404, "ResourceNotFoundException"),
        "case {case:?}: a refused write must store nothing, got {status} with {body}"
    );
}

#[tokio::test]
async fn a_boundary_finer_than_a_minute_is_refused_and_stores_nothing() {
    let router = build_router();
    let now = Utc::now();
    let in_a_day = minute_aligned(now + TimeDelta::days(1));
    let in_two_days = minute_aligned(now + TimeDelta::days(2));
    let a_day_ago = minute_aligned(now - TimeDelta::days(1));

    // (case, the request's members beside its name, the member the refusal names).
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a start date one second past the minute",
            json!({"StartDate": in_a_day + 1}),
            "StartDate",
        ),
        (
            "a start date one second short of the next minute",
            json!({"StartDate": in_a_day + 59}),
            "StartDate",
        ),
        (
            "a start date half a minute past the minute",
            json!({"StartDate": in_a_day + 30}),
            "StartDate",
        ),
        (
            "a start date carrying a fraction of a second",
            json!({"StartDate": in_a_day as f64 + 0.25}),
            "StartDate",
        ),
        (
            "a start date behind the clock, carrying seconds",
            json!({"StartDate": a_day_ago + 7}),
            "StartDate",
        ),
        (
            "an expire date one second past the minute, beside a start date on the minute",
            json!({"StartDate": in_a_day, "ExpireDate": in_two_days + 1}),
            "ExpireDate",
        ),
        (
            "an expire date carrying seconds, with no start date at all",
            json!({"ExpireDate": in_two_days + 45}),
            "ExpireDate",
        ),
        (
            "an expire date carrying a fraction of a second",
            json!({"StartDate": in_a_day, "ExpireDate": in_two_days as f64 + 0.5}),
            "ExpireDate",
        ),
        (
            // The boundary a window OPENS at is judged first, so a request whose boundaries are
            // both finer than a minute is told about that one.
            "both boundaries carrying seconds",
            json!({"StartDate": in_a_day + 13, "ExpireDate": in_two_days + 44}),
            "StartDate",
        ),
    ];

    for (index, (case, members, member)) in cases.into_iter().enumerate() {
        let name = format!("sub-minute-boundary-{index}");
        let request = with_members(
            put_request(&name, RECURRING.0, RECURRING.1),
            members,
        );
        let (status, body) = call(&router, "PutAlarmMuteRule", &request).await;
        // CloudWatch answers a precondition its model cannot express in the request-validation
        // class, which the awsJson protocols carry as the local name of
        // `com.amazon.coral.validate#ValidationException`.
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} with {body}"
        );
        let expected = minute_precision(member);
        let got = error_message(&body);
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?} in {body}"
        );
        assert_absent(&router, case, &name).await;
    }
}

#[tokio::test]
async fn a_boundary_on_the_minute_is_stored_as_the_instant_submitted() {
    let router = build_router();
    let now = Utc::now();
    let in_a_day = minute_aligned(now + TimeDelta::days(1));
    let in_two_days = minute_aligned(now + TimeDelta::days(2));
    let a_day_ago = minute_aligned(now - TimeDelta::days(1));

    // Each row sits on the other side of one of the refusals above: the whole minute itself, on
    // one member or both, ahead of the clock and behind it, a minute apart and days apart.
    let cases: Vec<(&str, Value)> = vec![
        (
            "a start date on the minute, alone",
            json!({"StartDate": in_a_day}),
        ),
        (
            "an expire date on the minute, alone",
            json!({"ExpireDate": in_two_days}),
        ),
        (
            "boundaries a minute apart, both on the minute",
            json!({"StartDate": in_a_day, "ExpireDate": in_a_day + 60}),
        ),
        (
            "boundaries a day apart, both on the minute",
            json!({"StartDate": in_a_day, "ExpireDate": in_two_days}),
        ),
        (
            "a start date behind the clock, on the minute",
            json!({"StartDate": a_day_ago, "ExpireDate": in_two_days}),
        ),
        (
            "the epoch minute itself",
            json!({"StartDate": 0, "ExpireDate": in_two_days}),
        ),
        (
            "boundaries carrying no members at all",
            json!({"Description": "a window the schedule alone bounds"}),
        ),
    ];

    for (index, (case, members)) in cases.into_iter().enumerate() {
        let name = format!("whole-minute-boundary-{index}");
        let request = with_members(
            put_request(&name, RECURRING.0, RECURRING.1),
            members,
        );
        let (status, body) = call(&router, "PutAlarmMuteRule", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let read = ok(
            &router,
            "GetAlarmMuteRule",
            &json!({"AlarmMuteRuleName": name}),
        )
        .await;
        for member in ["StartDate", "ExpireDate"] {
            assert_eq!(
                read.get(member).and_then(Value::as_f64),
                request.get(member).and_then(Value::as_f64),
                "case {case:?}: expected the exact {member} submitted ({:?}), got {read}",
                request.get(member)
            );
        }
    }
}
