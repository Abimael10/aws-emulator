// SPDX-License-Identifier: CC0-1.0

//! A probe of the FIRST evaluation of an alarm created over a series that was ALREADY breaching
//! before the alarm existed — the shape a recorded scenario reaches an alarm's actions through.
//!
//! The RULE asserted here: an alarm's state is computed from the datapoints the metric store
//! holds and from nothing else, so which of them were published BEFORE the alarm was written
//! makes no difference — "When this operation creates an alarm, the alarm state is immediately
//! set to INSUFFICIENT_DATA. The alarm is then evaluated and its state is set appropriately."
//! (`smithy_query(query="operation",operation="PutMetricAlarm")`). An alarm written over a
//! series whose graded windows already breach is therefore moved to `ALARM` by its FIRST
//! evaluation, and the action that transition fires is executed.
//!
//! The rows differ along what the rule generalises over: how many of the alarm's evaluation
//! periods the already-published data fills, and how many periods the alarm evaluates.

use serde_json::{json, Value};

use crate::harness::{
    emulator, given_topic_with_subscriber, now_secs, rpc, until_notified, until_state, Emulator,
    TARGET_PREFIX,
};

/// The signing name CloudWatch's own requests are scoped by.
const CLOUDWATCH: &str = "monitoring";

/// The namespace this suite's series live in.
const NAMESPACE: &str = "Custom/AlreadyBreaching";

/// Call one CloudWatch operation, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH, TARGET_PREFIX, operation, body).await
}

/// Publish `windows` minute-wide windows of breaching data into the series `name` identifies,
/// the newest of them already CLOSED — the series as it stands before any alarm watches it.
async fn publish_breaching_history(emulator: &Emulator, name: &str, windows: u32) {
    let base = (now_secs() / 60.0).floor() * 60.0;
    let data: Vec<Value> = (1..=windows)
        .map(|back| {
            json!({
                "MetricName": "Errors",
                "Dimensions": [{"Name": "Alarm", "Value": name}],
                "Timestamp": base - 60.0 * f64::from(back) + 30.0,
                "Value": 5.0,
                "StorageResolution": 60,
            })
        })
        .collect();
    cw(
        emulator,
        "PutMetricData",
        &json!({"Namespace": NAMESPACE, "MetricData": data}),
    )
    .await;
}

#[tokio::test]
async fn an_alarm_over_an_already_breaching_series_fires_on_its_first_evaluation() {
    // (case, how many closed minute windows the series already holds, the alarm's
    //  EvaluationPeriods)
    let rows: [(&str, u32, i64); 3] = [
        ("one filled period of one", 1, 1),
        ("two filled periods of two", 2, 2),
        ("five filled periods of three", 5, 3),
    ];

    for (case, windows, evaluation_periods) in rows {
        let emulator = emulator();
        let (topic, url) = given_topic_with_subscriber(&emulator, "already-breaching").await;
        let name = format!("already-breaching-{windows}-{evaluation_periods}");
        publish_breaching_history(&emulator, &name, windows).await;

        cw(
            &emulator,
            "PutMetricAlarm",
            &json!({
                "AlarmName": &name,
                "Namespace": NAMESPACE,
                "MetricName": "Errors",
                "Dimensions": [{"Name": "Alarm", "Value": &name}],
                "Statistic": "Maximum",
                "Period": 60,
                "EvaluationPeriods": evaluation_periods,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanThreshold",
                "AlarmActions": [&topic],
            }),
        )
        .await;

        let state = until_state(&emulator, &name, "ALARM").await;
        assert_eq!(
            state, "ALARM",
            "case {case:?}: an alarm over an already breaching series must be graded on the data \
             the store holds, got {state:?}"
        );
        let delivered = until_notified(&emulator, &url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: that transition must execute the alarm's action, got {delivered:?}"
        );
        assert_eq!(
            delivered[0]["AlarmName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the notification must name the alarm that fired it, got {}",
            delivered[0]
        );
    }
}
