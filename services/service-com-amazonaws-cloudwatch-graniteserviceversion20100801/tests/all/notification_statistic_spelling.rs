// SPDX-License-Identifier: CC0-1.0

//! How the STATISTIC of a metric alarm is spelled on each of the two wires it travels: the
//! API's, and the state-change document CloudWatch publishes to the alarm's actions.
//!
//! The RULE asserted here: a basic statistic is one configured value with two spellings. The API
//! spells it in title case — the `Statistic` enum's own values, which is what `PutMetricAlarm`
//! accepts and `DescribeAlarms` reports (`smithy_query(query="shape",shape="Statistic")`) —
//! while the notification's `Trigger` carries the upper-case token of the same statistic beside
//! the `StatisticType` that says which member the alarm carries. A consumer of the payload
//! therefore matches the field as the enum CloudWatch puts on the wire, and a `DescribeAlarms`
//! result can still be fed straight back into `PutMetricAlarm`.
//!
//! A statistic of ANOTHER family is not a value of that enum and has no second spelling: an
//! `ExtendedStatistic` ("the percentile statistic for the metric specified in MetricName",
//! `MetricAlarm$ExtendedStatistic`) travels as the alarm names it, under its own
//! `StatisticType`.
//!
//! The rows differ along what the rule generalises over: which statistic of the enum the alarm
//! was configured with — every one of the five, so the row the issue cited is one of them — and
//! which statistic FAMILY the member belongs to.

use serde_json::{json, Value};

use crate::harness::{
    emulator, given_topic_with_subscriber, rpc, set_alarm_state, until_notified, Emulator,
    TARGET_PREFIX,
};

/// The signing name CloudWatch's own requests are scoped by.
const CLOUDWATCH: &str = "monitoring";

/// Every value the `Statistic` enum declares, in the order the model declares them
/// (`smithy_query(query="shape",shape="Statistic")`): these are the names `PutMetricAlarm`
/// accepts on `Statistic`, and the ones whose notification spelling this suite asserts.
const STATISTICS: [&str; 5] = ["SampleCount", "Average", "Sum", "Minimum", "Maximum"];

/// Call one CloudWatch operation, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH, TARGET_PREFIX, operation, body).await
}

/// The one metric alarm `DescribeAlarms` reports for `name`.
async fn described(emulator: &Emulator, name: &str) -> Value {
    let body = cw(emulator, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    body["MetricAlarms"][0].clone()
}

/// The notification one transition of the alarm `name` delivers to the queue at `url`.
async fn notification_of_a_transition(
    emulator: &Emulator,
    name: &str,
    url: &str,
) -> Value {
    // A transition into the state the alarm carries the action for, so exactly one document is
    // owed; the wait is for the document, not for a span.
    set_alarm_state(emulator, name, "ALARM").await;
    let delivered = until_notified(emulator, url).await;
    delivered
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the transition of {name} must deliver a notification"))
}

#[tokio::test]
async fn a_notification_carries_the_statistic_as_the_upper_case_token() {
    for statistic in STATISTICS {
        let case = statistic;
        let emulator = emulator();
        let (topic, url) = given_topic_with_subscriber(&emulator, "statistic-tokens").await;
        let alarm = format!("{statistic}-alarm");
        cw(
            &emulator,
            "PutMetricAlarm",
            &json!({
                "AlarmName": &alarm,
                "Namespace": "Custom/Statistics",
                "MetricName": "Latency",
                "Statistic": statistic,
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanThreshold",
                "AlarmActions": [&topic],
            }),
        )
        .await;

        let notification = notification_of_a_transition(&emulator, &alarm, &url).await;
        let trigger = &notification["Trigger"];
        let expected = statistic.to_ascii_uppercase();
        assert_eq!(
            trigger["Statistic"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: the notification must carry the statistic as its upper-case token, \
             expected {expected:?}, got {trigger}"
        );
        assert_eq!(
            trigger["StatisticType"].as_str(),
            Some("Statistic"),
            "case {case:?}: the notification must say which statistic member the alarm carries, \
             got {trigger}"
        );
        assert!(
            trigger.get("ExtendedStatistic").is_none(),
            "case {case:?}: an alarm carrying a basic statistic must report no \
             ExtendedStatistic, got {trigger}"
        );

        // The API's own spelling is untouched by the document's: the alarm reads back as the
        // value the enum declares, so the read can be fed straight back into the write.
        let read_back = described(&emulator, &alarm).await;
        assert_eq!(
            read_back["Statistic"].as_str(),
            Some(statistic),
            "case {case:?}: DescribeAlarms must report the statistic in the spelling the model \
             declares, got {read_back}"
        );
    }
}

#[tokio::test]
async fn a_notification_carries_an_extended_statistic_as_the_alarm_names_it() {
    // (case, the ExtendedStatistic the alarm is configured with)
    //
    // "The percentile statistic for the metric specified in MetricName"
    // (`smithy_query(query="doc",shape="MetricAlarm",member="ExtendedStatistic")`): a value of a
    // family of its own rather than of the `Statistic` enum, so there is no second spelling of
    // it — the rows differ in which family member the alarm names.
    let rows: [(&str, &str); 3] = [
        ("a percentile", "p99"),
        ("a percentile with a fraction", "p99.9"),
        ("a trimmed mean", "tm95"),
    ];

    for (case, extended) in rows {
        let emulator = emulator();
        let (topic, url) = given_topic_with_subscriber(&emulator, "extended-statistics").await;
        let alarm = format!("extended-{extended}-alarm");
        cw(
            &emulator,
            "PutMetricAlarm",
            &json!({
                "AlarmName": &alarm,
                "Namespace": "Custom/Statistics",
                "MetricName": "Latency",
                "ExtendedStatistic": extended,
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanThreshold",
                "AlarmActions": [&topic],
            }),
        )
        .await;

        let notification = notification_of_a_transition(&emulator, &alarm, &url).await;
        let trigger = &notification["Trigger"];
        assert_eq!(
            trigger["ExtendedStatistic"].as_str(),
            Some(extended),
            "case {case:?}: the notification must carry the extended statistic as the alarm \
             names it, got {trigger}"
        );
        assert_eq!(
            trigger["StatisticType"].as_str(),
            Some("ExtendedStatistic"),
            "case {case:?}: the notification must say which statistic member the alarm carries, \
             got {trigger}"
        );
        assert!(
            trigger.get("Statistic").is_none(),
            "case {case:?}: an alarm carrying an extended statistic must report no Statistic, \
             got {trigger}"
        );
    }
}
