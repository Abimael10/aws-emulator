// SPDX-License-Identifier: CC0-1.0

//! The WARM-UP window a metric alarm is created with: what it withholds while it is in force,
//! what a read of such an alarm reports, and what ends it — end to end over the wire with
//! CloudWatch, SNS and SQS on one engine, so each answer is read where a client reads it.
//!
//! The RULE asserted here, which is the configuration's own: "Use these settings to delay alarm
//! evaluation after you create or update the alarm […] During the warm-up period, the alarm
//! stays in `INSUFFICIENT_DATA` and does not perform alarm actions."
//! (`smithy_query(query="shape",shape="WarmUpConfiguration")`) — so while the window stands the
//! alarm is not graded at all, whatever its series hold: it reports the state its write left
//! behind, fires no action and delivers no notification, and a read of it reports the evaluation
//! state `IN_WARM_UP`, which is what a client waiting for the warm-up to pass polls for. The
//! window lasts "the length of the warm-up period, in minutes" from the write, and ends EARLY
//! once "the alarm has enough metric data to fill its evaluation window" unless the alarm asked
//! to sit the whole period out
//! (`WarmUpConfiguration$OnlyStartEvaluatingAfterWarmUpPeriodEnds`); once it is over the alarm
//! is graded like any other, reports no warm-up evaluation state, and a later write does not
//! bring the window back — "If you change it after the warm-up period ends, the new value does
//! not restart the warm-up period."
//!
//! The rows differ along what the rule generalises over: whether the alarm carries a warm-up at
//! all, how long its period is (from the shortest the model admits to the longest), and whether
//! it ends early or is sat out.

use std::time::Instant;

use serde_json::{json, Value};

use crate::harness::{
    emulator, given_topic_with_subscriber, graded_window_stamp, notifications, rpc,
    states_over_the_watch, until_notified, Emulator, LOOK_INTERVAL, QUIET_WATCH, TARGET_PREFIX,
    WORK_WAIT,
};

/// The signing name CloudWatch's own requests are scoped by.
const CLOUDWATCH: &str = "monitoring";

/// The evaluation state an alarm inside its warm-up window reports.
const IN_WARM_UP: &str = "IN_WARM_UP";

/// The alarm's period, in seconds, and the window a datapoint is published into.
const PERIOD: i64 = 60;

/// The shortest warm-up a caller may configure, in minutes: `WarmUpPeriodDurationInMinutes` is
/// `@range 1..2880` (`smithy_query(query="shape",shape="WarmUpConfiguration")`).
const SHORTEST_WARM_UP_MINUTES: i64 = 1;

/// The longest one, in minutes — the other published bound of the same range. Used where a case
/// needs a window that cannot possibly have expired while the case runs.
const LONGEST_WARM_UP_MINUTES: i64 = 2_880;

/// How long a case waits for a warm-up of [`SHORTEST_WARM_UP_MINUTES`] to pass.
///
/// The warm-up period is a duration the CALLER configured, so it keeps its value: the shortest
/// one the model admits is a minute, and this deadline is several times that span so a loaded or
/// a slow host still reaches it. Nothing is asserted about how long the wait took — only that
/// the alarm the window withheld is graded once it is over.
const PAST_SHORTEST_WARM_UP: std::time::Duration = std::time::Duration::from_secs(180);

/// Call one CloudWatch operation, requiring a 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH, TARGET_PREFIX, operation, body).await
}

/// The `PutMetricAlarm` request for the alarm `name` over its own series, breaching once a
/// datapoint above the threshold is published, firing `topic` on `ALARM`, and carrying
/// `warm_up` when the case configures one.
fn alarm_request(name: &str, topic: &str, warm_up: Option<Value>) -> Value {
    let mut request = json!({
        "AlarmName": name,
        "Namespace": "Custom/WarmUp",
        "MetricName": "Errors",
        "Dimensions": [{"Name": "Alarm", "Value": name}],
        "Statistic": "Maximum",
        "Period": PERIOD,
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

/// Publish one breaching observation into the series the alarm `name` watches, stamped in the
/// newest window an evaluation grades — so what decides the alarm is a datapoint of a CLOSED
/// period rather than one the clock is still inside.
async fn publish_breach(emulator: &Emulator, name: &str) {
    cw(
        emulator,
        "PutMetricData",
        &json!({
            "Namespace": "Custom/WarmUp",
            "MetricData": [{
                "MetricName": "Errors",
                "Dimensions": [{"Name": "Alarm", "Value": name}],
                "Timestamp": graded_window_stamp(f64::from(u32::try_from(PERIOD).unwrap_or(60))),
                "Value": 5.0,
                "StorageResolution": 60,
            }],
        }),
    )
    .await;
}

/// The one metric alarm `DescribeAlarms` reports for `name`.
async fn described(emulator: &Emulator, name: &str) -> Value {
    let body = cw(emulator, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    body["MetricAlarms"][0].clone()
}

/// The `(StateValue, EvaluationState)` pair that read reports.
async fn reported(emulator: &Emulator, name: &str) -> (Option<String>, Option<String>) {
    let alarm = described(emulator, name).await;
    (
        alarm["StateValue"].as_str().map(str::to_string),
        alarm.get("EvaluationState").and_then(Value::as_str).map(str::to_string),
    )
}

/// Keep the alarm's series breaching while looking for the state `wanted`, until it is reported
/// or `deadline` passes; answers what was last reported.
///
/// The publish is repeated because the windows keep moving: a datapoint stays in the window the
/// case placed it in, and every publish of this loop carries the same value, so what the
/// evaluation grades stays the breach being waited for. The wait is for the CONDITION — the
/// state the alarm reaches — and not for a span of this host's clock.
async fn until_state_while_breaching(
    emulator: &Emulator,
    name: &str,
    wanted: &str,
    deadline: std::time::Duration,
) -> (Option<String>, Option<String>) {
    let stop = Instant::now() + deadline;
    loop {
        publish_breach(emulator, name).await;
        let seen = reported(emulator, name).await;
        if seen.0.as_deref() == Some(wanted) || Instant::now() >= stop {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Keep the alarm's series breaching over [`QUIET_WATCH`], and answer every state it reported.
async fn states_while_breaching(emulator: &Emulator, name: &str) -> Vec<String> {
    let stop = Instant::now() + QUIET_WATCH;
    let mut seen: Vec<String> = Vec::new();
    while Instant::now() < stop {
        publish_breach(emulator, name).await;
        let state = reported(emulator, name).await.0.unwrap_or_default();
        if seen.last() != Some(&state) {
            seen.push(state);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

#[tokio::test]
async fn a_read_of_an_alarm_inside_its_warm_up_reports_the_warm_up_evaluation_state() {
    // (case, the WarmUpConfiguration the alarm is created with, the EvaluationState expected)
    //
    // The rows differ in whether a warm-up is configured at all, in the span it names — both
    // published bounds of `WarmUpPeriodDurationInMinutes` (`@range 1..2880`) and one between
    // them — and in whether the alarm sits the period out.
    let rows: [(&str, Option<Value>, Option<&str>); 5] = [
        ("no warm-up at all", None, None),
        (
            "the shortest warm-up the model admits",
            Some(json!({"WarmUpPeriodDurationInMinutes": SHORTEST_WARM_UP_MINUTES})),
            Some(IN_WARM_UP),
        ),
        (
            "a warm-up that is sat out",
            Some(json!({
                "WarmUpPeriodDurationInMinutes": 5,
                "OnlyStartEvaluatingAfterWarmUpPeriodEnds": true,
            })),
            Some(IN_WARM_UP),
        ),
        (
            "a warm-up that may end early",
            Some(json!({
                "WarmUpPeriodDurationInMinutes": 5,
                "OnlyStartEvaluatingAfterWarmUpPeriodEnds": false,
            })),
            Some(IN_WARM_UP),
        ),
        (
            "the longest warm-up the model admits",
            Some(json!({"WarmUpPeriodDurationInMinutes": LONGEST_WARM_UP_MINUTES})),
            Some(IN_WARM_UP),
        ),
    ];

    let emulator = emulator();
    let (topic, _url) = given_topic_with_subscriber(&emulator, "warm-up-reads").await;
    for (case, warm_up, expected) in rows {
        let name = format!("read-{}", case.replace(' ', "-"));
        cw(
            &emulator,
            "PutMetricAlarm",
            &alarm_request(&name, &topic, warm_up.clone()),
        )
        .await;

        let (state, evaluation_state) = reported(&emulator, &name).await;
        assert_eq!(
            state.as_deref(),
            Some("INSUFFICIENT_DATA"),
            "case {case:?}: a newly created alarm reports INSUFFICIENT_DATA, got {state:?}"
        );
        assert_eq!(
            evaluation_state.as_deref(),
            expected,
            "case {case:?}: expected EvaluationState {expected:?}, got {evaluation_state:?}"
        );
        // The configuration itself round-trips: the window a read reports is the one the write
        // asked for.
        let read_back = described(&emulator, &name).await;
        assert_eq!(
            read_back.get("WarmUpConfiguration"),
            warm_up.as_ref(),
            "case {case:?}: expected WarmUpConfiguration {warm_up:?}, got {read_back}"
        );
    }
}

#[tokio::test]
async fn a_warm_up_withholds_the_transition_and_its_notification_until_it_ends() {
    let emulator = emulator();
    // Each arm acts on a topic of its own, so a delivered notification names the arm it came
    // from rather than being attributed by its contents.
    let (gated_topic, gated_url) = given_topic_with_subscriber(&emulator, "gated").await;
    let (live_topic, live_url) = given_topic_with_subscriber(&emulator, "live").await;
    let gated = "gated-alarm";
    let live = "live-alarm";
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(
            gated,
            &gated_topic,
            Some(json!({
                "WarmUpPeriodDurationInMinutes": SHORTEST_WARM_UP_MINUTES,
                // The arm sits the whole period out, so nothing but the window's own end
                // releases it.
                "OnlyStartEvaluatingAfterWarmUpPeriodEnds": true,
            })),
        ),
    )
    .await;
    // The control: the same alarm over its own series, with no warm-up at all.
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(live, &live_topic, None),
    )
    .await;

    // The control proves the breach really decides an alarm of this configuration, so the gated
    // arm's silence below is the warm-up and not a pipeline that delivers nothing.
    let (live_state, live_evaluation_state) =
        until_state_while_breaching(&emulator, live, "ALARM", WORK_WAIT).await;
    assert_eq!(
        live_state.as_deref(),
        Some("ALARM"),
        "the alarm with no warm-up must be graded on the published breach, got {live_state:?}"
    );
    assert_eq!(
        live_evaluation_state, None,
        "an alarm with no warm-up reports no EvaluationState, got {live_evaluation_state:?}"
    );
    let live_delivered = until_notified(&emulator, &live_url).await;
    assert_eq!(
        live_delivered.len(),
        1,
        "the alarm with no warm-up must deliver its transition's notification, got \
         {live_delivered:?}"
    );

    // The gated arm, over the same breach: not graded at all while its window stands.
    let held = states_while_breaching(&emulator, gated).await;
    assert_eq!(
        held,
        vec!["INSUFFICIENT_DATA".to_string()],
        "an alarm inside its warm-up must hold the state its write left behind, got {held:?}"
    );
    let (_, warming) = reported(&emulator, gated).await;
    assert_eq!(
        warming.as_deref(),
        Some(IN_WARM_UP),
        "an alarm inside its warm-up must report {IN_WARM_UP}, got {warming:?}"
    );
    let withheld = notifications(&emulator, &gated_url).await;
    assert!(
        withheld.is_empty(),
        "an alarm inside its warm-up performs no action, got {withheld:?}"
    );

    // Once the window has passed, the same breach decides the alarm, its notification is
    // delivered, and the warm-up evaluation state is gone.
    let (released, after) =
        until_state_while_breaching(&emulator, gated, "ALARM", PAST_SHORTEST_WARM_UP).await;
    assert_eq!(
        released.as_deref(),
        Some("ALARM"),
        "once its warm-up has passed, the alarm must be graded on the breach, got {released:?}"
    );
    assert_eq!(
        after, None,
        "an alarm whose warm-up has passed reports no EvaluationState, got {after:?}"
    );
    let delivered = until_notified(&emulator, &gated_url).await;
    assert_eq!(
        delivered.len(),
        1,
        "the transition the warm-up withheld must deliver exactly one notification once the \
         window has passed, got {delivered:?}"
    );
    assert_eq!(
        delivered[0]["NewStateValue"].as_str(),
        Some("ALARM"),
        "the delivered notification must report the transition the warm-up withheld, got {}",
        delivered[0]
    );

    // "If you change it after the warm-up period ends, the new value does not restart the
    // warm-up period." — a write naming the longest warm-up the model admits leaves this alarm
    // out of warm-up, so it keeps being graded.
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(
            gated,
            &gated_topic,
            Some(json!({"WarmUpPeriodDurationInMinutes": LONGEST_WARM_UP_MINUTES})),
        ),
    )
    .await;
    let (restated, restated_evaluation_state) = reported(&emulator, gated).await;
    assert_eq!(
        restated_evaluation_state, None,
        "a warm-up configured again after the window passed must not restart it, got \
         {restated_evaluation_state:?}"
    );
    assert_eq!(
        restated.as_deref(),
        Some("ALARM"),
        "a configuration write leaves the alarm's state unchanged, got {restated:?}"
    );
}

#[tokio::test]
async fn a_warm_up_ends_early_once_the_data_fills_the_evaluation_window() {
    // Both arms carry the LONGEST warm-up the model admits, so neither window can have expired
    // while the case runs: what tells them apart is only whether the alarm waits the period out.
    let emulator = emulator();
    let (early_topic, early_url) = given_topic_with_subscriber(&emulator, "early").await;
    let (sat_out_topic, sat_out_url) = given_topic_with_subscriber(&emulator, "sat-out").await;
    let early = "early-alarm";
    let sat_out = "sat-out-alarm";
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(
            early,
            &early_topic,
            // "The default is false. […] If false, the alarm ends the warm-up period early."
            Some(json!({"WarmUpPeriodDurationInMinutes": LONGEST_WARM_UP_MINUTES})),
        ),
    )
    .await;
    cw(
        &emulator,
        "PutMetricAlarm",
        &alarm_request(
            sat_out,
            &sat_out_topic,
            Some(json!({
                "WarmUpPeriodDurationInMinutes": LONGEST_WARM_UP_MINUTES,
                "OnlyStartEvaluatingAfterWarmUpPeriodEnds": true,
            })),
        ),
    )
    .await;

    // The arm that may end early is graded as soon as its one evaluation period is filled.
    let (early_state, early_evaluation_state) =
        until_state_while_breaching(&emulator, early, "ALARM", WORK_WAIT).await;
    assert_eq!(
        early_state.as_deref(),
        Some("ALARM"),
        "an alarm whose warm-up may end early must be graded once its evaluation window is \
         filled, got {early_state:?}"
    );
    assert_eq!(
        early_evaluation_state, None,
        "an alarm whose warm-up ended early reports no EvaluationState, got \
         {early_evaluation_state:?}"
    );
    let early_delivered = until_notified(&emulator, &early_url).await;
    assert_eq!(
        early_delivered.len(),
        1,
        "the transition that followed the early end must deliver its notification, got \
         {early_delivered:?}"
    );

    // The arm that sits the period out is not graded on the same data.
    let held = states_while_breaching(&emulator, sat_out).await;
    assert_eq!(
        held,
        vec!["INSUFFICIENT_DATA".to_string()],
        "an alarm that waits the whole warm-up period out must not be graded on arriving data, \
         got {held:?}"
    );
    let (_, warming) = reported(&emulator, sat_out).await;
    assert_eq!(
        warming.as_deref(),
        Some(IN_WARM_UP),
        "an alarm still inside its warm-up must report {IN_WARM_UP}, got {warming:?}"
    );
    let withheld = notifications(&emulator, &sat_out_url).await;
    assert!(
        withheld.is_empty(),
        "an alarm still inside its warm-up performs no action, got {withheld:?}"
    );
    // The states a watch of the other arm would have seen are not this one's: the control above
    // moved, so the silence here is the warm-up.
    let unchanged = states_over_the_watch(&emulator, sat_out).await;
    assert_eq!(
        unchanged,
        vec!["INSUFFICIENT_DATA".to_string()],
        "the arm that sits its warm-up out must hold its state with nothing published either, \
         got {unchanged:?}"
    );
}
