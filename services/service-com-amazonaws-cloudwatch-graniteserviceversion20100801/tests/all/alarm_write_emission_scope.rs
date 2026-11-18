// SPDX-License-Identifier: CC0-1.0

//! WHICH event family each call that touches an alarm publishes, end to end over the wire
//! through the real `MultiServiceRouter` with CloudWatch, EventBridge and Amazon SQS on one
//! engine — so what is asserted is what a client sees: the messages on the queue a rule
//! matching BOTH of an alarm's event families targets.
//!
//! The RULE asserted here: **every call that leaves an alarm's CONFIGURATION different from the
//! one the account held publishes exactly one `CloudWatch Alarm Configuration Change` promptly —
//! whichever operation made the write, including the one that only turns the alarm's actions on
//! or off, because `ActionsEnabled` ("Indicates whether actions should be executed during any
//! changes to the alarm state", `MetricAlarm$ActionsEnabled`) is a member of that configuration
//! like any other — while a call that leaves the configuration as it was publishes none: a
//! forced state is the alarm's state rather than its configuration and publishes the state
//! change alone, a tagging call changes something that is not the configuration at all ("any
//! tags you specify in this parameter are ignored. To change the tags of an existing alarm, use
//! TagResource or UntagResource", `PutMetricAlarmInput$Tags`), and a write that re-states the
//! stored configuration overwrites it with itself.**
//!
//! The rows differ along what the rule generalises over: WHICH operation made the call
//! (`PutMetricAlarm`, `DisableAlarmActions`, `SetAlarmState`, `TagResource`,
//! `EnableAlarmActions`), which DIRECTION the actions switch moved, whether the call changed
//! anything at all, and — in the second table — the KIND of alarm whose actions are switched (a
//! metric alarm, a composite alarm; a log alarm's actions are written by the same one writer,
//! and its own scheduled query needs a CloudWatch Logs mount this suite does not stand up).
//!
//! Nothing here is asserted about how fast the host runs: an event is waited for by looking
//! again until it has arrived, and the absences are graded against the count of events the rule
//! owes rather than against a span.

use serde_json::{json, Value};

use crate::alarm_configuration_change_events::{
    alarm_arn, assert_envelope, collect, emulator_with_a_bus, eventbridge, given_target_queue,
    monitoring, queue_arn_of, CREATE, DETAIL_TYPE, LOOK_INTERVAL, SOURCE, UPDATE, WORK_WAIT,
};
use crate::harness::{merged, Emulator};

/// The `detail-type` of an alarm STATE change, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make this suite pass.
const STATE_CHANGE_DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The namespace and metric the metric alarms below watch. Nothing publishes into it, so no
/// evaluation of theirs ever moves one of them and every configuration change on the queue is
/// one a call of this suite made.
const NAMESPACE: &str = "Custom/EmissionScope";
const METRIC: &str = "EmitScopeProbe";

/// The metric alarm request of this suite, with `patch` merged over it.
fn metric_alarm(name: &str, patch: &Value) -> Value {
    merged(
        json!({
            "AlarmName": name,
            "AlarmDescription": "the alarm of this suite",
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "missing",
            "ActionsEnabled": true,
        }),
        patch,
    )
}

/// Wire a rule matching BOTH event families of the alarm `alarm_name` to the queue at `url`:
/// one queue holds everything the account published about that alarm, which is how a row
/// asserts that a call published the one family it owes and nothing of the other.
async fn given_rule_for_both_families(
    emulator: &Emulator,
    case: &str,
    rule_name: &str,
    alarm_name: &str,
    queue_url: &str,
) {
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE, STATE_CHANGE_DETAIL_TYPE],
        "detail": {"alarmName": [alarm_name]},
    })
    .to_string();
    eventbridge(
        emulator,
        case,
        "PutRule",
        &json!({"Name": rule_name, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let queue_arn = queue_arn_of(emulator, queue_url).await;
    let wired = eventbridge(
        emulator,
        case,
        "PutTargets",
        &json!({"Rule": rule_name, "Targets": [{"Id": "q", "Arn": queue_arn}]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );
}

/// The events among `held` carrying `detail_type`, in the order they were delivered.
fn of_family<'held>(held: &'held [Value], detail_type: &str) -> Vec<&'held Value> {
    held.iter()
        .filter(|event| event["detail-type"] == json!(detail_type))
        .collect()
}

/// Wait until `wanted` events of the family `detail_type` have been delivered to the queue at
/// `url`, adding everything delivered meanwhile to `held`.
///
/// The wait is for the CONDITION rather than for a span: it looks again until the events are
/// there or [`WORK_WAIT`] has passed.
async fn until_family_delivered(
    emulator: &Emulator,
    url: &str,
    detail_type: &str,
    wanted: usize,
    held: &mut Vec<Value>,
) {
    let looks = WORK_WAIT.as_millis() / LOOK_INTERVAL.as_millis().max(1);
    for _ in 0..looks {
        collect(emulator, url, held).await;
        if of_family(held, detail_type).len() >= wanted {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    collect(emulator, url, held).await;
}

/// What one row's call owes on the rule's queue.
#[derive(Debug)]
enum Owed {
    /// One configuration change, reporting this `detail.operation`.
    Configuration(&'static str),
    /// A state change alone, reporting this `detail.state.value`, and no configuration change.
    StateChangeAlone(&'static str),
    /// Nothing at all: the call left the alarm's configuration as it was.
    Nothing,
}

/// RULE: each call publishes the one event family it owes — a configuration change for every
/// write that left the configuration different, a state change alone for a forced state, and
/// nothing for a call that changed no configuration.
#[tokio::test]
async fn each_alarm_write_publishes_only_the_event_family_it_owes() {
    let case = "the emit scope of one alarm's writes";
    let emulator = emulator_with_a_bus();
    let alarm = "emission-scope-metric";
    let queue = given_target_queue(&emulator, "emission-scope-metric-q").await;
    // The rule is wired BEFORE the alarm exists, so the event the create publishes is matched by
    // a rule that was already live.
    given_rule_for_both_families(
        &emulator,
        case,
        "emission-scope-metric-rule",
        alarm,
        &queue,
    )
    .await;

    let mut held: Vec<Value> = Vec::new();
    // The create, which every row below is a call against.
    monitoring(&emulator, "PutMetricAlarm", &metric_alarm(alarm, &json!({}))).await;
    let arn = alarm_arn(&emulator, case, "MetricAlarm", alarm).await;
    until_family_delivered(&emulator, &queue, DETAIL_TYPE, 1, &mut held).await;
    let created = of_family(&held, DETAIL_TYPE);
    assert_eq!(
        created.len(),
        1,
        "case {case:?}: expected the create to publish one configuration change, got {held:?}"
    );
    assert_envelope(case, created[0], alarm, &arn, CREATE);

    // Each row is one call, and the event family it owes. The rows are made in order against
    // the alarm the create left.
    let rows: Vec<(&str, &str, Value, Owed)> = vec![
        (
            "a call that turns the alarm's actions off",
            "DisableAlarmActions",
            json!({"AlarmNames": [alarm]}),
            Owed::Configuration(UPDATE),
        ),
        (
            "a call that forces the alarm's state",
            "SetAlarmState",
            json!({
                "AlarmName": alarm,
                "StateValue": "ALARM",
                "StateReason": "forced by this suite",
            }),
            Owed::StateChangeAlone("ALARM"),
        ),
        (
            "a call that tags the alarm",
            "TagResource",
            json!({"ResourceARN": arn, "Tags": [{"Key": "team", "Value": "platform"}]}),
            Owed::Nothing,
        ),
        (
            "a write that re-states the stored configuration",
            "PutMetricAlarm",
            metric_alarm(alarm, &json!({"ActionsEnabled": false})),
            Owed::Nothing,
        ),
        (
            "a call that turns the alarm's actions back on",
            "EnableAlarmActions",
            json!({"AlarmNames": [alarm]}),
            Owed::Configuration(UPDATE),
        ),
    ];

    // How many configuration changes the rule owes so far: the create's, plus one per row that
    // changed the configuration.
    let mut configurations = 1_usize;
    // And how many state changes: one per forced state.
    let mut states = 0_usize;
    for (row, operation, body, owed) in &rows {
        let row_case = format!("{case}, row {row:?}");
        monitoring(&emulator, operation, body).await;
        match owed {
            Owed::Configuration(expected_operation) => {
                configurations += 1;
                until_family_delivered(&emulator, &queue, DETAIL_TYPE, configurations, &mut held)
                    .await;
                let changes = of_family(&held, DETAIL_TYPE);
                assert_eq!(
                    changes.len(),
                    configurations,
                    "case {row_case}: expected {configurations} configuration change(s) on the \
                     rule's queue, got {held:?}"
                );
                let event = changes[configurations - 1];
                assert_envelope(&row_case, event, alarm, &arn, expected_operation);
            }
            Owed::StateChangeAlone(expected_state) => {
                states += 1;
                until_family_delivered(
                    &emulator,
                    &queue,
                    STATE_CHANGE_DETAIL_TYPE,
                    states,
                    &mut held,
                )
                .await;
                let changes = of_family(&held, STATE_CHANGE_DETAIL_TYPE);
                let forced = changes.iter().any(|event| {
                    event.pointer("/detail/state/value") == Some(&json!(expected_state))
                });
                assert!(
                    forced,
                    "case {row_case}: expected a state change reporting {expected_state:?}, got \
                     {changes:?}"
                );
                assert_eq!(
                    of_family(&held, DETAIL_TYPE).len(),
                    configurations,
                    "case {row_case}: a forced state changes no configuration, so it publishes \
                     no configuration change, got {held:?}"
                );
            }
            Owed::Nothing => {
                collect(&emulator, &queue, &mut held).await;
                assert_eq!(
                    of_family(&held, DETAIL_TYPE).len(),
                    configurations,
                    "case {row_case}: expected the call to publish no configuration change, got \
                     {held:?}"
                );
            }
        }
    }

    // One event per configuration-changing call and not one more: the create, the call that
    // turned the actions off and the call that turned them back on.
    let operations: Vec<String> = of_family(&held, DETAIL_TYPE)
        .iter()
        .map(|event| event["detail"]["operation"].to_string())
        .collect();
    let expected: Vec<String> = [CREATE, UPDATE, UPDATE]
        .iter()
        .map(|operation| json!(operation).to_string())
        .collect();
    assert_eq!(
        operations, expected,
        "case {case:?}: expected the operations {expected:?} in order on the rule's queue, got \
         {operations:?} from {held:?}"
    );
}

/// RULE, over the KIND of alarm and the DIRECTION of the switch: turning an alarm's actions off
/// and on again publishes one configuration-change update per call that really changed the
/// member, reporting the setting it stored beside the one it replaced — and a call asking for
/// the setting the alarm already has publishes nothing.
#[tokio::test]
async fn turning_an_alarms_actions_off_and_on_publishes_one_update_per_change() {
    let emulator = emulator_with_a_bus();
    let child = "emission-scope-switch-child";
    // The child a composite alarm's rule reads: what makes that rule well formed.
    monitoring(&emulator, "PutMetricAlarm", &metric_alarm(child, &json!({}))).await;

    // The rows differ along the KIND of alarm whose actions are switched: the member, the
    // writer and the event are the same for both.
    let composite = "emission-scope-switch-composite";
    let metric = "emission-scope-switch-metric";
    let rows: Vec<(&str, &str, &str, &str, Value)> = vec![
        (
            "a metric alarm",
            "MetricAlarm",
            metric,
            "PutMetricAlarm",
            metric_alarm(metric, &json!({})),
        ),
        (
            "a composite alarm",
            "CompositeAlarm",
            composite,
            "PutCompositeAlarm",
            json!({
                "AlarmName": composite,
                "AlarmRule": format!("ALARM(\"{child}\")"),
                "AlarmDescription": "the composite alarm of this suite",
                "ActionsEnabled": true,
            }),
        ),
    ];

    for (case, kind, alarm, create_operation, create_body) in &rows {
        let queue = given_target_queue(&emulator, &format!("{alarm}-q")).await;
        given_rule_for_both_families(
            &emulator,
            case,
            &format!("{alarm}-rule"),
            alarm,
            &queue,
        )
        .await;

        let mut held: Vec<Value> = Vec::new();
        monitoring(&emulator, create_operation, create_body).await;
        let arn = alarm_arn(&emulator, case, kind, alarm).await;
        until_family_delivered(&emulator, &queue, DETAIL_TYPE, 1, &mut held).await;

        // The switch, in order: off, off again, on, on again. A row carrying the setting the
        // call REPLACED is one that changed the member; `None` is a call that found it already
        // at the setting it asked for.
        let switches: [(&str, &str, bool, Option<bool>); 4] = [
            ("the call that turns the actions off", "DisableAlarmActions", false, Some(true)),
            ("that same call again", "DisableAlarmActions", false, None),
            ("the call that turns them back on", "EnableAlarmActions", true, Some(false)),
            ("that same call again", "EnableAlarmActions", true, None),
        ];
        let mut configurations = 1_usize;
        for (switch, operation, enabled, replaced) in switches {
            let row_case = format!("{case}, {switch}");
            monitoring(&emulator, operation, &json!({"AlarmNames": [alarm]})).await;
            let Some(before) = replaced else {
                collect(&emulator, &queue, &mut held).await;
                assert_eq!(
                    of_family(&held, DETAIL_TYPE).len(),
                    configurations,
                    "case {row_case}: a call asking for the setting the alarm already has \
                     changes no configuration, so it publishes nothing, got {held:?}"
                );
                continue;
            };
            configurations += 1;
            until_family_delivered(&emulator, &queue, DETAIL_TYPE, configurations, &mut held)
                .await;
            let changes = of_family(&held, DETAIL_TYPE);
            assert_eq!(
                changes.len(),
                configurations,
                "case {row_case}: expected {configurations} configuration change(s) on the \
                 rule's queue, got {held:?}"
            );
            let event = changes[configurations - 1];
            assert_envelope(&row_case, event, alarm, &arn, UPDATE);
            for (member, pointer, expected) in [
                (
                    "the setting the call stored",
                    "/detail/configuration/actionsEnabled",
                    json!(enabled),
                ),
                (
                    "the setting it replaced",
                    "/detail/previousConfiguration/actionsEnabled",
                    json!(before),
                ),
            ] {
                let found = event.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    found, expected,
                    "case {row_case}: expected {member} to be {expected} at {pointer}, got \
                     {found} in {event}"
                );
            }
        }
    }
}
