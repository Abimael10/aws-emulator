// SPDX-License-Identifier: CC0-1.0

//! The rules a `PutAlarmMuteRule` request has to satisfy BETWEEN its members, and the sentences
//! CloudWatch refuses each class in.
//!
//! The rule under test: a mute window that cannot be run is refused at write time, in the
//! sentence of its class, and nothing is stored — so the name the refused write carried still
//! answers to no rule. Three classes have a sentence of their own, fixed text that interpolates
//! nothing from the request:
//!
//! * a window longer than the published maximum ("maximum of 15 days (`P15D`)",
//!   `Schedule$Duration`);
//! * a ONE-TIME schedule, which names the instant its single window opens inside the expression
//!   ("At expressions - For one-time mute windows", `Schedule$Expression`), given window
//!   boundaries of its own as well;
//! * a window whose `ExpireDate` — the instant the rule is "no longer evaluated"
//!   (`PutAlarmMuteRuleInput$ExpireDate`) — is no later than the `StartDate` it takes effect
//!   after (`PutAlarmMuteRuleInput$StartDate`), which is a rule already expired when written.
//!
//! Every table row is a distinct input along what the rule generalises over (how far past the
//! published maximum the window runs, which of the two boundary members a one-time schedule
//! carries, by how much the expiry precedes the start, and which expression form states the
//! schedule), and each refusal row is paired with the acceptance that proves the rule refuses
//! only what breaks it — the published bound itself included.

use crate::alarm_mute_rules::{
    minute_aligned, put_request, with_members, DURATION_TOO_LONG, SCHEDULE_MALFORMED,
};
use crate::harness::{build_router, call, error_code, error_message, ok};
use ::chrono::{DateTime, TimeDelta, Utc};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// How CloudWatch words the refusal of a one-time schedule carrying window boundaries.
const AT_EXPRESSION_DATES: &str = "Can not set start or expire dates for At expressions";

/// How CloudWatch words the refusal of a rule that is expired the moment it is written.
const RULE_EXPIRED: &str = "The Alarm Mute Rule is expired.";

/// An `at` expression naming the whole minute `at` falls in, in UTC.
fn at_expression(at: DateTime<Utc>) -> String {
    format!("at({})", at.format("%Y-%m-%dT%H:%M"))
}

/// Whether `name` answers to a rule this store holds: the one reader of a written rule.
async fn is_stored(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, _) = call(
        router,
        "GetAlarmMuteRule",
        &json!({"AlarmMuteRuleName": name}),
    )
    .await;
    status == 200
}

/// Assert that `name` answers to no rule, in the class a missing rule is refused in — what a
/// refused write has to leave behind.
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
    assert_eq!(
        body.get("Message").and_then(Value::as_str),
        None,
        "case {case:?}: that class is answered by its code alone, got {body}"
    );
}

#[tokio::test]
async fn a_window_that_cannot_be_run_is_refused_and_stores_nothing() {
    let router = build_router();
    let now = Utc::now();
    let in_a_day = minute_aligned(now + TimeDelta::days(1));
    let in_two_days = minute_aligned(now + TimeDelta::days(2));

    // (case, the request's members beside its name, the sentence the refusal is worded in).
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a window one second longer than the published maximum",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "P15DT1S",
            }}}),
            DURATION_TOO_LONG,
        ),
        (
            "a window a day longer than the published maximum",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "P16D",
            }}}),
            DURATION_TOO_LONG,
        ),
        (
            "a window stated in hours past the published maximum",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "PT361H",
            }}}),
            DURATION_TOO_LONG,
        ),
        (
            "a one-time window past the published maximum",
            json!({"Rule": {"Schedule": {
                "Expression": at_expression(now + TimeDelta::days(1)), "Duration": "P20D",
            }}}),
            DURATION_TOO_LONG,
        ),
        (
            "a one-time schedule given a start date",
            json!({
                "Rule": {"Schedule": {
                    "Expression": at_expression(now + TimeDelta::days(1)),
                    "Duration": "PT1H",
                    "Timezone": "UTC",
                }},
                "StartDate": in_a_day,
            }),
            AT_EXPRESSION_DATES,
        ),
        (
            "a one-time schedule given an expire date",
            json!({
                "Rule": {"Schedule": {
                    "Expression": at_expression(now + TimeDelta::days(1)), "Duration": "PT1H",
                }},
                "ExpireDate": in_two_days,
            }),
            AT_EXPRESSION_DATES,
        ),
        (
            "a one-time schedule given both boundaries, with targets",
            json!({
                "Rule": {"Schedule": {
                    "Expression": at_expression(now + TimeDelta::days(1)), "Duration": "PT1H",
                }},
                "MuteTargets": {"AlarmNames": ["WebServerCPUAlarm"]},
                "StartDate": in_a_day,
                "ExpireDate": in_two_days,
            }),
            AT_EXPRESSION_DATES,
        ),
        (
            "a recurring window expiring a day before it takes effect",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * ?)", "Duration": "PT2H", "Timezone": "UTC",
                }},
                "StartDate": in_two_days,
                "ExpireDate": in_a_day,
            }),
            RULE_EXPIRED,
        ),
        (
            "a recurring window expiring a minute before it takes effect",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * *)", "Duration": "PT2H",
                }},
                "StartDate": in_a_day,
                "ExpireDate": in_a_day - 60,
            }),
            RULE_EXPIRED,
        ),
        (
            "a recurring window expiring the instant it takes effect",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * *)", "Duration": "PT2H",
                }},
                "StartDate": in_a_day,
                "ExpireDate": in_a_day,
            }),
            RULE_EXPIRED,
        ),
        (
            "a window whose boundaries are both in the past, the later one first",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * *)", "Duration": "PT2H",
                }},
                "StartDate": minute_aligned(now - TimeDelta::days(1)),
                "ExpireDate": minute_aligned(now - TimeDelta::days(2)),
            }),
            RULE_EXPIRED,
        ),
        (
            // The schedule itself is judged first: an expression in neither documented form
            // leaves nothing to compare the boundaries against.
            "boundaries that disagree, on an expression in neither form",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "not a cron or at expression!!", "Duration": "PT1H",
                }},
                "StartDate": in_two_days,
                "ExpireDate": in_a_day,
            }),
            SCHEDULE_MALFORMED,
        ),
    ];

    for (index, (case, members, expected)) in cases.into_iter().enumerate() {
        let name = format!("unrunnable-window-{index}");
        let request = with_members(json!({"Name": name}), members);
        let (status, body) = call(&router, "PutAlarmMuteRule", &request).await;
        // CloudWatch answers a precondition its model cannot express in the request-validation
        // class, which the awsJson protocols carry as the local name of
        // `com.amazon.coral.validate#ValidationException`.
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {status} with {body}"
        );
        let got = error_message(&body);
        assert_eq!(
            got, expected,
            "case {case:?}: expected {expected:?}, got {got:?} in {body}"
        );
        assert_absent(&router, case, &name).await;
    }
}

#[tokio::test]
async fn a_window_the_rules_allow_is_stored_as_written() {
    let router = build_router();
    let now = Utc::now();
    let in_a_day = minute_aligned(now + TimeDelta::days(1));
    let in_two_days = minute_aligned(now + TimeDelta::days(2));

    // (case, the request's members beside its name).
    //
    // Each row sits on the other side of one of the refusals above: the published bounds
    // themselves, a one-time schedule carrying NO boundary, boundaries in the order the rule
    // requires, and a single boundary, which states no pair to compare at all.
    let cases: Vec<(&str, Value)> = vec![
        (
            "a window of exactly the published maximum",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "P15D",
            }}}),
        ),
        (
            "the same maximum stated in hours",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "PT360H",
            }}}),
        ),
        (
            "a window of exactly the published minimum",
            json!({"Rule": {"Schedule": {
                "Expression": "cron(0 2 * * *)", "Duration": "PT1M",
            }}}),
        ),
        (
            "a one-time schedule carrying no boundary of its own",
            json!({"Rule": {"Schedule": {
                "Expression": at_expression(now + TimeDelta::days(1)),
                "Duration": "PT1H",
                "Timezone": "UTC",
            }}}),
        ),
        (
            "boundaries a minute apart, in the order the rule requires",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * *)", "Duration": "PT2H",
                }},
                "StartDate": in_a_day,
                "ExpireDate": in_a_day + 60,
            }),
        ),
        (
            "boundaries a day apart",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * ?)", "Duration": "PT2H", "Timezone": "UTC",
                }},
                "StartDate": in_a_day,
                "ExpireDate": in_two_days,
            }),
        ),
        (
            "a start date alone",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(0 2 * * *)", "Duration": "PT2H",
                }},
                "StartDate": in_a_day,
            }),
        ),
        (
            // A rule with no start date "takes effect immediately upon creation"
            // (`PutAlarmMuteRuleInput$StartDate`), so an expiry that has passed states no pair
            // to refuse: the clock decides what such a rule's status is.
            "an expire date that has already passed, alone",
            json!({
                "Rule": {"Schedule": {
                    "Expression": "cron(* * * * *)", "Duration": "PT1H",
                }},
                "ExpireDate": minute_aligned(now - TimeDelta::days(1)),
            }),
        ),
    ];

    for (index, (case, members)) in cases.into_iter().enumerate() {
        let name = format!("runnable-window-{index}");
        let request = with_members(json!({"Name": name}), members);
        let (status, body) = call(&router, "PutAlarmMuteRule", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert!(
            is_stored(&router, &name).await,
            "case {case:?}: an accepted write must store the rule it named"
        );
        // Every member the request carried reads back as it was written.
        let read = ok(
            &router,
            "GetAlarmMuteRule",
            &json!({"AlarmMuteRuleName": name}),
        )
        .await;
        assert_eq!(
            read["Rule"]["Schedule"]["Expression"],
            request["Rule"]["Schedule"]["Expression"],
            "case {case:?}: expected the stored expression, got {read}"
        );
        assert_eq!(
            read["Rule"]["Schedule"]["Duration"],
            request["Rule"]["Schedule"]["Duration"],
            "case {case:?}: expected the stored duration, got {read}"
        );
        for member in ["StartDate", "ExpireDate"] {
            assert_eq!(
                read.get(member).and_then(Value::as_f64),
                request.get(member).and_then(Value::as_f64),
                "case {case:?}: expected the stored {member} {:?}, got {read}",
                request.get(member)
            );
        }
    }
}

#[tokio::test]
async fn a_rule_whose_window_is_still_to_open_reads_back_scheduled() {
    let router = build_router();
    let now = Utc::now();
    ok(
        &router,
        "PutMetricAlarm",
        &json!({
            "AlarmName": "MutedAlarm",
            "Namespace": "Custom/Alarms",
            "MetricName": "Latency",
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 10.0,
            "ComparisonOperator": "GreaterThanThreshold",
        }),
    )
    .await;

    // Rows differ along how far ahead the window's boundaries lie and how wide the window is:
    // whatever the distances, both boundaries ahead of the clock is a rule that has not opened.
    let ahead: Vec<(&str, i64, i64, &str)> = vec![
        ("boundaries a day and two days ahead", 1, 2, "PT1H"),
        ("boundaries two and three days ahead", 2, 3, "PT4H"),
        ("boundaries a day and a fortnight ahead", 1, 14, "P7D"),
    ];
    for (index, (case, start_days, expire_days, duration)) in ahead.into_iter().enumerate() {
        let name = format!("scheduled-rule-{index}");
        let start = minute_aligned(now + TimeDelta::days(start_days));
        let expires = minute_aligned(now + TimeDelta::days(expire_days));
        let request = with_members(
            put_request(&name, "cron(0 2 * * *)", duration),
            json!({
                "MuteTargets": {"AlarmNames": ["MutedAlarm"]},
                "StartDate": start,
                "ExpireDate": expires,
            }),
        );
        ok(&router, "PutAlarmMuteRule", &request).await;

        let read = ok(
            &router,
            "GetAlarmMuteRule",
            &json!({"AlarmMuteRuleName": name}),
        )
        .await;
        // The boundaries come back as the exact instants submitted: no rounding to a coarser
        // granularity and no shift of a zone.
        assert_eq!(
            (
                read.get("StartDate").and_then(Value::as_f64),
                read.get("ExpireDate").and_then(Value::as_f64),
            ),
            (Some(start as f64), Some(expires as f64)),
            "case {case:?}: expected the exact boundaries submitted, got {read}"
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some("SCHEDULED"),
            "case {case:?}: a window still to open is SCHEDULED, got {read}"
        );
        assert_eq!(
            read.get("MuteTargets"),
            request.get("MuteTargets"),
            "case {case:?}: expected the stored targets, got {read}"
        );
    }
}
