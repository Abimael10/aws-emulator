// SPDX-License-Identifier: CC0-1.0

//! A probe of the delivery a transition owes when the client makes NO further call to
//! CloudWatch: the service's own alarm clock is what has to materialise the evaluation and
//! execute the action, because a client that has written its alarms then watches the TARGET.
//!
//! The RULE asserted here: a transition the clock makes due executes the alarm's actions
//! without any further request to CloudWatch — "The actions to execute when this alarm
//! transitions to the ALARM state from any other state"
//! (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="AlarmActions")`) is owed by
//! the transition, and the transition is owed by the clock — while an alarm whose warm-up
//! window still stands is not graded at all, so its own actions are not executed
//! (`WarmUpConfiguration`: "During the warm-up period, the alarm stays in INSUFFICIENT_DATA
//! and does not perform alarm actions").
//!
//! Both arms watch series that are already breaching, act on ONE topic, and are written in one
//! go; after that only the subscribed QUEUE is read, so what arrives there is the clock's work
//! and the arm each notification names is the whole assertion.

use serde_json::{json, Value};

use crate::harness::{
    emulator, given_topic_with_subscriber, now_secs, rpc, until_notified, Emulator, TARGET_PREFIX,
};

/// The signing name CloudWatch's own requests are scoped by.
const CLOUDWATCH: &str = "monitoring";

/// The namespace this suite's series live in.
const NAMESPACE: &str = "Custom/ClockDelivery";

/// Call one CloudWatch operation, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH, TARGET_PREFIX, operation, body).await
}

/// The `PutMetricAlarm` request for `name`, acting on `topic`, carrying `warm_up` when the arm
/// configures one.
fn alarm_request(name: &str, topic: &str, warm_up: Option<Value>) -> Value {
    let mut request = json!({
        "AlarmName": name,
        "Namespace": NAMESPACE,
        "MetricName": "Errors",
        "Dimensions": [{"Name": "Alarm", "Value": name}],
        "Statistic": "Maximum",
        "Period": 60,
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanThreshold",
        "AlarmActions": [topic],
    });
    if let Some(warm_up) = warm_up {
        request["WarmUpConfiguration"] = warm_up;
    }
    request
}

/// Publish five closed minute-wide windows of breaching data into the series `name` identifies.
async fn publish_breaching_history(emulator: &Emulator, name: &str) {
    let base = (now_secs() / 60.0).floor() * 60.0;
    let data: Vec<Value> = (1..=5)
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

/// The longest warm-up the model admits, in minutes (`WarmUpPeriodDurationInMinutes` is
/// `@range 1..2880`): a window that cannot have passed while the case runs.
const LONGEST_WARM_UP_MINUTES: i64 = 2_880;

#[tokio::test]
async fn the_clock_delivers_the_transition_a_client_makes_no_further_call_for() {
    let emulator = emulator();
    let (topic, url) = given_topic_with_subscriber(&emulator, "clock-delivery").await;
    let gated = "clock-gated";
    let live = "clock-live";
    for name in [gated, live] {
        publish_breaching_history(&emulator, name).await;
    }
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(
            gated,
            &topic,
            Some(json!({
                "WarmUpPeriodDurationInMinutes": LONGEST_WARM_UP_MINUTES,
                "OnlyStartEvaluatingAfterWarmUpPeriodEnds": true,
            })),
        ),
    )
    .await;
    cw(&emulator, "PutMetricAlarm", &alarm_request(live, &topic, None)).await;

    // From here on only the queue is read.
    let delivered = until_notified(&emulator, &url).await;
    let names: Vec<&str> = delivered
        .iter()
        .filter_map(|notification| notification["AlarmName"].as_str())
        .collect();
    assert!(
        names.contains(&live),
        "the arm with no warm-up must have its action executed by the clock alone, got {names:?} \
         in {delivered:?}"
    );
    assert!(
        !names.contains(&gated),
        "the arm inside its warm-up window must have no action executed, got {names:?} in \
         {delivered:?}"
    );
}
