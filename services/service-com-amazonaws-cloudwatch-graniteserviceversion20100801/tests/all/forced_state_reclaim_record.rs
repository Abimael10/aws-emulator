// SPDX-License-Identifier: CC0-1.0

//! The STATE RECORD either writer of a metric alarm's state leaves behind, over a full force and
//! reclaim cycle — end to end with CloudWatch, SNS and SQS on one engine, so the record is read
//! where a client reads it (`DescribeAlarms`) and each transition's notification where a client
//! sees it (the queue subscribed to the alarm's action topic).
//!
//! The rule: an alarm's state record — `StateValue`, `StateReason`, `StateReasonData` and
//! `StateUpdatedTimestamp` — belongs to the writer that last decided the state, and
//! `SetAlarmState` decides it only TEMPORARILY. "Temporarily sets the state of an alarm for
//! testing purposes. When the updated state differs from the previous value, the action configured
//! for the appropriate state is invoked. […] Metric alarms returns to their actual state quickly,
//! often within seconds." (`smithy_query(query="operation",operation="SetAlarmState")`) — so the
//! force REPLACES the record with the caller's own state and reason (and the decision record the
//! caller sent, if any), while the alarm's next evaluation of the stored datapoints RE-AUTHORS the
//! whole record: the state those datapoints decide, an explanation naming the datapoints it graded
//! and the threshold it compared them against, a decision record queried at a later instant, and a
//! later `StateUpdatedTimestamp`. Each of the two transitions is a state change, so each invokes
//! the actions of the state it moves to, reporting the state it moved out of.
//!
//! The rows differ along what the rule generalises over: which state is forced (a different one of
//! the modelled `StateValue` members per row,
//! `smithy_query(query="shape",shape="StateValue")`), which state the published series decides —
//! so which of the three action lists the reclaim fires and which direction the explanation's
//! closing clause names — the alarm's period, and whether the caller hands the force a
//! `StateReasonData` of its own.

use std::time::Instant;

use serde_json::{json, Value};

use crate::harness::{
    alarm_stamp, document_instant_secs, emulator, given_topic_with_subscriber,
    graded_window_stamp, rpc, Emulator, LOOK_INTERVAL, TARGET_PREFIX, WORK_WAIT,
};

/// The signing name CloudWatch's own requests are scoped by.
const CLOUDWATCH: &str = "monitoring";

/// The SQS signing name and target prefix, for `rpc`.
const SQS: (&str, &str) = ("sqs", "AmazonSQS");

/// The namespace every alarm in this suite watches.
const NAMESPACE: &str = "Custom/ForcedReclaimRecord";

/// The metric every alarm in this suite watches.
const METRIC: &str = "Faults";

/// The threshold every alarm in this suite compares its datapoints against.
const THRESHOLD: f64 = 50.0;

/// The three modelled `StateValue` members (`smithy_query(query="shape",shape="StateValue")`).
const OK: &str = "OK";
const ALARM: &str = "ALARM";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The reason text the caller forces a state with: text no evaluation would ever author, so a
/// record still reading this way is one nothing re-derived.
const FORCED_REASON: &str = "forced by the test, no datapoint decided it";

/// One row of the table: the cycle it drives and the record each leg must leave behind.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// What the row is, named in every assertion it makes.
    case: &'static str,
    /// The alarm's `Period`, which is also the span its forced state stands for.
    period: u32,
    /// The value published into the series the alarm watches, which decides [`Row::evaluated`]
    /// against [`THRESHOLD`].
    value: f64,
    /// The state the published series decides: the alarm's state before the force and the one its
    /// own evaluation reclaims it to.
    evaluated: &'static str,
    /// The state the caller forces, which is never the state the series decides.
    forced: &'static str,
    /// The `StateReasonData` the caller hands `SetAlarmState`, where the row sends one.
    forced_reason_data: Option<&'static str>,
}

/// Call one CloudWatch operation, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH, TARGET_PREFIX, operation, body).await
}

/// The `PutMetricAlarm` member the actions of `state` are configured on: "The actions to execute
/// when this alarm transitions to the ALARM state" / "to the OK state" / "to the
/// INSUFFICIENT_DATA state" (`PutMetricAlarmInput$AlarmActions` / `$OKActions` /
/// `$InsufficientDataActions`).
fn action_member(state: &str) -> &'static str {
    match state {
        ALARM => "AlarmActions",
        OK => "OKActions",
        _ => "InsufficientDataActions",
    }
}

/// The alarm a row configures: one evaluation period over one series, with the topic whose queue
/// the row watches for the FORCED transition on the forced state's action list and the topic it
/// watches for the RECLAIM on the evaluated state's list.
fn alarm_request(row: &Row, name: &str, forced_topic: &str, reclaim_topic: &str) -> Value {
    let mut request = json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": [{"Name": "Row", "Value": name}],
        "Statistic": "Maximum",
        "Period": row.period,
        "EvaluationPeriods": 1,
        "DatapointsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "ignore",
    });
    request[action_member(row.forced)] = json!([forced_topic]);
    request[action_member(row.evaluated)] = json!([reclaim_topic]);
    request
}

/// Publish one datapoint of the series the alarm `name` watches, stamped inside the newest window
/// an evaluation of that alarm grades ([`graded_window_stamp`]).
async fn publish(emulator: &Emulator, row: &Row, name: &str) {
    cw(
        emulator,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": METRIC,
                "Dimensions": [{"Name": "Row", "Value": name}],
                "Value": row.value,
                "Timestamp": graded_window_stamp(f64::from(row.period)),
            }],
        }),
    )
    .await;
}

/// The whole state record `DescribeAlarms` reports for one metric alarm.
#[derive(Debug, Clone)]
struct Record {
    /// `StateValue`, as reported, or the empty string where the alarm carries none.
    state: String,
    /// `StateReason`, as reported, or the empty string where the alarm carries none.
    reason: String,
    /// `StateReasonData`, parsed, or `None` where the alarm carries none.
    reason_data: Option<Value>,
    /// `StateUpdatedTimestamp`, as the epoch seconds the awsJson protocols carry it in.
    updated_secs: Option<f64>,
}

impl Record {
    /// The instant the decision record says the series was queried at, for a record that carries
    /// one.
    fn query_date_secs(&self) -> Option<f64> {
        self.reason_data
            .as_ref()?
            .get("queryDate")?
            .as_str()
            .and_then(document_instant_secs)
    }
}

/// The state record of the alarm `name`, as a client reads it.
async fn record(emulator: &Emulator, name: &str) -> Record {
    let described = cw(emulator, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    let alarm = described["MetricAlarms"][0].clone();
    Record {
        state: text(&alarm, "StateValue"),
        reason: text(&alarm, "StateReason"),
        reason_data: alarm
            .get("StateReasonData")
            .and_then(Value::as_str)
            .map(|raw| {
                serde_json::from_str::<Value>(raw).unwrap_or_else(|error| {
                    panic!("StateReasonData must be a JSON document ({error}): {raw}")
                })
            }),
        updated_secs: alarm_stamp(&alarm, "StateUpdatedTimestamp"),
    }
}

/// One string member of a reported alarm, or the empty string where it carries none.
fn text(alarm: &Value, member: &str) -> String {
    alarm
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every alarm notification waiting on the queue at `url`, received AND deleted.
///
/// A received message a consumer does not delete becomes visible again once its visibility
/// timeout runs out ("If you don't delete the message, it is eventually redelivered",
/// `ReceiveMessage`), so a later look would count a notification this one already took.
async fn drain(emulator: &Emulator, url: &str) -> Vec<Value> {
    let received = rpc(
        emulator,
        SQS.0,
        SQS.1,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut found = Vec::new();
    for message in &messages {
        if let Some(receipt) = message["ReceiptHandle"].as_str() {
            rpc(
                emulator,
                SQS.0,
                SQS.1,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": receipt}),
            )
            .await;
        }
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body)
            .unwrap_or_else(|error| panic!("the body must be the SNS envelope ({error}): {body}"));
        let inner = envelope["Message"].as_str().unwrap_or_default();
        found.push(serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("the message must be an alarm notification ({error}): {inner}")
        }));
    }
    found
}

/// Look at the queue at `url` until it has held at least one notification, taking what it holds
/// into `held` — a wait for the DOCUMENT, not for a span.
async fn until_held(emulator: &Emulator, url: &str, held: &mut Vec<Value>) {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        held.extend(drain(emulator, url).await);
        if !held.is_empty() || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look at the alarm `name` until it reports `wanted`, keeping its series published meanwhile and
/// taking whatever the queue at `url` holds into `held`. Answers the last record read — a wait for
/// the RECORD, not for a span.
async fn until_reported(
    emulator: &Emulator,
    row: &Row,
    name: &str,
    wanted: &str,
    url: &str,
    held: &mut Vec<Value>,
) -> Record {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        publish(emulator, row, name).await;
        let seen = record(emulator, name).await;
        held.extend(drain(emulator, url).await);
        if seen.state == wanted || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The parts the explanation an EVALUATION authors must carry for the state it moved the alarm to:
/// the count of datapoints that read the way that state's own direction needs out of the periods
/// the alarm evaluates, the datapoints themselves, the comparison as that direction reads it with
/// the threshold they were compared against, and the closing clause naming the direction and its
/// minimum.
fn evaluated_reason_parts(row: &Row) -> Vec<String> {
    let comparison = if row.evaluated == ALARM {
        "was greater than"
    } else {
        "was not greater than"
    };
    let direction = if row.evaluated == ALARM {
        format!("{OK} -> {ALARM}")
    } else {
        format!("{ALARM} -> {OK}")
    };
    vec![
        "Threshold Crossed: 1 out of the last 1 datapoints [".to_string(),
        format!("{:.1} (", row.value),
        format!("{comparison} the threshold ({THRESHOLD:.1})"),
        format!("(minimum 1 datapoint for {direction} transition)."),
    ]
}

/// The one notification a leg of a row is owed, with the leg named in the failure.
fn only_notification(case: &str, leg: &str, held: &[Value]) -> Value {
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: {leg} must publish exactly one notification, got {held:?}"
    );
    held[0].clone()
}

#[tokio::test]
async fn the_evaluation_re_authors_the_record_a_forced_state_left_and_fires_its_own_transition() {
    let rows = [
        // The recorded scenario: a clear series, a forced ALARM, and the evaluation taking the
        // alarm back to OK — so the reclaim fires the OK actions.
        Row {
            case: "a forced ALARM over a clear series",
            period: 10,
            value: 6.0,
            evaluated: OK,
            forced: ALARM,
            forced_reason_data: None,
        },
        // The other direction: a breaching series, a forced OK, and the evaluation taking the
        // alarm back to ALARM — so the reclaim fires the ALARM actions. The caller hands this
        // force a decision record of its own, which the alarm then carries verbatim.
        Row {
            case: "a forced OK over a breaching series, with the caller's own reason data",
            period: 10,
            value: 99.0,
            evaluated: ALARM,
            forced: OK,
            forced_reason_data: Some(r#"{"reason":"a scaling policy reads this"}"#),
        },
        // The third modelled state, on an alarm of another period — so the span the force stands
        // for is that alarm's own.
        Row {
            case: "a forced INSUFFICIENT_DATA over a clear series of a longer period",
            period: 20,
            value: 6.0,
            evaluated: OK,
            forced: INSUFFICIENT_DATA,
            forced_reason_data: None,
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = emulator();
        let (forced_topic, forced_queue) =
            given_topic_with_subscriber(&emulator, "forced-transition").await;
        let (reclaim_topic, reclaim_queue) =
            given_topic_with_subscriber(&emulator, "reclaimed-transition").await;
        let name = "reclaimed-record";
        publish(&emulator, &row, name).await;
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(&row, name, &forced_topic, &reclaim_topic),
        )
        .await;

        // The baseline is an EVALUATED record, not the one `PutMetricAlarm` created the alarm
        // with: the alarm's first evaluation of the published series decides it, and it is the
        // record the force then has to replace.
        let mut baseline_held = Vec::new();
        let baseline = until_reported(
            &emulator,
            &row,
            name,
            row.evaluated,
            &reclaim_queue,
            &mut baseline_held,
        )
        .await;
        assert_eq!(
            baseline.state, row.evaluated,
            "case {case:?}: the published series must decide the baseline state, got {baseline:?}"
        );
        until_held(&emulator, &reclaim_queue, &mut baseline_held).await;
        let first =
            only_notification(case, "the alarm's first evaluated transition", &baseline_held);
        assert_eq!(
            first["NewStateValue"].as_str(),
            Some(row.evaluated),
            "case {case:?}: the first transition must report the evaluated state, got {first}"
        );
        assert!(
            baseline.reason_data.is_some(),
            "case {case:?}: an evaluated record carries the decision record of the evaluation \
             that authored it, got {baseline:?}"
        );
        let _ = drain(&emulator, &forced_queue).await;

        // The force. Its record is the caller's own, so the pre-force record is not what a client
        // reads afterwards.
        let mut request = json!({
            "AlarmName": name,
            "StateValue": row.forced,
            "StateReason": FORCED_REASON,
        });
        if let Some(data) = row.forced_reason_data {
            request["StateReasonData"] = json!(data);
        }
        cw(&emulator, "SetAlarmState", &request).await;

        let forced = record(&emulator, name).await;
        assert_eq!(
            forced.state, row.forced,
            "case {case:?}: a client reading after the force must see the state it forced, got \
             {forced:?}"
        );
        assert_eq!(
            forced.reason, FORCED_REASON,
            "case {case:?}: the forced record must carry the caller's own reason rather than the \
             pre-force one ({:?}), got {forced:?}",
            baseline.reason
        );
        let expected_data: Option<Value> = row
            .forced_reason_data
            .map(|data| serde_json::from_str(data).expect("the row's reason data is a document"));
        assert_eq!(
            forced.reason_data, expected_data,
            "case {case:?}: the forced record must carry exactly the decision record the caller \
             sent, got {forced:?}"
        );
        let baseline_updated = baseline
            .updated_secs
            .unwrap_or_else(|| panic!("case {case:?}: the baseline record is stamped"));
        let forced_updated = forced
            .updated_secs
            .unwrap_or_else(|| panic!("case {case:?}: the forced record is stamped"));
        assert!(
            forced_updated >= baseline_updated,
            "case {case:?}: the force must not report a state update older than the record it \
             replaced ({baseline_updated}), got {forced_updated}"
        );

        // The force moved the alarm, so it invoked the actions of the state it moved to.
        let mut forced_held = Vec::new();
        until_held(&emulator, &forced_queue, &mut forced_held).await;
        let notified = only_notification(case, "the forced transition", &forced_held);
        assert_eq!(
            notified["NewStateValue"].as_str(),
            Some(row.forced),
            "case {case:?}: the forced transition must report the state forced, got {notified}"
        );
        assert_eq!(
            notified["OldStateValue"].as_str(),
            Some(row.evaluated),
            "case {case:?}: the forced transition must report the state the alarm left, got \
             {notified}"
        );
        assert_eq!(
            notified["NewStateReason"].as_str(),
            Some(FORCED_REASON),
            "case {case:?}: the forced transition must report the caller's own reason, got \
             {notified}"
        );

        // The series keeps being published, with no further control-plane call: the evaluation at
        // the end of the forced span owns the record again.
        let mut reclaim_held = Vec::new();
        let reclaimed = until_reported(
            &emulator,
            &row,
            name,
            row.evaluated,
            &reclaim_queue,
            &mut reclaim_held,
        )
        .await;
        assert_eq!(
            reclaimed.state, row.evaluated,
            "case {case:?}: the evaluation of the published series must take the forced state \
             back, got {reclaimed:?}"
        );
        for part in evaluated_reason_parts(&row) {
            assert!(
                reclaimed.reason.contains(&part),
                "case {case:?}: the re-authored reason must report {part:?}, got {:?}",
                reclaimed.reason
            );
        }
        let reclaimed_data = reclaimed.reason_data.clone().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the re-authored record must carry the decision record of the \
                 evaluation that authored it, got {reclaimed:?}"
            )
        });
        assert_eq!(
            reclaimed_data.get("recentDatapoints"),
            Some(&json!([row.value])),
            "case {case:?}: the decision record must name the datapoints the evaluation graded, \
             got {reclaimed_data}"
        );
        assert_eq!(
            reclaimed_data.get("threshold").and_then(Value::as_f64),
            Some(THRESHOLD),
            "case {case:?}: the decision record must name the threshold it compared against, got \
             {reclaimed_data}"
        );
        assert_eq!(
            reclaimed_data.get("period").and_then(Value::as_f64),
            Some(f64::from(row.period)),
            "case {case:?}: the decision record must name the period it folded the series by, got \
             {reclaimed_data}"
        );
        let baseline_query = baseline.query_date_secs().unwrap_or_else(|| {
            panic!("case {case:?}: the baseline record names its query instant, got {baseline:?}")
        });
        let reclaimed_query = reclaimed.query_date_secs().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the re-authored record names its query instant, got {reclaimed:?}"
            )
        });
        assert!(
            reclaimed_query > baseline_query,
            "case {case:?}: the re-authored record must be queried later than the pre-force one \
             ({baseline_query}), got {reclaimed_query}"
        );
        let reclaimed_updated = reclaimed
            .updated_secs
            .unwrap_or_else(|| panic!("case {case:?}: the re-authored record is stamped"));
        assert!(
            reclaimed_updated > forced_updated,
            "case {case:?}: the re-authored record must be stamped later than the forced one \
             ({forced_updated}), got {reclaimed_updated}"
        );

        // And the reclaim is a transition of its own: it invokes the actions of the state it
        // moved the alarm to, reporting the forced state as the one it moved out of.
        until_held(&emulator, &reclaim_queue, &mut reclaim_held).await;
        let recovery = only_notification(case, "the reclaiming transition", &reclaim_held);
        assert_eq!(
            recovery["OldStateValue"].as_str(),
            Some(row.forced),
            "case {case:?}: the reclaim must report the forced state as the one it left, got \
             {recovery}"
        );
        assert_eq!(
            recovery["NewStateValue"].as_str(),
            Some(row.evaluated),
            "case {case:?}: the reclaim must report the evaluated state, got {recovery}"
        );
        assert_eq!(
            recovery["NewStateReason"].as_str(),
            Some(reclaimed.reason.as_str()),
            "case {case:?}: the reclaim must report the reason the evaluation authored, got \
             {recovery}"
        );
    }
}
