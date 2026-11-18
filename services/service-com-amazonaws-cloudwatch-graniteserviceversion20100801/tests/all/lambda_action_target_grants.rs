// SPDX-License-Identifier: CC0-1.0

//! WHICH of the Lambda functions an alarm's fired action list names are invoked, and WHEN that
//! is decided — end to end over the wire through the real `MultiServiceRouter` with CloudWatch,
//! AWS Lambda and CloudWatch Logs on one engine, so what a client sees is what is asserted.
//!
//! The RULE asserted here: each function an alarm action list names is handed the transition on
//! its own, and every one of those invocations is decided by THAT function's own resource-based
//! policy as it stands at the moment the transition fires — so one transition invokes exactly
//! the named functions whose policy then grants this service `lambda:InvokeFunction`, accounts
//! for every named function either way, and a grant WITHDRAWN between two transitions withholds
//! the invocation the second would have made.
//!
//! What the sibling suite `lambda_alarm_actions.rs` pins and this one does not repeat: the FORMS
//! a grant can take (none, unscoped, scoped by `aws:SourceArn`, a grant of another action),
//! decided for ONE target per row against a transition a client FORCES. What only this suite
//! reaches: two targets whose policies DISAGREE sharing one action list, so the decision is
//! shown to be per target rather than per transition, with the alarm moved by its own evaluation
//! of a datapoint a client published — the path on which a client makes no further call — and
//! the grant's own LIFETIME, so the policy is shown to be read at the transition rather than
//! when the alarm was written.
//!
//! The rows differ along what the rule generalises over: which of the two named targets carries
//! the grant (the first, the second, both, neither), so the number of targets one transition
//! invokes runs from none to all of them and an un-granted target is named both before and after
//! a granted one.

use std::time::Instant;

use serde_json::{json, Value};

use crate::harness::{until_state, Emulator, LOOK_INTERVAL, WORK_WAIT};
use crate::lambda_alarm_actions::{
    action_items, add_permission, create_function, emulator_with_lambda, lambda, logged,
    monitoring, until_active, until_logged, ALARM_ACTION_PRINCIPAL, INVOKE_ACTION, PRINTED_PREFIX,
    STATEMENT_ID,
};

/// The namespace the rows publish into: a customer namespace, so the datapoint goes over the
/// wire the way a client's own does (`PutMetricData` refuses an `AWS/` namespace).
const NAMESPACE: &str = "Custom/LambdaActionTargets";

/// The metric the alarms watch inside it.
const METRIC: &str = "Load";

/// The threshold every alarm here grades against, and the value published for it, which is on
/// the breaching side of it.
const THRESHOLD: f64 = 50.0;
/// See [`THRESHOLD`].
const BREACHING_VALUE: f64 = THRESHOLD + 10.0;

/// The cadence the alarms are evaluated on: the shortest `PutMetricAlarm` admits ("Valid values
/// are 10, 20, 30, and any multiple of 60", `PutMetricAlarmInput$Period`).
const PERIOD: i64 = 10;

/// The granularity the published datapoint is stored at: "Valid values are 1 and 60"
/// (`MetricDatum$StorageResolution`), the finer of the two, so the window it lands in is the
/// alarm's own period and closes within it.
const STORAGE_RESOLUTION: i64 = 1;

/// The arm a window holding no datapoint is counted under: "ignore — the current alarm state is
/// maintained" (`PutMetricAlarmInput$TreatMissingData`) — so the only transition any alarm here
/// makes is the one its published datapoint decides, and it holds the state afterwards.
const TREAT_MISSING_DATA: &str = "ignore";

/// One row: which of the two targets the alarm's `AlarmActions` names carries the grant.
struct Row {
    case: &'static str,
    granted: [bool; 2],
}

/// The rows.
const ROWS: &[Row] = &[
    Row {
        case: "a granted target named before an un-granted twin",
        granted: [true, false],
    },
    Row {
        case: "an un-granted target named before a granted twin",
        granted: [false, true],
    },
    Row {
        case: "both named targets granted",
        granted: [true, true],
    },
    Row {
        case: "neither named target granted",
        granted: [false, false],
    },
];

/// One target of a row: the function, the ARN the action list names it by, and whether its own
/// policy grants this service the invocation.
struct Target {
    function: String,
    arn: String,
    granted: bool,
}

/// One configured row: its alarm, the series that alarm watches and the targets its ALARM action
/// list names.
struct Configured {
    case: &'static str,
    alarm: String,
    dimension: String,
    targets: Vec<Target>,
}

/// A metric alarm over the row's own series whose `AlarmActions` names `actions`, graded on the
/// shortest cadence an alarm may be evaluated on.
async fn put_alarm(emulator: &Emulator, alarm: &str, dimension: &str, actions: &[&str]) {
    monitoring(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": alarm,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "Case", "Value": dimension}],
            "Statistic": "Maximum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": THRESHOLD,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": TREAT_MISSING_DATA,
            "AlarmActions": actions,
        }),
    )
    .await;
}

/// Publish ONE breaching datapoint for the row's series, stamped at the instant of the call
/// ("If you do not specify a time stamp, the time stamp of the data point is the time that
/// `PutMetricData` was called", `MetricDatum$Timestamp`).
async fn publish_breach(emulator: &Emulator, dimension: &str) {
    monitoring(
        emulator,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Case", "Value": dimension}],
                "Value": BREACHING_VALUE,
                "StorageResolution": STORAGE_RESOLUTION,
            }],
        }),
    )
    .await;
}

/// Withdraw the grant written under [`STATEMENT_ID`] from the function's resource-based policy,
/// which is how a client takes an invocation back (`RemovePermission`).
async fn remove_permission(emulator: &Emulator, case: &str, function: &str) {
    let (status, answered) = lambda(
        emulator,
        "DELETE",
        &format!("/2015-03-31/functions/{function}/policy/{STATEMENT_ID}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected HTTP 204 from Lambda RemovePermission, got {status} ({answered})"
    );
}

/// The alarm's `Action` history items, looked at again until it holds at least `wanted` of them
/// or the deadline passes — a wait for the CONDITION, so a case asserts on what was recorded
/// rather than on a span.
async fn until_accounted(emulator: &Emulator, alarm: &str, wanted: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + WORK_WAIT;
    loop {
        let items = action_items(emulator, alarm).await;
        if items.len() >= wanted || Instant::now() >= giving_up_at {
            return items;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The one history item whose summary names `arn`, of the items the alarm accounted for.
fn item_for<'a>(case: &str, items: &'a [Value], arn: &str) -> &'a Value {
    let mut naming = items
        .iter()
        .filter(|item| summary(item).ends_with(&format!(" action {arn}")));
    let found = naming
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no action item names {arn:?}, got {items:?}"));
    assert!(
        naming.next().is_none(),
        "case {case:?}: expected the transition to account for {arn:?} exactly once, got \
         {items:?}"
    );
    found
}

/// The `HistorySummary` of one history item.
fn summary(item: &Value) -> &str {
    item["HistorySummary"].as_str().unwrap_or_default()
}

/// The `HistoryData` document of one history item, parsed.
fn data(item: &Value) -> Value {
    serde_json::from_str(item["HistoryData"].as_str().unwrap_or("null")).unwrap_or(Value::Null)
}

/// The sentence AWS Lambda refuses an invocation its target's policy admits nothing of.
fn refusal(arn: &str) -> String {
    format!(
        "User: {ALARM_ACTION_PRINCIPAL} is not authorized to perform: {INVOKE_ACTION} on \
         resource: {arn} because no resource-based policy allows the {INVOKE_ACTION} action"
    )
}

/// RULE: one transition hands its action list's every function the state change, and each of
/// those invocations is decided by that function's own resource-based policy — so the targets
/// whose policy grants this service the invocation run and the targets whose policy grants it
/// nothing are not invoked at all, whichever of them the list names first.
#[tokio::test]
async fn one_transition_decides_each_function_it_fires_by_that_functions_own_policy() {
    // Every row watches its own series and names its own pair of targets, so all of them are
    // configured on ONE emulator and the close of the window their datapoints land in is waited
    // out once rather than per row.
    let emulator = emulator_with_lambda();
    let mut rows: Vec<Configured> = Vec::with_capacity(ROWS.len());

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let mut targets: Vec<Target> = Vec::with_capacity(row.granted.len());
        for (slot, granted) in row.granted.iter().enumerate() {
            let function = format!("action-target-{index}-{slot}");
            let arn = create_function(&emulator, case, &function).await;
            // The grant a client writes for an alarm action: the target function's own
            // resource-based policy, naming the principal CloudWatch invokes it as.
            if *granted {
                add_permission(&emulator, case, &function, INVOKE_ACTION, None).await;
            }
            targets.push(Target {
                function,
                arn,
                granted: *granted,
            });
        }
        let named: Vec<&str> = targets.iter().map(|target| target.arn.as_str()).collect();
        let alarm = format!("lambda-action-targets-{index}");
        let dimension = format!("targets-{index}");
        put_alarm(&emulator, &alarm, &dimension, &named).await;
        rows.push(Configured {
            case,
            alarm,
            dimension,
            targets,
        });
    }

    // Each created function leaves `Pending` by the clock: looked at until Lambda reports it
    // `Active`, so an invocation withheld below is the policy's refusal and not the lifecycle's.
    for row in &rows {
        for target in &row.targets {
            until_active(&emulator, row.case, &target.function).await;
        }
    }

    // What moves each alarm is its OWN evaluation of the datapoint published for it. No call
    // forces the transition, and the window the datapoint lands in has to close before any
    // evaluation grades it.
    for row in &rows {
        publish_breach(&emulator, &row.dimension).await;
    }

    for row in &rows {
        let case = row.case;
        let reached = until_state(&emulator, &row.alarm, "ALARM").await;
        assert_eq!(
            reached, "ALARM",
            "case {case:?}: expected the published datapoint to drive {} into ALARM, got \
             {reached:?}",
            row.alarm
        );

        let items = until_accounted(&emulator, &row.alarm, row.targets.len()).await;
        assert_eq!(
            items.len(),
            row.targets.len(),
            "case {case:?}: expected the transition to account for each of the {} functions it \
             fires exactly once, got {items:?}",
            row.targets.len()
        );

        for target in &row.targets {
            let item = item_for(case, &items, &target.arn);
            let found = summary(item);
            let document = data(item);
            if target.granted {
                assert_eq!(
                    found,
                    format!("Successfully executed action {}", target.arn),
                    "case {case:?}: expected the action naming the granted {} to be recorded as \
                     executed, got {found:?} with {document}",
                    target.arn
                );
                assert_eq!(
                    document["actionState"],
                    json!("Succeeded"),
                    "case {case:?}: expected the granted target's action to have succeeded, got \
                     {document}"
                );
                // What the function was handed is the transition itself, so the record of the
                // execution names the alarm that fired it.
                let handed: Value =
                    serde_json::from_str(document["publishedMessage"].as_str().unwrap_or("null"))
                        .unwrap_or(Value::Null);
                assert_eq!(
                    handed["alarmData"]["alarmName"],
                    json!(row.alarm),
                    "case {case:?}: expected the payload handed to {} to name the alarm that \
                     fired it, got {handed}",
                    target.arn
                );
                // And the target RAN: the line its handler printed is in its own log group.
                let lines = until_logged(&emulator, &target.function).await;
                let expected = format!("{PRINTED_PREFIX}{}:ALARM", row.alarm);
                assert!(
                    lines.iter().any(|line| line.contains(&expected)),
                    "case {case:?}: expected the granted target {} to run and print {expected:?} \
                     into its own log group, got {lines:?}",
                    target.function
                );
                continue;
            }

            assert_eq!(
                found,
                format!("Failed to execute action {}", target.arn),
                "case {case:?}: expected the action naming the un-granted {} to be recorded as \
                 failed, got {found:?} with {document}",
                target.arn
            );
            let reported = document["error"].as_str().unwrap_or_default();
            let expected = refusal(&target.arn);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the recorded error for {} to be {expected:?}, got \
                 {reported:?}",
                target.arn
            );
            assert!(
                document["publishedMessage"].is_null(),
                "case {case:?}: expected a refused action to report nothing handed over, got \
                 {document}"
            );
            // And nothing ran: the un-granted target's own log group holds no line of the
            // handler. The record read above is written as the handover is decided, so a run
            // owed here would already have been started — and a function that was never invoked
            // has no log group at all.
            let lines = logged(&emulator, &target.function).await;
            assert!(
                !lines.iter().any(|line| line.contains(PRINTED_PREFIX)),
                "case {case:?}: expected no run of the un-granted target {}, got {lines:?}",
                target.function
            );
        }
    }
}

/// RULE, the same one along the grant's LIFETIME: the policy that decides an invocation is the
/// one in force when the transition fires, so a grant a client withdraws between two transitions
/// admits the first invocation and withholds the second.
#[tokio::test]
async fn a_grant_withdrawn_between_transitions_withholds_the_next_invocation() {
    let case = "a grant withdrawn between the two transitions that fire it";
    let emulator = emulator_with_lambda();
    let function = "action-target-grant-lifetime";
    let alarm = "lambda-action-grant-lifetime";

    let arn = create_function(&emulator, case, function).await;
    add_permission(&emulator, case, function, INVOKE_ACTION, None).await;
    until_active(&emulator, case, function).await;

    // The same function in both action lists, so the two transitions below fire the same target
    // and the only thing that differs between them is the policy in force.
    monitoring(
        &emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": alarm,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "Case", "Value": "grant-lifetime"}],
            "Statistic": "Maximum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": THRESHOLD,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": TREAT_MISSING_DATA,
            "AlarmActions": [&arn],
            "OKActions": [&arn],
        }),
    )
    .await;

    crate::harness::set_alarm_state(&emulator, alarm, "ALARM").await;
    let first = until_accounted(&emulator, alarm, 1).await;
    assert_eq!(
        summary(item_for(case, &first, &arn)),
        format!("Successfully executed action {arn}"),
        "case {case:?}: expected the transition made while the grant stood to be recorded as \
         executed, got {first:?}"
    );

    // The client takes the grant back, and the alarm moves again.
    remove_permission(&emulator, case, function).await;
    crate::harness::set_alarm_state(&emulator, alarm, "OK").await;
    let items = until_accounted(&emulator, alarm, first.len() + 1).await;

    let executed: Vec<&Value> = items
        .iter()
        .filter(|item| summary(item) == format!("Successfully executed action {arn}"))
        .collect();
    let refused: Vec<&Value> = items
        .iter()
        .filter(|item| summary(item) == format!("Failed to execute action {arn}"))
        .collect();
    assert_eq!(
        executed.len(),
        1,
        "case {case:?}: expected exactly the one transition made while the grant stood to have \
         executed the action, got {items:?}"
    );
    assert!(
        !refused.is_empty(),
        "case {case:?}: expected the transition made after the grant was withdrawn to be \
         recorded as refused, got {items:?}"
    );
    assert_eq!(
        executed.len() + refused.len(),
        items.len(),
        "case {case:?}: expected every account of this action to be one of the two, got {items:?}"
    );
    let expected = refusal(&arn);
    for item in &refused {
        let document = data(item);
        let reported = document["error"].as_str().unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the recorded error to be {expected:?}, got {reported:?}"
        );
        assert!(
            document["publishedMessage"].is_null(),
            "case {case:?}: expected a refused action to report nothing handed over, got \
             {document}"
        );
    }

    // What ran is exactly the invocation the grant admitted: the function's own log group holds
    // the line of the transition made while the grant stood, and none of the transition made
    // after it was withdrawn.
    let lines = until_logged(&emulator, function).await;
    let admitted = format!("{PRINTED_PREFIX}{alarm}:ALARM");
    let withheld = format!("{PRINTED_PREFIX}{alarm}:OK");
    assert!(
        lines.iter().any(|line| line.contains(&admitted)),
        "case {case:?}: expected the invocation the grant admitted to have run and printed \
         {admitted:?}, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains(&withheld)),
        "case {case:?}: expected no run of the invocation the withdrawn grant withheld, got \
         {lines:?}"
    );
}
