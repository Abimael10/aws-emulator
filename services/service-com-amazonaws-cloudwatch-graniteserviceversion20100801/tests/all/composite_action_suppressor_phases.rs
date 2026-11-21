// SPDX-License-Identifier: CC0-1.0

//! The ACTIONS SUPPRESSOR of a composite alarm, end to end over the wire with CloudWatch, SNS
//! and SQS on one engine — so what a client sees is what is asserted: the phase
//! `DescribeAlarms` reports, the account `DescribeAlarmHistory` keeps of the actions that phase
//! withheld, and whether a notification reaches the queue subscribed to the action's topic.
//!
//! The RULE asserted here: **a composite alarm that names an `ActionsSuppressor` withholds the
//! actions of its transitions while a suppression is in force and REPORTS which of the modelled
//! reasons is withholding them (`CompositeAlarm$ActionsSuppressedBy`, whose values are
//! `WaitPeriod`, `ExtensionPeriod` and `Alarm`) — `WaitPeriod` from the instant the alarm goes
//! into `ALARM` with its suppressor not yet in that state, for at most
//! `ActionsSuppressorWaitPeriod` seconds; `Alarm` while the suppressor is in `ALARM`;
//! `ExtensionPeriod` for at most `ActionsSuppressorExtensionPeriod` seconds after the suppressor
//! leaves it; and no reason at all once the period in force has passed, at which point "the
//! composite alarm performs its actions". A suppressed transition executes none of the alarm's
//! actions and accounts for that in its own history with an `Action` item whose `actionState` is
//! `Suppressed`, while an otherwise identical composite carrying no suppressor delivers.**
//!
//! The rows differ along what that rule generalises over: which of the modelled reasons is in
//! force (each of the three, in the order the suppressor's own state takes them), which of the
//! two configured periods runs out, and whether the alarm carries a suppressor at all.
//!
//! Every phase and every delivery is AWAITED on the condition rather than on a span: a
//! suppression ends at an instant the configured period fixes, and the delivery it releases
//! follows it.

use std::time::Instant;

use serde_json::{json, Value};

use crate::harness::{
    emulator, given_topic_with_subscriber, rpc, set_alarm_state, Emulator, LOOK_INTERVAL,
    QUIET_WATCH, WORK_WAIT,
};

/// The modelled alarm states this suite names.
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The alarms every case creates: the child whose state the composite's rule reads, the alarm it
/// names as its actions suppressor, the suppressed composite and its unsuppressed twin.
const CHILD: &str = "suppressor-child";
const SUPPRESSOR: &str = "maintenance-window";
const SUPPRESSED: &str = "suppressed-composite";
const CONTROL: &str = "control-composite";

/// A period long enough that no phase of a case that only READS the phases can run out while it
/// is reading them, in the seconds `ActionsSuppressorWaitPeriod` and
/// `ActionsSuppressorExtensionPeriod` are counted in.
const LONG_PERIOD: i64 = 600;

/// A period short enough for a case that waits for one to RUN OUT to see it happen.
const SHORT_PERIOD: i64 = 3;

/// One CloudWatch call, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        "monitoring",
        "GraniteServiceVersion20100801",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call, requiring a 200.
async fn sqs(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "sqs", "AmazonSQS", operation, body).await
}

/// Create the two metric alarms a case drives by hand: a composite alarm reads its children's
/// and its suppressor's STATES, so the series behind them is never read here, and
/// `TreatMissingData` `ignore` ("the current alarm state is maintained") keeps each alarm in the
/// state the case put it in once its own evaluation comes due.
async fn given_metric_alarm(emulator: &Emulator, name: &str) {
    cw(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Custom/Suppressor",
            "MetricName": "Depth",
            "Dimensions": [{"Name": "Alarm", "Value": name}],
            "Statistic": "Maximum",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "ignore",
        }),
    )
    .await;
    set_alarm_state(emulator, name, OK).await;
}

/// Create the suppressed composite and its unsuppressed twin, both firing `topic` on ALARM over
/// the same rule, the suppressed one configured with the two periods given.
async fn given_composites(emulator: &Emulator, topic: &str, wait: i64, extension: i64) {
    let rule = format!("ALARM(\"{CHILD}\")");
    cw(
        emulator,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": SUPPRESSED,
            "AlarmRule": rule,
            "AlarmActions": [topic],
            "ActionsSuppressor": SUPPRESSOR,
            "ActionsSuppressorWaitPeriod": wait,
            "ActionsSuppressorExtensionPeriod": extension,
        }),
    )
    .await;
    cw(
        emulator,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": CONTROL,
            "AlarmRule": rule,
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// The composite alarm `name`, as `DescribeAlarms` reports it: "If you specify CompositeAlarms,
/// the operation returns only a list of composite alarms" (`DescribeAlarmsInput$AlarmTypes`).
async fn composite(emulator: &Emulator, name: &str) -> Value {
    cw(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["CompositeAlarm"]}),
    )
    .await["CompositeAlarms"][0]
        .clone()
}

/// Look at `name` until it reports `wanted` as what withholds its actions, and answer the alarm
/// as it then stood — a wait for the CONDITION, with a deadline only so a failure is a failure
/// rather than a hang.
async fn until_suppressed_by(emulator: &Emulator, case: &str, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let alarm = composite(emulator, name).await;
        if alarm["ActionsSuppressedBy"].as_str() == Some(wanted) {
            return alarm;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must report ActionsSuppressedBy {wanted:?}; it last reported \
             {alarm}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look at `name` until its state is `wanted`.
async fn until_composite_state(emulator: &Emulator, case: &str, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let alarm = composite(emulator, name).await;
        if alarm["StateValue"].as_str() == Some(wanted) {
            return alarm;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must reach {wanted}; it last reported {alarm}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Take every notification waiting on the queue at `url` as the alarm document SNS enveloped,
/// deleting what it hands over so a later look reads only what arrived since.
async fn take(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut taken = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("case {case:?}: not the SNS envelope ({error}): {body}"));
        let inner = envelope["Message"].as_str().unwrap_or_default();
        taken.push(serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: not an alarm notification ({error}): {inner}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    taken
}

/// Watch the queue for [`QUIET_WATCH`], answering every notification that arrived over it: how a
/// case shows a delivery it expects NOT to be made, and one it expects to be.
async fn over_the_watch(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + QUIET_WATCH;
    let mut arrived = Vec::new();
    while Instant::now() < deadline {
        arrived.extend(take(emulator, case, url).await);
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    arrived
}

/// Look at the queue until a notification naming `name` has arrived, answering EVERY
/// notification taken while waiting for it — so a delivery the case expects NOT to be made is
/// seen rather than swallowed by the wait for the one it does expect.
async fn until_notified_about(
    emulator: &Emulator,
    case: &str,
    url: &str,
    name: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + WORK_WAIT;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(take(emulator, case, url).await);
        if arrived
            .iter()
            .any(|notification| notification["AlarmName"].as_str() == Some(name))
        {
            return arrived;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: a notification naming {name} must reach the queue, got {arrived:?}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The alarms the notifications `arrived` name, in the order they arrived.
fn named(arrived: &[Value]) -> Vec<&str> {
    arrived
        .iter()
        .filter_map(|notification| notification["AlarmName"].as_str())
        .collect()
}

/// The `actionState` of every `Action` history item recorded for the composite alarm `name`,
/// newest first: the account a client reads of what became of each transition's actions.
async fn action_states(emulator: &Emulator, case: &str, name: &str) -> Vec<String> {
    let body = cw(
        emulator,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "AlarmTypes": ["CompositeAlarm"],
            "HistoryItemType": "Action",
        }),
    )
    .await;
    body["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let data = item["HistoryData"].as_str().unwrap_or_default();
            let document: Value = serde_json::from_str(data).unwrap_or_else(|error| {
                panic!("case {case:?}: HistoryData is not JSON ({error}): {data}")
            });
            document["actionState"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Look at the history of `name` until an `Action` item reports `wanted`, answering every state
/// it then held.
async fn until_action_state(
    emulator: &Emulator,
    case: &str,
    name: &str,
    wanted: &str,
) -> Vec<String> {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let states = action_states(emulator, case, name).await;
        if states.iter().any(|state| state == wanted) {
            return states;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: an Action item of {name} must report actionState {wanted:?}; it last \
             held {states:?}"
        );
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The runtime flavour the emulator boots on, so the service's own alarm clock really wakes up
/// between the drives rather than only when the test awaits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suppressed_composite_reports_the_phase_withholding_its_actions() {
    let emulator = emulator();
    let (topic, queue) = given_topic_with_subscriber(&emulator, "suppressor-phases").await;
    given_metric_alarm(&emulator, CHILD).await;
    given_metric_alarm(&emulator, SUPPRESSOR).await;
    given_composites(&emulator, &topic, LONG_PERIOD, LONG_PERIOD).await;

    // The child completes the rule, so both composites go into ALARM — the suppressed one with
    // its suppressor still in OK, which is the instant its wait period is measured from.
    set_alarm_state(&emulator, CHILD, ALARM).await;

    // (case, the state the suppressor is forced into before the look, the reason the suppressed
    //  composite must then report)
    let steps: &[(&str, Option<&str>, &str)] = &[
        (
            "the composite goes into ALARM before its suppressor does",
            None,
            "WaitPeriod",
        ),
        (
            "the suppressor confirms the ALARM state",
            Some(ALARM),
            "Alarm",
        ),
        (
            "the suppressor leaves the ALARM state again",
            Some(OK),
            "ExtensionPeriod",
        ),
    ];
    for (case, forced, wanted) in steps {
        if let Some(state) = forced {
            set_alarm_state(&emulator, SUPPRESSOR, state).await;
        }
        let alarm = until_suppressed_by(&emulator, case, SUPPRESSED, wanted).await;
        assert_eq!(
            alarm["StateValue"].as_str(),
            Some(ALARM),
            "case {case:?}: the suppression must not change the alarm's own state, got {alarm}"
        );
        // " Captures the reason for action suppression. " (`CompositeAlarm$ActionsSuppressedReason`)
        // — a phase that withholds the actions says why, and names the alarm it waits on.
        let reason = alarm["ActionsSuppressedReason"].as_str().unwrap_or_default();
        assert!(
            reason.contains(SUPPRESSOR),
            "case {case:?}: the reason must name the suppressor alarm, got {reason:?}"
        );
    }

    // The unsuppressed twin is the control: it carries no suppressor, so it reports no reason
    // and its own notification is delivered — which is also what shows that the suppressed
    // alarm's absent delivery is the suppression rather than a dispatch that never ran.
    let control = until_composite_state(&emulator, "the unsuppressed twin", CONTROL, ALARM).await;
    assert_eq!(
        control["ActionsSuppressedBy"],
        Value::Null,
        "case \"the unsuppressed twin\": an alarm with no suppressor must report no reason, got \
         {control}"
    );
    // The control's delivery is awaited on the CONDITION, and the watch that follows it is what
    // shows the suppressed alarm delivered nothing: both transitions happened before the
    // control's notification arrived, so a queue holding only the control's is a queue the
    // suppressed one never reached.
    let mut delivered =
        until_notified_about(&emulator, "the unsuppressed twin", &queue, CONTROL).await;
    delivered.extend(over_the_watch(&emulator, "the unsuppressed twin", &queue).await);
    assert_eq!(
        named(&delivered),
        vec![CONTROL],
        "case \"the unsuppressed twin\": only the alarm with no suppressor may deliver, got \
         {delivered:?}"
    );

    // The account of the actions the suppression withheld, which is where a client reads why it
    // was not paged — and no record of an execution, because none was performed.
    let states = until_action_state(&emulator, "the withheld actions", SUPPRESSED, "Suppressed")
        .await;
    assert!(
        !states
            .iter()
            .any(|state| state == "Succeeded" || state == "Failed"),
        "case \"the withheld actions\": a suppressed transition executes no action, got \
         {states:?}"
    );
}

/// One case of the release: which configured period runs out, and the drives that open it.
struct Release {
    /// What this case is, for every assertion message.
    case: &'static str,
    /// The alarm's `ActionsSuppressorWaitPeriod`, in seconds.
    wait: i64,
    /// Its `ActionsSuppressorExtensionPeriod`, in seconds.
    extension: i64,
    /// The states the suppressor is driven through, each awaited as the phase it puts the
    /// composite alarm in, before the case waits for the release.
    drives: &'static [(&'static str, &'static str)],
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_period_that_runs_out_releases_the_actions_it_withheld() {
    // The rows differ along which of the two configured periods runs out — the one the alarm
    // waits for its suppressor in, and the one it waits in after the suppressor has left ALARM.
    let rows: &[Release] = &[
        Release {
            case: "the wait period runs out",
            wait: SHORT_PERIOD,
            extension: LONG_PERIOD,
            drives: &[],
        },
        Release {
            case: "the extension period runs out",
            wait: LONG_PERIOD,
            extension: SHORT_PERIOD,
            drives: &[(ALARM, "Alarm"), (OK, "ExtensionPeriod")],
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = emulator();
        let (topic, queue) = given_topic_with_subscriber(&emulator, "suppressor-release").await;
        given_metric_alarm(&emulator, CHILD).await;
        given_metric_alarm(&emulator, SUPPRESSOR).await;
        given_composites(&emulator, &topic, row.wait, row.extension).await;

        set_alarm_state(&emulator, CHILD, ALARM).await;
        until_composite_state(&emulator, case, SUPPRESSED, ALARM).await;
        for (state, phase) in row.drives {
            set_alarm_state(&emulator, SUPPRESSOR, state).await;
            until_suppressed_by(&emulator, case, SUPPRESSED, phase).await;
        }

        // "After this time, the composite alarm performs its actions."
        // (`CompositeAlarm$ActionsSuppressorWaitPeriod` /
        // `CompositeAlarm$ActionsSuppressorExtensionPeriod`)
        let delivered = until_notified_about(&emulator, case, &queue, SUPPRESSED).await;
        let notification = delivered
            .iter()
            .find(|notification| notification["AlarmName"].as_str() == Some(SUPPRESSED))
            .unwrap_or_else(|| panic!("case {case:?}: {delivered:?} names no {SUPPRESSED}"));
        assert_eq!(
            notification["NewStateValue"].as_str(),
            Some(ALARM),
            "case {case:?}: the released notification must carry the state whose actions were \
             withheld, got {notification}"
        );

        // Nothing suppresses the actions any more, and the history accounts for the withholding
        // and for the execution that followed it.
        let alarm = composite(&emulator, SUPPRESSED).await;
        assert_eq!(
            alarm["ActionsSuppressedBy"],
            Value::Null,
            "case {case:?}: a period that has run out must leave no reason reported, got {alarm}"
        );
        let states = until_action_state(&emulator, case, SUPPRESSED, "Succeeded").await;
        assert!(
            states.contains(&"Suppressed".to_string()),
            "case {case:?}: the history must still account for the actions the suppression \
             withheld, got {states:?}"
        );
    }
}
