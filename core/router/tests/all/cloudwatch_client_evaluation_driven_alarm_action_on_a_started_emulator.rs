// SPDX-License-Identifier: CC0-1.0

//! An EVALUATION-DRIVEN metric alarm — one no caller forces, moved by the datapoints a client
//! published — reaches the queue subscribed to the topic its `AlarmActions` names, on the
//! emulator a client STARTS: the built binary, on its own port, with request authentication on,
//! reached over TLS with SigV4-signed calls.
//!
//! The RULE under test: **an alarm whose `ActionsEnabled` is true and whose `AlarmActions` names
//! an SNS topic executes that action on the transition its own EVALUATION makes, so a series the
//! alarm's comparison breaches over puts it in `ALARM` and the subscribed queue receives one SNS
//! envelope whose `Message` document names the alarm, carries `NewStateValue` `ALARM` and the
//! state it moved out of, while the alarm's history records that execution as an `Action` item
//! reporting `Succeeded` for the topic the publish went to.** The action list is the one the
//! state moved to selects — "The actions to execute when this alarm transitions to the ALARM
//! state from any other state. Each action is specified as an Amazon Resource Name (ARN) […]
//! Amazon SNS actions: arn:aws:sns:region:account-id:sns-topic-name"
//! (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="AlarmActions")`) — and the one
//! thing that withholds it is a WARM-UP window: "During the warm-up period, the alarm stays in
//! INSUFFICIENT_DATA and does not perform alarm actions."
//! (`smithy_query(query="shape",shape="WarmUpConfiguration")`), which is why each row drives a
//! gated arm beside the control and reads both.
//!
//! What only this suite reaches: an evaluation-driven transition's whole delivery chain against
//! the emulator as it is SERVED, with every resource written BY A CLIENT — the topic, the queue,
//! its policy, the `sqs` subscription, the published series and the alarms. The sibling suite
//! [`crate::cloudwatch_client_forced_alarm_action_on_a_started_emulator`] drives the same chain
//! from a `SetAlarmState` FORCE, which queues its notification inside the forcing call; here
//! nothing but the alarm's own evaluation of the stored datapoints can move it, so a transition
//! only the in-process router materialises, or one whose publish the started process cannot make,
//! is a failure a test reports rather than one only a client sees.
//!
//! The rows differ along what the rule generalises over: the wire the client WRITES over (the
//! `awsQuery` form body a query-protocol SDK sends, where the action list arrives as
//! `AlarmActions.member.1` and a time stamp as ISO 8601 text, and the `awsJson1_0` body the RPC
//! one sends), how the subscribed queue's policy scopes its grant to SNS (unscoped, or
//! conditioned on the publishing topic), how an ISO time stamp is spelled (with and without the
//! microseconds a client's clock carries) and how many closed periods of the series were already
//! breaching when the alarm was written. Every wait is on a CONDITION, and the absence leg is
//! read only once the control's own delivery has arrived.

use std::time::{Duration, Instant};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::element;

/// The service principal SNS performs its delivery to a subscribed queue as, which that queue's
/// own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The API version an `awsQuery` client sends in its form body for each service
/// (`Action=<Operation>&Version=<version>`).
const CLOUDWATCH_VERSION: &str = "2010-08-01";
const SNS_VERSION: &str = "2010-03-31";

/// The `X-Amz-Target` prefix CloudWatch's RPC wire names an operation under.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// How long a wait for a state or for a delivery lasts. Generous: every wait is on the CONDITION
/// it is waiting for, so a loaded host is given room rather than failed.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long one `ReceiveMessage` long-polls, which is what a consumer of such a queue does, and
/// what paces each turn of a wait that re-publishes the series meanwhile.
const WAIT_TIME_SECONDS: u32 = 2;

/// The namespace every row publishes into, each row under a metric name of its own so no row's
/// series is another's.
const NAMESPACE: &str = "Started/EvaluationDriven";

/// The value every row publishes and the threshold its alarms compare it against: one datapoint
/// of the series breaches, which is what `DatapointsToAlarm` 1 asks for.
const BREACHING_VALUE: &str = "100.0";
const THRESHOLD: &str = "10.0";

/// The width of the periods every row's alarms aggregate their series into, in seconds, and the
/// `Period` they are written with.
const PERIOD_SECS: f64 = 60.0;

/// The longest warm-up the model admits, in minutes (`WarmUpConfiguration$WarmUpPeriodDurationInMinutes`
/// is `@range 1..2880`): a window that cannot have passed while a row runs, so the gated arm's
/// withholding is the only thing its reading can be.
const LONGEST_WARM_UP_MINUTES: &str = "2880";

/// The SHORTEST warm-up the model admits, in minutes, which is the window a case that waits for
/// one to PASS writes.
const SHORTEST_WARM_UP_MINUTES: &str = "1";

/// The `EvaluationState` an alarm inside its warm-up window reports.
const IN_WARM_UP: &str = "IN_WARM_UP";

/// Which of the two protocols this service's model declares a row writes over.
#[derive(Debug, Clone, Copy)]
enum Wire {
    /// The `awsQuery` form body, where a list member arrives as `<Member>.member.1` and a time
    /// stamp as ISO 8601 text.
    Form,
    /// The `awsJson1_0` body, where a list is a JSON array and a time stamp a number.
    Json,
}

/// How an ISO 8601 time stamp is spelled on the form wire: a client's clock carries microseconds,
/// and a whole second carries none.
#[derive(Debug, Clone, Copy)]
enum Stamp {
    WholeSecond,
    Microsecond,
}

/// How the subscribed queue's policy grants SNS the send: the documented grant, and whether it is
/// scoped by condition to the publishing topic.
#[derive(Debug, Clone, Copy)]
enum Grant {
    Unscoped,
    ScopedToTopic,
}

impl Grant {
    fn policy(self, queue_arn: &str, topic_arn: &str) -> String {
        let mut statement = json!({
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        });
        if matches!(self, Self::ScopedToTopic) {
            statement["Condition"] = json!({"ArnEquals": {"aws:SourceArn": topic_arn}});
        }
        json!({"Version": "2012-10-17", "Statement": [statement]}).to_string()
    }
}

/// One row of the table: what it writes, over which wire, and what it writes it against.
struct Row {
    case: &'static str,
    wire: Wire,
    stamp: Stamp,
    grant: Grant,
    /// How many CLOSED periods of the series were already breaching when the alarm was written.
    filled_periods: u32,
}

/// Wall-clock now as the epoch seconds the evaluation windows are measured against.
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the system clock is after the epoch")
        .as_secs_f64()
}

/// `secs` as the ISO 8601 instant an `awsQuery` client spells a time stamp with — the calendar
/// rendering of an epoch instant, made by the SDK's own date-time type rather than by calendar
/// arithmetic here: a whole second for the client whose clock carries no sub-second part, and the
/// same instant with the fraction it carries for the one whose clock does.
fn iso8601(secs: f64, stamp: Stamp) -> String {
    let instant = match stamp {
        #[allow(clippy::cast_possible_truncation)]
        Stamp::WholeSecond => DateTime::from_secs(secs.floor() as i64),
        Stamp::Microsecond => DateTime::from_secs_f64(secs),
    };
    instant
        .fmt(Format::DateTime)
        .expect("an epoch instant renders as a date-time")
}

/// One Amazon SQS call, over the `awsJson1_0` wire that service's SDK addresses it on.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Amazon SNS call, over the `awsQuery` form wire that service serves.
fn sns(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", SNS_VERSION, action, params);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {text}"
    );
    text
}

/// One CloudWatch WRITE over the row's own wire, from the form fields of the request: an indexed
/// `<Member>.member.<n>` field is an element of the array member `<Member>` on the RPC wire, a
/// dotted `<Member>.<Field>` field is a member of the structure `<Member>`, and a numeric field is
/// the number the JSON codec reads an `Integer` or a `Double` as — so one row's request can never
/// be a different request from another's.
fn cloudwatch_write(
    emulator: &Started,
    case: &str,
    wire: Wire,
    operation: &str,
    params: &[(&str, &str)],
) {
    let (status, text) = match wire {
        Wire::Form => emulator.form_call("monitoring", CLOUDWATCH_VERSION, operation, params),
        Wire::Json => emulator.rpc(
            "monitoring",
            &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            "application/x-amz-json-1.0",
            &json_body(params),
        ),
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {text}"
    );
}

/// The form fields of a request as the JSON body carrying the same members.
fn json_body(params: &[(&str, &str)]) -> Value {
    let mut body = json!({});
    for (name, value) in params {
        let scalar = scalar_of(value);
        match name.split_once(".member.") {
            Some((member, rest)) => {
                let (index, field) = match rest.split_once('.') {
                    Some((index, field)) => (index, Some(field)),
                    None => (rest, None),
                };
                let at = index.parse::<usize>().unwrap_or(1).saturating_sub(1);
                if !body[member].is_array() {
                    body[member] = json!([]);
                }
                if let Some(held) = body[member].as_array_mut() {
                    while held.len() <= at {
                        held.push(json!({}));
                    }
                    match field {
                        Some(field) => held[at][field] = scalar,
                        None => held[at] = scalar,
                    }
                }
            }
            None => match name.split_once('.') {
                Some((member, field)) => body[member][field] = scalar,
                None => body[*name] = scalar,
            },
        }
    }
    body
}

/// One form field's value as the JSON scalar carrying the same value: an integral field becomes
/// an integer, a fractional one a double, a boolean a boolean, and everything else the text.
fn scalar_of(value: &str) -> Value {
    if let Ok(whole) = value.parse::<i64>() {
        return Value::from(whole);
    }
    if let Ok(fractional) = value.parse::<f64>() {
        return Value::from(fractional);
    }
    match value {
        "true" => Value::from(true),
        "false" => Value::from(false),
        other => Value::from(other),
    }
}

/// The alarm `name` as `DescribeAlarms` reports it, read over the RPC wire so ONE reader serves
/// every row whichever wire that row wrote over.
fn described(emulator: &Started, case: &str, name: &str) -> Value {
    let (status, text) = emulator.rpc(
        "monitoring",
        &format!("{CLOUDWATCH_TARGET_PREFIX}.DescribeAlarms"),
        "application/x-amz-json-1.0",
        &json!({"AlarmNames": [name]}),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAlarms, got {status} with {text}"
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    parsed["MetricAlarms"][0].clone()
}

/// The `Action` items the alarm `name`'s history holds, newest first.
fn action_history(emulator: &Started, case: &str, name: &str) -> Vec<Value> {
    let (status, text) = emulator.rpc(
        "monitoring",
        &format!("{CLOUDWATCH_TARGET_PREFIX}.DescribeAlarmHistory"),
        "application/x-amz-json-1.0",
        &json!({
            "AlarmName": name,
            "HistoryItemType": "Action",
            "ScanBy": "TimestampDescending",
        }),
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAlarmHistory, got {status} with {text}"
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    parsed["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Publish one breaching datapoint of the row's series, stamped `at`.
fn publish(emulator: &Started, row: &Row, metric: &str, at: f64) {
    let timestamp = match row.wire {
        Wire::Form => iso8601(at, row.stamp),
        // The RPC codec reads an epoch instant for a `Timestamp`, which is what that wire's
        // client sends.
        Wire::Json => format!("{at}"),
    };
    cloudwatch_write(
        emulator,
        row.case,
        row.wire,
        "PutMetricData",
        &[
            ("Namespace", NAMESPACE),
            ("MetricData.member.1.MetricName", metric),
            ("MetricData.member.1.Timestamp", &timestamp),
            ("MetricData.member.1.Value", BREACHING_VALUE),
            ("MetricData.member.1.Unit", "Count"),
        ],
    );
}

/// The warm-up window one alarm is written with: how long it lasts, and whether the alarm sits
/// the whole period out or ends it as soon as its evaluation window is filled.
#[derive(Debug, Clone, Copy)]
struct WarmUp {
    minutes: &'static str,
    only_after_the_period_ends: bool,
}

/// Write one alarm over the row's series, with `warm_up` when the arm carries one.
fn put_alarm(
    emulator: &Started,
    row: &Row,
    name: &str,
    metric: &str,
    topic: &str,
    warm_up: Option<WarmUp>,
) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AlarmName", name),
        ("Namespace", NAMESPACE),
        ("MetricName", metric),
        ("Statistic", "Maximum"),
        ("Period", "60"),
        ("EvaluationPeriods", "1"),
        ("DatapointsToAlarm", "1"),
        ("Threshold", THRESHOLD),
        ("ComparisonOperator", "GreaterThanThreshold"),
        ("ActionsEnabled", "true"),
        ("AlarmActions.member.1", topic),
    ];
    if let Some(warm_up) = warm_up {
        params.push((
            "WarmUpConfiguration.WarmUpPeriodDurationInMinutes",
            warm_up.minutes,
        ));
        params.push((
            "WarmUpConfiguration.OnlyStartEvaluatingAfterWarmUpPeriodEnds",
            if warm_up.only_after_the_period_ends {
                "true"
            } else {
                "false"
            },
        ));
    }
    cloudwatch_write(emulator, row.case, row.wire, "PutMetricAlarm", &params);
}

/// Take every notification waiting on the queue at `url`, each unwrapped from the SNS envelope it
/// arrived in, deleting what it hands over so a later leg reads only what arrived since.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<(Value, Value)> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut taken = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        });
        let inner = envelope["Message"].as_str().unwrap_or_default();
        let notification: Value = serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: the message is not an alarm notification ({error}): {inner}")
        });
        taken.push((envelope, notification));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            );
        }
    }
    taken
}

/// Wait until the queue holds a notification, re-publishing the breaching datapoint each turn the
/// way a client watching its alarm does — a wait for the CONDITION, paced by the long poll itself
/// rather than by a sleep.
fn drain_until_notified(
    emulator: &Started,
    row: &Row,
    metric: &str,
    url: &str,
) -> Vec<(Value, Value)> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        publish(emulator, row, metric, now_secs());
        let held = drain(emulator, row.case, url);
        if !held.is_empty() || Instant::now() >= giving_up_at {
            return held;
        }
    }
}

/// The estate one row drives: a topic, a queue subscribed to it whose policy grants SNS the send,
/// and the series the row's alarms watch, already breaching over `filled_periods` closed periods.
/// Answers the topic's ARN and the queue's url.
fn given_estate(emulator: &Started, row: &Row, name: &str, metric: &str) -> (String, String) {
    let case = row.case;
    let topic = element(&sns(emulator, case, "CreateTopic", &[("Name", name)]), "TopicArn");
    let created = sqs(emulator, case, "CreateQueue", &json!({"QueueName": name}));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": {"Policy": row.grant.policy(&queue_arn, &topic)},
        }),
    );
    sns(
        emulator,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    );
    // The series as it stands BEFORE any alarm watches it: the middle of each closed period the
    // row says was already breaching, and one in the period the clock is in.
    let newest_closed_start = (now_secs() / PERIOD_SECS).floor() * PERIOD_SECS - PERIOD_SECS;
    for back in 0..row.filled_periods {
        let at = newest_closed_start - PERIOD_SECS * f64::from(back) + PERIOD_SECS / 2.0;
        publish(emulator, row, metric, at);
    }
    publish(emulator, row, metric, now_secs());
    (topic, url)
}

/// RULE: a client-written alarm over a breaching series is moved to `ALARM` by its own evaluation,
/// and that transition executes the SNS action its `AlarmActions` names — one envelope on the
/// subscribed queue and one `Action` item in the alarm's history — while an arm inside its warm-up
/// window withholds both.
#[test]
fn an_evaluation_driven_alarms_action_reaches_its_queue_and_a_warm_up_withholds_it() {
    let rows: &[Row] = &[
        Row {
            case: "the awsQuery form wire, microsecond time stamps, the grant scoped to the \
                   publishing topic, one closed period already breaching",
            wire: Wire::Form,
            stamp: Stamp::Microsecond,
            grant: Grant::ScopedToTopic,
            filled_periods: 1,
        },
        Row {
            case: "the awsJson1_0 wire, whole-second time stamps, an unscoped grant, three \
                   closed periods already breaching",
            wire: Wire::Json,
            stamp: Stamp::WholeSecond,
            grant: Grant::Unscoped,
            filled_periods: 3,
        },
    ];

    // ONE started emulator for the whole table: each row writes an estate of its own, so nothing
    // of one row reaches another, and the rows are driven against the same served process.
    let emulator = Started::start();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("eval-driven-{index}");
        let metric = format!("Errors{index}");
        let (topic, url) = given_estate(&emulator, row, &name, &metric);
        let control = format!("{name}-control");
        let gated = format!("{name}-gated");
        put_alarm(&emulator, row, &control, &metric, &topic, None);
        put_alarm(
            &emulator,
            row,
            &gated,
            &metric,
            &topic,
            Some(WarmUp {
                minutes: LONGEST_WARM_UP_MINUTES,
                only_after_the_period_ends: true,
            }),
        );

        // The control carries no warm-up, so its own evaluation of the stored datapoints is what
        // moves it. NOTHING of CloudWatch is read while the delivery is waited for: a client that
        // has written its alarm publishes into the series and watches the TARGET, so the only
        // calls this wait makes are the publish and the queue's own poll — a transition that
        // only a CloudWatch READ materialised would never reach the queue here.
        let delivered = drain_until_notified(&emulator, row, &metric, &url);
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: that transition must reach the subscribed queue once, got \
             {delivered:?}"
        );
        let (envelope, notification) = &delivered[0];
        assert_eq!(
            envelope["Type"].as_str(),
            Some("Notification"),
            "case {case:?}: the queue receives the notification envelope, got {envelope}"
        );
        assert_eq!(
            notification["AlarmName"].as_str(),
            Some(control.as_str()),
            "case {case:?}: the notification must name the alarm that produced it, got \
             {notification}"
        );
        assert_eq!(
            notification["NewStateValue"].as_str(),
            Some("ALARM"),
            "case {case:?}: the notification must carry the state moved to, got {notification}"
        );
        assert_eq!(
            notification["OldStateValue"].as_str(),
            Some("INSUFFICIENT_DATA"),
            "case {case:?}: the notification must carry the state moved out of, got \
             {notification}"
        );

        // Only now is the alarm itself read: it stands in the state the notification announced,
        // and it reports no evaluation state, carrying no warm-up.
        let moved = described(&emulator, case, &control);
        assert_eq!(
            moved["StateValue"].as_str(),
            Some("ALARM"),
            "case {case:?}: the breaching series must move the control alarm, got {moved}"
        );
        assert!(
            moved.get("EvaluationState").is_none_or(Value::is_null),
            "case {case:?}: an alarm carrying no warm-up reports no evaluation state, got {moved}"
        );

        // The alarm's own account of that execution: one `Action` item naming the topic the
        // publish went to and reporting it as taken.
        let items = action_history(&emulator, case, &control);
        let executed = items.first().unwrap_or_else(|| {
            panic!("case {case:?}: the delivered action must be recorded, got {items:?}")
        });
        let data: Value = serde_json::from_str(
            executed["HistoryData"].as_str().unwrap_or_default(),
        )
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the action record is not a document ({error}): {executed}")
        });
        assert_eq!(
            data["actionState"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the recorded action must report the publish it made, got {executed}"
        );
        assert!(
            executed["HistorySummary"]
                .as_str()
                .is_some_and(|summary| summary.contains(&topic)),
            "case {case:?}: the recorded action must name the topic it published to ({topic}), \
             got {executed}"
        );

        // The gated arm watches the SAME breaching series and carries the SAME action, and its
        // warm-up withholds both the transition and the action: read only now, after the
        // control's own delivery has arrived, so an empty queue is an absence rather than a
        // delivery still in flight.
        let withheld = described(&emulator, case, &gated);
        assert_eq!(
            withheld["StateValue"].as_str(),
            Some("INSUFFICIENT_DATA"),
            "case {case:?}: an alarm inside its warm-up stays in insufficient data, got \
             {withheld}"
        );
        assert_eq!(
            withheld["EvaluationState"].as_str(),
            Some(IN_WARM_UP),
            "case {case:?}: an alarm inside its warm-up reports that it is, got {withheld}"
        );
        let after = drain(&emulator, case, &url);
        assert!(
            after.is_empty(),
            "case {case:?}: nothing further may be delivered while the gated arm is warming up, \
             got {after:?}"
        );
    }
}

/// Wait until every alarm in `wanted` has had a notification delivered, re-publishing the
/// breaching datapoint each turn and reading nothing of CloudWatch meanwhile. Answers the alarms
/// that were notified, in the order their notifications arrived.
fn drain_until_each_notified(
    emulator: &Started,
    row: &Row,
    metric: &str,
    url: &str,
    wanted: &[&str],
) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut notified: Vec<String> = Vec::new();
    loop {
        publish(emulator, row, metric, now_secs());
        for (_, notification) in drain(emulator, row.case, url) {
            if let Some(name) = notification["AlarmName"].as_str() {
                if !notified.iter().any(|seen| seen == name) {
                    notified.push(name.to_string());
                }
            }
        }
        let all = wanted
            .iter()
            .all(|name| notified.iter().any(|seen| seen == name));
        if all || Instant::now() >= giving_up_at {
            return notified;
        }
    }
}

/// RULE: a warm-up window withholds the transition and its action only WHILE IT IS IN FORCE —
/// once it is over the alarm is handed back to its own grading and the action it withheld is
/// executed, so the notification arrives late rather than never. "After you create or update the
/// alarm, the alarm stays in INSUFFICIENT_DATA for this duration. During this time, the alarm does
/// not perform alarm actions."
/// (smithy_query(query="shape",shape="WarmUpConfiguration")) — and an arm that does not sit the
/// period out releases sooner: "If `false`, the alarm ends the warm-up period early. Evaluation
/// begins as soon as the alarm has enough metric data to fill its evaluation window."
/// (`WarmUpConfiguration$OnlyStartEvaluatingAfterWarmUpPeriodEnds`), which over a series that is
/// already breaching is at once.
///
/// The two arms differ along exactly that flag, over one series and one topic, so what separates
/// them is the gate and nothing else.
#[test]
fn a_warm_up_releases_the_action_it_withheld_once_it_is_over() {
    let row = Row {
        case: "the shortest warm-up the model admits, on the awsQuery form wire",
        wire: Wire::Form,
        stamp: Stamp::Microsecond,
        grant: Grant::ScopedToTopic,
        filled_periods: 1,
    };
    let emulator = Started::start();
    let name = "warm-up-release";
    let metric = "ErrorsRelease";
    let (topic, url) = given_estate(&emulator, &row, name, metric);
    let early = format!("{name}-early");
    let gated = format!("{name}-gated");
    put_alarm(
        &emulator,
        &row,
        &early,
        metric,
        &topic,
        Some(WarmUp {
            minutes: SHORTEST_WARM_UP_MINUTES,
            only_after_the_period_ends: false,
        }),
    );
    put_alarm(
        &emulator,
        &row,
        &gated,
        metric,
        &topic,
        Some(WarmUp {
            minutes: SHORTEST_WARM_UP_MINUTES,
            only_after_the_period_ends: true,
        }),
    );

    // Both arms are released in the end: the early one as soon as its evaluation window is
    // filled, the gated one once its window has passed. Nothing of CloudWatch is read while the
    // deliveries are waited for.
    let notified = drain_until_each_notified(&emulator, &row, metric, &url, &[&early, &gated]);
    assert!(
        notified.contains(&early),
        "the arm that does not sit its warm-up out must have its action executed, got {notified:?}"
    );
    assert!(
        notified.contains(&gated),
        "the arm that sits its warm-up out must have the action it withheld executed once that \
         window is over, got {notified:?}"
    );
    assert_eq!(
        notified.first().map(String::as_str),
        Some(early.as_str()),
        "the arm that ends its warm-up early is released first, got {notified:?}"
    );

    // Both arms now report the state their notifications announced, and neither reports a warm-up
    // any more: the window each was in is over.
    for arm in [&early, &gated] {
        let released = described(&emulator, row.case, arm);
        assert_eq!(
            released["StateValue"].as_str(),
            Some("ALARM"),
            "the released arm {arm:?} must stand in the state it announced, got {released}"
        );
        assert!(
            released.get("EvaluationState").is_none_or(Value::is_null),
            "an alarm whose warm-up is over reports no warm-up state, got {released}"
        );
    }
}
