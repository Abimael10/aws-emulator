// SPDX-License-Identifier: CC0-1.0

//! An alarm's SYSTEMS MANAGER OPSITEM action is executed from EVERY action list an alarm
//! carries, and by every kind of alarm that carries one — through the WHOLE mounted service
//! set, the one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **the action list an alarm executes is the one belonging to the state it
//! transitioned INTO — "the actions to execute when this alarm transitions to the ALARM state
//! from any other state" (`PutMetricAlarmInput$AlarmActions`), and the same sentence for
//! `OKActions` and `InsufficientDataActions`, on `PutCompositeAlarm` as on `PutMetricAlarm` — so
//! an OpsItem action (`arn:aws:ssm:<region>:<account>:opsitem:<severity>[#CATEGORY=<category>]`)
//! named in any one of those lists has Systems Manager OpsCenter open the work item whenever the
//! alarm moves into that state, whichever kind of alarm names it and whether the alarm's own
//! evaluation computed the move or a caller forced it; the executing alarm's history accounts
//! for it with an item whose `HistoryItemType` is `Action` and whose `AlarmType` is the kind of
//! alarm that executed it. `ActionsEnabled` is the gate in front of all of it — "indicates
//! whether actions should be executed during any changes to the alarm state"
//! (`MetricAlarm$ActionsEnabled`) — so the same transition made after `DisableAlarmActions`
//! opens nothing and accounts for nothing, and `EnableAlarmActions` restores it for the next
//! one without opening the work item it withheld.**
//!
//! Two services have to agree for that to hold — CloudWatch executes the action, Systems Manager
//! holds the work item — and the create CloudWatch makes is an in-process call addressed the way
//! a client's own `ssm`-scoped call is addressed, so WHICH mounted model claims it is only
//! decidable with every service mounted. What the work item is asserted on is the OpsCenter read
//! a client uses (`DescribeOpsItems`), so an OpsItem reported here is one Systems Manager itself
//! holds.
//!
//! The sibling suite `cloudwatch_alarm_ssm_ops_item_action.rs` owns the wire walk and the
//! OpsCenter and history reads this suite reuses, and pins the rule for a METRIC alarm's
//! `AlarmActions` and `OKActions` over severities, categories and Regions. The rows here differ
//! from each other, and from that suite's, along the rest of what the rule generalises over:
//! which action LIST names the OpsItem action (so which state's transition fires it, including
//! the `INSUFFICIENT_DATA` one), which KIND of alarm carries it (a metric alarm and a composite
//! alarm, whose history item is reported under its own `AlarmType`), what MADE the transition
//! (a forced one, and one a composite alarm's own rule computed out of its child's state), the
//! severity and category the ARN names, and the Region the whole wiring lives in.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wire walk, the OpsCenter read and the alarm-history read come from the suites that own
// them rather than a second copy of each.
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{cw, router};
use crate::cloudwatch_alarm_ssm_ops_item_action::{
    action_items, history_data, ops_items_of, until_ops_items, until_recorded, OPS_ITEM_SOURCE,
};

/// The account every ARN of this emulator names.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The series every metric alarm of this suite watches, and which nothing publishes a datapoint
/// to: such an alarm's state is its own missing-data treatment's doing alone, so it cannot drift
/// under a row that is running.
const NAMESPACE: &str = "Custom/OpsItemActionLists";
const METRIC: &str = "Depth";

/// The modelled `StateValue` values (`SetAlarmStateInput$StateValue`: one of ALARM |
/// INSUFFICIENT_DATA | OK).
const ALARM: &str = "ALARM";
const OK: &str = "OK";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The modelled `AlarmType` values a history item is reported under, and the collections
/// `DescribeAlarms` reports each kind of alarm in.
const METRIC_ALARM: &str = "MetricAlarm";
const COMPOSITE_ALARM: &str = "CompositeAlarm";
const EVERY_KIND: &[&str] = &[METRIC_ALARM, COMPOSITE_ALARM];

/// How long a leg waits for a state the stored alarms imply. Generous on purpose: the states
/// waited for are owed within a span of the call that made them due, and nothing here is
/// asserted about how fast the host runs — the deadline only turns a failure into a failure
/// instead of a hang.
const DEADLINE: Duration = Duration::from_secs(120);

/// How often such a wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Which KIND of alarm carries the action, which decides how the transition into the row's state
/// is made and what the history item accounting for the execution is reported under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A metric alarm, moved into the row's state by `SetAlarmState`.
    Metric,
    /// A composite alarm, moved into the row's state by its own rule over a child alarm whose
    /// missing-data treatment decides that child's state.
    Composite,
}

impl Kind {
    /// The modelled `AlarmType` an alarm of this kind records its history under.
    fn alarm_type(self) -> &'static str {
        match self {
            Self::Metric => METRIC_ALARM,
            Self::Composite => COMPOSITE_ALARM,
        }
    }

    /// The collection `DescribeAlarms` reports an alarm of this kind in.
    fn collection(self) -> &'static str {
        match self {
            Self::Metric => "MetricAlarms",
            Self::Composite => "CompositeAlarms",
        }
    }
}

/// One row: the kind of alarm, the state whose action list names the OpsItem action, and what
/// the ARN says the work item carries.
struct Row {
    case: &'static str,
    region: &'static str,
    kind: Kind,
    /// The state whose action list names the action, and so the state whose transition fires it.
    state: &'static str,
    /// The severity the ARN names. "Severity options range from 1 to 4" (`OpsItem$Severity`).
    severity: &'static str,
    /// The category the ARN names, when it names one. "Category options include: Availability,
    /// Cost, Performance, Recovery, Security" (`OpsItem$Category`).
    category: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a metric alarm's INSUFFICIENT_DATA list, fired by a forced transition",
            region: "us-west-2",
            kind: Kind::Metric,
            state: INSUFFICIENT_DATA,
            severity: "2",
            category: Some("Performance"),
        },
        Row {
            case: "a composite alarm's ALARM list, fired by its own rule over a breaching child",
            region: "eu-west-1",
            kind: Kind::Composite,
            state: ALARM,
            severity: "1",
            category: None,
        },
        Row {
            case: "a composite alarm's OK list, in another Region",
            region: "us-east-1",
            kind: Kind::Composite,
            state: OK,
            severity: "4",
            category: Some("Recovery"),
        },
    ]
}

/// The other state a row's alarm is moved through, so that the move INTO the row's state is a
/// state CHANGE whatever the alarm was left in. It fires nothing itself: only the row's own list
/// names an action.
fn other_state(state: &str) -> &'static str {
    if state == OK {
        ALARM
    } else {
        OK
    }
}

/// The OpsItem action ARN of this row, in the form `PutMetricAlarmInput$AlarmActions` documents
/// for a Systems Manager action.
fn action_arn(row: &Row) -> String {
    let region = row.region;
    let severity = row.severity;
    match row.category {
        Some(category) => {
            format!("arn:aws:ssm:{region}:{ACCOUNT}:opsitem:{severity}#CATEGORY={category}")
        }
        None => format!("arn:aws:ssm:{region}:{ACCOUNT}:opsitem:{severity}"),
    }
}

/// The three action lists of an alarm, with `action` in the one belonging to `state` and the
/// other two empty — so WHICH list a transition executed is decided by the state it moved into.
fn action_lists(action: &str, state: &str) -> (Value, Value, Value) {
    let named = json!([action]);
    let none = json!([]);
    match state {
        ALARM => (named, none.clone(), none),
        OK => (none.clone(), named, none),
        _ => (none.clone(), none, named),
    }
}

/// Write the metric alarm of a row: a static-threshold alarm over a series nothing publishes to,
/// whose `TreatMissingData` keeps whatever state it is in — "ignore — the current alarm state is
/// maintained" (`PutMetricAlarmInput$TreatMissingData`) — so the only state changes it makes are
/// the ones a row forces.
async fn put_metric_alarm(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    alarm: &str,
    action: &str,
    state: &str,
) {
    let (alarm_actions, ok_actions, insufficient_data_actions) = action_lists(action, state);
    cw(
        router,
        case,
        region,
        "PutMetricAlarm",
        &json!({
            "AlarmName": alarm,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "Alarm", "Value": alarm}],
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "ignore",
            "ActionsEnabled": true,
            "AlarmActions": alarm_actions,
            "OKActions": ok_actions,
            "InsufficientDataActions": insufficient_data_actions,
        }),
    )
    .await;
}

/// Write the child of a composite row: the alarm whose missing-data treatment alone decides the
/// state the row's rule reads ("breaching" puts it in ALARM, "notBreaching" in OK,
/// `PutMetricAlarmInput$TreatMissingData`). It carries no action of its own.
async fn put_child_alarm(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    child: &str,
    treat_missing: &str,
) {
    cw(
        router,
        case,
        region,
        "PutMetricAlarm",
        &json!({
            "AlarmName": child,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "Child", "Value": child}],
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "DatapointsToAlarm": 1,
            "Threshold": 50.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": treat_missing,
        }),
    )
    .await;
}

/// Write the composite alarm of a row over `rule`, with the OpsItem action in the list belonging
/// to the row's state.
async fn put_composite_alarm(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    alarm: &str,
    rule: &str,
    action: &str,
    state: &str,
) {
    let (alarm_actions, ok_actions, insufficient_data_actions) = action_lists(action, state);
    cw(
        router,
        case,
        region,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": alarm,
            "AlarmRule": rule,
            "ActionsEnabled": true,
            "AlarmActions": alarm_actions,
            "OKActions": ok_actions,
            "InsufficientDataActions": insufficient_data_actions,
        }),
    )
    .await;
}

/// Force one alarm into `state`, which is what executes the actions configured for it.
async fn force(router: &MultiServiceRouter, case: &str, region: &str, alarm: &str, state: &str) {
    cw(
        router,
        case,
        region,
        "SetAlarmState",
        &json!({
            "AlarmName": alarm,
            "StateValue": state,
            "StateReason": "driven by the test",
        }),
    )
    .await;
}

/// Turn the actions of one alarm on or off, whatever kind of alarm it is.
async fn switch_actions(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    alarm: &str,
    enabled: bool,
) {
    let operation = if enabled {
        "EnableAlarmActions"
    } else {
        "DisableAlarmActions"
    };
    cw(
        router,
        case,
        region,
        operation,
        &json!({"AlarmNames": [alarm]}),
    )
    .await;
}

/// What `DescribeAlarms` reports for `alarm` once it reports `wanted` as its state, waiting for
/// that CONDITION rather than for a span.
///
/// Both kinds are asked for: "If you omit this parameter, only metric alarms are returned, even
/// if composite alarms or log alarms exist in the account."
/// (`DescribeAlarmsInput$AlarmTypes`)
async fn until_state(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    kind: Kind,
    alarm: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cw(
            router,
            case,
            region,
            "DescribeAlarms",
            &json!({"AlarmNames": [alarm], "AlarmTypes": EVERY_KIND}),
        )
        .await;
        let reported = described[kind.collection()][0].clone();
        if reported["StateValue"].as_str() == Some(wanted) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {alarm} to reach {wanted:?}, got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Put the row's alarm into the state whose action list names the OpsItem action: forced for a
/// metric alarm, computed by its own rule for a composite one.
async fn given_alarm_in_its_state(
    router: &MultiServiceRouter,
    row: &Row,
    alarm: &str,
    action: &str,
) {
    let (case, region) = (row.case, row.region);
    match row.kind {
        Kind::Metric => {
            put_metric_alarm(router, case, region, alarm, action, row.state).await;
            // A created alarm starts in INSUFFICIENT_DATA and its treatment keeps it there, so
            // the move through the other state is what makes the move into the row's state a
            // state CHANGE for every row.
            force(router, case, region, alarm, other_state(row.state)).await;
            until_state(router, case, region, row.kind, alarm, other_state(row.state)).await;
            force(router, case, region, alarm, row.state).await;
        }
        Kind::Composite => {
            let child = format!("{alarm}-child");
            // The child settles in the state the row's rule needs, and is WAITED for: the
            // composite is written over a child that is already there, so the rule's first
            // value is the row's state and the alarm makes exactly one transition.
            let (treat_missing, settles_in) = if row.state == ALARM {
                ("breaching", ALARM)
            } else {
                ("notBreaching", OK)
            };
            put_child_alarm(router, case, region, &child, treat_missing).await;
            until_state(router, case, region, Kind::Metric, &child, settles_in).await;
            let rule = format!("ALARM(\"{child}\")");
            put_composite_alarm(router, case, region, alarm, &rule, action, row.state).await;
        }
    }
    until_state(router, case, region, row.kind, alarm, row.state).await;
}

/// RULE: an OpsItem action opens the work item on the transition into the state whose list names
/// it, whichever list and whichever kind of alarm that is, and `ActionsEnabled` gates it.
#[tokio::test]
async fn an_ops_item_action_fires_from_the_list_of_the_state_the_alarm_moved_into() {
    for (index, row) in rows().iter().enumerate() {
        let (case, region, kind) = (row.case, row.region, row.kind);
        // One emulator per row, assembled the way the binary assembles it: what a row reads in
        // OpsCenter can then only be its own.
        let router = router();
        let alarm = format!("opsitem-lists-{index}");
        let action = action_arn(row);

        given_alarm_in_its_state(&router, row, &alarm, &action).await;

        // The alarm accounts for the execution: ONE item, of the type a record of an executed
        // action carries, reported under the kind of alarm that executed it.
        let items = until_recorded(&router, case, region, &alarm, EVERY_KIND, 1).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: the alarm must account for exactly one action execution, got {items:?}"
        );
        let recorded = &items[0];
        assert_eq!(
            recorded["HistoryItemType"].as_str(),
            Some("Action"),
            "case {case:?}: the record of an executed action is an Action item, got {recorded}"
        );
        assert_eq!(
            recorded["AlarmType"].as_str(),
            Some(kind.alarm_type()),
            "case {case:?}: the item must be reported under the kind of alarm that executed the \
             action, got {recorded}"
        );
        assert_eq!(
            recorded["HistorySummary"].as_str(),
            Some(format!("Successfully executed action {action}").as_str()),
            "case {case:?}: expected the action to be recorded as executed, got {recorded}"
        );
        let data = history_data(recorded);
        assert_eq!(
            data["actionState"],
            json!("Succeeded"),
            "case {case:?}: expected the recorded action to have succeeded, got {data}"
        );
        assert_eq!(
            data["stateValue"],
            json!(row.state),
            "case {case:?}: the record must name the state whose list was executed, got {data}"
        );
        assert_eq!(
            data["notificationResource"],
            json!(action),
            "case {case:?}: the record must name the action that was executed, got {data}"
        );

        // And OpsCenter holds the work item the action opened, carrying what the ARN named.
        let opened = until_ops_items(&router, case, region, &alarm, 1).await;
        assert_eq!(
            opened.len(),
            1,
            "case {case:?}: the action must open exactly one OpsItem, got {opened:?}"
        );
        let item = &opened[0];
        assert_eq!(
            item["Title"].as_str(),
            Some(alarm.as_str()),
            "case {case:?}: the OpsItem must be titled for the alarm that raised it, got {item}"
        );
        assert_eq!(
            item["Source"].as_str(),
            Some(OPS_ITEM_SOURCE),
            "case {case:?}: the OpsItem must carry the origin OpsCenter records for an \
             alarm-raised work item, got {item}"
        );
        assert_eq!(
            item["Severity"].as_str(),
            Some(row.severity),
            "case {case:?}: the OpsItem must carry the severity the action named, got {item}"
        );
        assert_eq!(
            item["Category"].as_str(),
            row.category,
            "case {case:?}: the OpsItem must carry the category the action named, got {item}"
        );

        // The gate: with the switch off the same transition is made and nothing is executed.
        // The execution is part of the call that materialises the transition, so what it did
        // not do is read as soon as that call has answered.
        switch_actions(&router, case, region, &alarm, false).await;
        force(&router, case, region, &alarm, other_state(row.state)).await;
        force(&router, case, region, &alarm, row.state).await;
        let held = ops_items_of(&router, case, region, &alarm).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: no work item may be opened while the actions are off, got {held:?}"
        );
        let suppressed = action_items(&router, case, region, &alarm, EVERY_KIND).await;
        assert_eq!(
            suppressed.len(),
            1,
            "case {case:?}: a suppressed action is no execution to account for, got \
             {suppressed:?}"
        );

        // And back on: the switch was what withheld it, so the next transition into the same
        // state opens the work item again — and the withheld one is not opened late.
        switch_actions(&router, case, region, &alarm, true).await;
        force(&router, case, region, &alarm, other_state(row.state)).await;
        force(&router, case, region, &alarm, row.state).await;
        let reopened = until_ops_items(&router, case, region, &alarm, 2).await;
        assert_eq!(
            reopened.len(),
            2,
            "case {case:?}: the transition made with the actions back on must open the work \
             item, and only one more, got {reopened:?}"
        );
        let accounted = until_recorded(&router, case, region, &alarm, EVERY_KIND, 2).await;
        assert_eq!(
            accounted.len(),
            2,
            "case {case:?}: the alarm must account for the second execution and no other, got \
             {accounted:?}"
        );
    }
}
