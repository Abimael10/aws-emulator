// SPDX-License-Identifier: CC0-1.0

//! WHICH alarm transitions a rule whose pattern names BOTH an alarm resource and a state
//! receives, and WHAT that rule's target is handed, end to end over the wire through the real
//! `MultiServiceRouter` with CloudWatch, EventBridge and Amazon SQS on one engine — so what is
//! asserted is what a client sees: the messages on the queue the rule targets.
//!
//! The RULE asserted here: **a rule's `EventPattern` admits an event only when EVERY member the
//! pattern names matches it, so a pattern naming one alarm's ARN in `resources` and one state in
//! `detail.state.value` receives exactly that alarm's transitions INTO that state — a transition
//! of the same alarm into any other state and a transition of another alarm reach it not at all —
//! and the target of such a rule is handed the published envelope unless it carries an
//! `InputTransformer` ("settings to enable you to provide custom input to a target based on
//! certain event data", `Target$InputTransformer`), in which case it is handed that
//! `InputTemplate` with each `<key>` of its `InputPathsMap` replaced by the value the key's JSON
//! path selects out of the event.**
//!
//! That is the rule a client's alarm-driven automation depends on: a state-and-resource pattern
//! is how one rule is wired to one alarm's one transition, and the near misses are what tells a
//! client the pattern it wrote is the pattern it got.
//!
//! The rows differ along what the rule generalises over: WHICH state of the three an alarm can be
//! in the pattern selects (`StateValue`: `ALARM`, `INSUFFICIENT_DATA`, `OK`, per
//! `SetAlarmStateInput$StateValue`) — so each state is in turn the one selected and, in the other
//! rows, one of the near misses that must be withheld — and whether the target takes the event as
//! published or reshapes it through an `InputTransformer`.
//!
//! Nothing here is asserted about how fast the host runs: the delivery is waited for by looking
//! again until it has arrived, and what the pattern WITHHELD is graded against the count of
//! events the rule owes — every near miss is asked for before the transition the rule matches, so
//! one that was going to be delivered has had at least as long as the one that was.

use std::time::Instant;

use serde_json::{json, Value};

use crate::alarm_configuration_change_events::{
    alarm_arn, collect, emulator_with_a_bus, eventbridge, given_target_queue, monitoring,
    queue_arn_of, until_delivered, LOOK_INTERVAL, SOURCE,
};
use crate::harness::{set_alarm_state, Emulator, QUIET_WATCH};

/// The `detail-type` of an alarm STATE change, spelled out here rather than imported from the
/// service crate, so a wrong constant in the implementation cannot make this suite pass.
const STATE_CHANGE_DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The kind of alarm every row writes, as `DescribeAlarms` names it.
const METRIC_ALARM: &str = "MetricAlarm";

/// The three values an alarm's state takes (`SetAlarmStateInput$StateValue`, "one of: ALARM |
/// INSUFFICIENT_DATA | OK"), in the order a row walks the two it does not select.
const ALARM: &str = "ALARM";
const OK: &str = "OK";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const STATES: [&str; 3] = [ALARM, OK, INSUFFICIENT_DATA];

/// The namespace and metric the alarms below watch. Nothing ever publishes into it, and the
/// alarms maintain their state where the data is missing ([`TREAT_MISSING_DATA`]), so every event
/// on a row's queue is one of the transitions that row asked for.
const NAMESPACE: &str = "Custom/PatternSelectivity";
const METRIC: &str = "Backlog";

/// "ignore: the current alarm state is maintained" (`PutMetricAlarmInput$TreatMissingData`): the
/// evaluation of a series nothing publishes into moves neither alarm of a row.
const TREAT_MISSING_DATA: &str = "ignore";

/// The period those alarms aggregate over, as the modelled seconds member carries it.
const PERIOD: i64 = 60;

/// The `InputPathsMap` of the row whose target reshapes the event, and the `InputTemplate` those
/// keys are filled into: three of the event's own values, and not one constant of the row's.
const RESHAPE_PATHS: [(&str, &str); 3] = [
    ("alarm", "$.detail.alarmName"),
    ("from", "$.detail.previousState.value"),
    ("to", "$.detail.state.value"),
];
const RESHAPE_TEMPLATE: &str = r#"{"alarm":"<alarm>","from":"<from>","to":"<to>"}"#;

/// What a row's target receives.
#[derive(Debug)]
enum Expect {
    /// The published envelope, read by member.
    Envelope,
    /// The template of [`RESHAPE_TEMPLATE`], filled from the event.
    Reshaped,
}

/// One row: the state the rule's pattern selects, and what its target is handed.
struct Row {
    case: &'static str,
    selected: &'static str,
    expect: Expect,
}

/// The states a row forces its alarms through, in order: the two the pattern does NOT select
/// first — each a near miss the rule must withhold — and the selected one last, which is the
/// transition the rule owes. Consecutive entries differ, and the first differs from the
/// `INSUFFICIENT_DATA` a new alarm reports, so each force is a transition of its own.
fn walk(selected: &'static str) -> Vec<&'static str> {
    let mut walked: Vec<&'static str> = STATES
        .iter()
        .copied()
        .filter(|state| *state != selected)
        .collect();
    walked.push(selected);
    walked
}

/// The metric alarm `name`, watching a series of the caller's own that nothing publishes into.
async fn given_alarm(emulator: &Emulator, name: &str) {
    monitoring(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "AlarmName", "Value": name}],
            "Statistic": "Average",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": TREAT_MISSING_DATA,
        }),
    )
    .await;
}

/// Wire a rule whose pattern names BOTH `alarm_arn` in the envelope's `resources` and `selected`
/// in `detail.state.value` to the queue at `queue_url`, with the target reshaping the event when
/// `expect` says so.
async fn given_state_and_resource_rule(
    emulator: &Emulator,
    case: &str,
    rule_name: &str,
    alarm_arn: &str,
    selected: &str,
    queue_url: &str,
    expect: &Expect,
) {
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [STATE_CHANGE_DETAIL_TYPE],
        "resources": [alarm_arn],
        "detail": {"state": {"value": [selected]}},
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
    let mut target = json!({"Id": "q", "Arn": queue_arn});
    if matches!(expect, Expect::Reshaped) {
        let paths: serde_json::Map<String, Value> = RESHAPE_PATHS
            .iter()
            .map(|(key, path)| ((*key).to_string(), json!(path)))
            .collect();
        target["InputTransformer"] = json!({
            "InputPathsMap": paths,
            "InputTemplate": RESHAPE_TEMPLATE,
        });
    }
    let wired = eventbridge(
        emulator,
        case,
        "PutTargets",
        &json!({"Rule": rule_name, "Targets": [target]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the queue target to be accepted, got {wired}"
    );
}

/// Keep looking at the queue at `url` over [`QUIET_WATCH`], adding anything delivered meanwhile
/// to `held`: the span in which a near miss the pattern should have withheld would turn up.
async fn over_the_quiet_watch(emulator: &Emulator, url: &str, held: &mut Vec<Value>) {
    let deadline = Instant::now() + QUIET_WATCH;
    while Instant::now() < deadline {
        collect(emulator, url, held).await;
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    collect(emulator, url, held).await;
}

/// RULE: a rule whose `EventPattern` names one alarm's ARN in `resources` and one state in
/// `detail.state.value` receives that alarm's transition into that state and nothing else, and
/// its target is handed the envelope — or, where the target carries an `InputTransformer`, the
/// document that template renders out of the event.
#[tokio::test]
async fn a_state_and_resource_pattern_receives_that_alarms_transition_into_that_state_alone() {
    let rows = [
        Row {
            case: "a pattern selecting the ALARM state, the target taking the envelope",
            selected: ALARM,
            expect: Expect::Envelope,
        },
        Row {
            case: "a pattern selecting the OK state, the target taking the envelope",
            selected: OK,
            expect: Expect::Envelope,
        },
        Row {
            case: "a pattern selecting the INSUFFICIENT_DATA state, the target taking the \
                   envelope",
            selected: INSUFFICIENT_DATA,
            expect: Expect::Envelope,
        },
        Row {
            case: "a pattern selecting the ALARM state, the target reshaping the event",
            selected: ALARM,
            expect: Expect::Reshaped,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator_with_a_bus();
        let watched = format!("watched-{index}");
        let other = format!("other-{index}");
        given_alarm(&emulator, &watched).await;
        given_alarm(&emulator, &other).await;
        let watched_arn = alarm_arn(&emulator, case, METRIC_ALARM, &watched).await;
        let queue_url = given_target_queue(&emulator, &format!("selective-{index}")).await;
        given_state_and_resource_rule(
            &emulator,
            case,
            &format!("selective-{index}"),
            &watched_arn,
            row.selected,
            &queue_url,
            &row.expect,
        )
        .await;

        // The near misses first: every state of the OTHER alarm — which the pattern's
        // `resources` exclude — and then the two states of the WATCHED alarm the pattern's
        // `detail.state.value` excludes, before the transition the rule owes.
        let walked = walk(row.selected);
        for state in &walked {
            set_alarm_state(&emulator, &other, state).await;
        }
        for state in &walked {
            set_alarm_state(&emulator, &watched, state).await;
        }
        // A transition reports the state it moved OUT OF, which for the one the rule matches is
        // the state the walk left the alarm in before its last force.
        let previous = walked[walked.len() - 2];

        let mut held: Vec<Value> = Vec::new();
        until_delivered(&emulator, &queue_url, 1, &mut held).await;
        over_the_quiet_watch(&emulator, &queue_url, &mut held).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly the {:?} transition of {watched:?} on the rule's \
             queue, got {held:?}",
            row.selected
        );
        let delivered = &held[0];

        match row.expect {
            Expect::Envelope => {
                for (member, pointer, expected) in [
                    ("the event's source", "/source", json!(SOURCE)),
                    (
                        "the event's detail-type",
                        "/detail-type",
                        json!(STATE_CHANGE_DETAIL_TYPE),
                    ),
                    ("the alarm the event is about", "/resources", json!([&watched_arn])),
                    ("the alarm's name", "/detail/alarmName", json!(&watched)),
                    ("the state moved into", "/detail/state/value", json!(row.selected)),
                    (
                        "the state moved out of",
                        "/detail/previousState/value",
                        json!(previous),
                    ),
                ] {
                    let actual = delivered.pointer(pointer).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        actual, expected,
                        "case {case:?}: expected {member} at {pointer} to be {expected}, got \
                         {actual} in {delivered}"
                    );
                }
                // Every transition carries WHY it happened, which is what a consumer reads off
                // one.
                let reason = delivered
                    .pointer("/detail/state/reason")
                    .cloned()
                    .unwrap_or(Value::Null);
                assert!(
                    reason.as_str().is_some_and(|text| !text.is_empty()),
                    "case {case:?}: expected the transition's reason at /detail/state/reason, \
                     got {reason} in {delivered}"
                );
            }
            Expect::Reshaped => {
                let expected = json!({
                    "alarm": &watched,
                    "from": previous,
                    "to": row.selected,
                });
                assert_eq!(
                    *delivered, expected,
                    "case {case:?}: expected the target's template filled from the event to be \
                     {expected}, got {delivered}"
                );
            }
        }
    }
}
