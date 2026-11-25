// SPDX-License-Identifier: CC0-1.0

//! A metric alarm a CLIENT writes and FORCES reaches the queue subscribed to its topic, and its
//! `ActionsEnabled` switch decides whether it does — on the emulator a client STARTS: the built
//! binary, on its own port, with request authentication on, reached over TLS with SigV4-signed
//! calls.
//!
//! The RULE under test: **a metric alarm whose `AlarmActions` names an SNS topic executes that
//! action on every transition INTO the ALARM state, so a queue subscribed to the topic (and
//! granting the SNS service principal the send) receives exactly one message per transition whose
//! `Message` document carries the alarm's name, `NewStateValue` = `ALARM` and `NewStateReason` =
//! the reason the call that made the transition supplied; while the alarm's `ActionsEnabled` is
//! false the transition still happens and the publish is withheld, and `EnableAlarmActions`
//! restores it for the next transition.** `SetAlarmState` is a transition like any other here:
//! "When the updated state differs from the previous value, the action configured for the
//! appropriate state is invoked. For example, if your alarm is configured to send an Amazon SNS
//! message when an alarm is triggered, temporarily changing the alarm state to ALARM sends an SNS
//! message"
//! (`smithy_query(query="operation",operation="SetAlarmState")`), and `ActionsEnabled` is the
//! switch: "Indicates whether actions should be executed during any changes to the alarm state"
//! (`MetricAlarm$ActionsEnabled`).
//!
//! What only this suite reaches: the whole estate written BY A CLIENT — the topic, the queue, its
//! policy, the `sqs` subscription and the alarm itself, each over the wire an SDK addresses that
//! service on — against the emulator as it is SERVED. The sibling suites pin the other axes:
//! [`crate::cloudwatch_alarm_action_reaches_its_subscribed_queue`] drives the same contrast
//! in-process over the whole mounted service set with authentication off, and
//! [`crate::cloudwatch_cfn_alarm_notification_on_a_started_emulator`] pins a CloudFormation-deployed
//! estate on the started binary. Neither drives a client's own writes through the started
//! emulator, and neither puts an alarm's action list on the `awsQuery` FORM wire, where a list
//! member arrives as `AlarmActions.member.1` rather than as a JSON array — so an action list the
//! form codec dropped, or a delivery only the in-process router makes, is a failure a test
//! reports rather than one only a client sees.
//!
//! The rows differ along what the rule generalises over: the wire the client drives CloudWatch
//! over (the `awsQuery` form body a query-protocol SDK sends, and the `awsJson1_0` body the RPC
//! one sends — both protocols this service's model declares) and how the subscribed queue's
//! policy scopes its grant to SNS (unscoped, or conditioned on the publishing topic). Every wait
//! is on a CONDITION, and the absence leg is read only after the suppressed transition is
//! VISIBLE on the alarm, so an empty queue there is an absence rather than a delivery still in
//! flight.

use std::time::{Duration, Instant};

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

/// How long a wait for a delivery, for a state or for the switch lasts. Generous: every wait is
/// on the CONDITION it is waiting for, so a loaded host is given room rather than failed.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one `ReceiveMessage` long-polls, which is what a consumer of such a queue does. It is
/// also what bounds the one poll an absence leg makes.
const WAIT_TIME_SECONDS: u32 = 2;

/// Which of the two protocols this service's model declares a row drives CloudWatch over.
#[derive(Debug, Clone, Copy)]
enum Wire {
    /// The `awsQuery` form body, where an action list arrives as `AlarmActions.member.1`.
    Form,
    /// The `awsJson1_0` body, where it arrives as a JSON array.
    Json,
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
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Amazon SNS call, over the `awsQuery` form wire that service serves.
fn sns(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", SNS_VERSION, action, params);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    text
}

/// One CloudWatch call over the row's wire, answered as the text its protocol rendered.
///
/// `params` are the form fields of the query call; the same request is sent as a JSON object for
/// a row on the RPC wire, so one row's request can never be a different request from another's.
fn cloudwatch(
    emulator: &Started,
    case: &str,
    wire: Wire,
    operation: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = match wire {
        Wire::Form => emulator.form_call("monitoring", CLOUDWATCH_VERSION, operation, params),
        Wire::Json => emulator.rpc(
            "monitoring",
            &format!("GraniteServiceVersion20100801.{operation}"),
            "application/x-amz-json-1.0",
            &json_body(params),
        ),
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {text}; \
         the log:\n{}",
        emulator.log()
    );
    text
}

/// The form fields of a request as the JSON body carrying the same members: an indexed
/// `<Member>.member.<n>` field becomes an element of the array member `<Member>`, and a numeric
/// field becomes a number, which is what the JSON codec reads an `Integer` or a `Double` as.
fn json_body(params: &[(&str, &str)]) -> Value {
    let mut body = json!({});
    for (name, value) in params {
        // An integral field becomes an integer and a fractional one a double, so a member the
        // model types `Integer` is never handed a fractional number.
        let scalar = value.parse::<i64>().map(Value::from).unwrap_or_else(|_| {
            value
                .parse::<f64>()
                .map_or_else(|_| Value::from(*value), Value::from)
        });
        match name.split_once(".member.") {
            Some((member, _)) => {
                if !body[member].is_array() {
                    body[member] = json!([]);
                }
                if let Some(held) = body[member].as_array_mut() {
                    held.push(scalar);
                }
            }
            None => body[*name] = scalar,
        }
    }
    body
}

/// Read one member of the alarm `name` as `DescribeAlarms` reports it, whichever wire the row
/// drives: the XML element and the JSON member carry the same text.
fn described(emulator: &Started, case: &str, wire: Wire, name: &str, member: &str) -> String {
    let body = cloudwatch(
        emulator,
        case,
        wire,
        "DescribeAlarms",
        &[("AlarmNames.member.1", name)],
    );
    match wire {
        Wire::Form => element(&body, member),
        Wire::Json => {
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let reported = &parsed["MetricAlarms"][0][member];
            reported
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| reported.to_string())
        }
    }
}

/// Look at one member of the alarm until it reports `wanted`, and answer what it reported last —
/// a wait for the CONDITION, so a caller asserts on what the alarm says rather than on a span.
fn until_reported(
    emulator: &Started,
    case: &str,
    wire: Wire,
    name: &str,
    member: &str,
    wanted: &str,
) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let reported = described(emulator, case, wire, name, member);
        if reported == wanted || Instant::now() >= giving_up_at {
            return reported;
        }
    }
}

/// Force the alarm `name` into `state`, with `reason` as the reason the transition reports.
fn force(emulator: &Started, case: &str, wire: Wire, name: &str, state: &str, reason: &str) {
    cloudwatch(
        emulator,
        case,
        wire,
        "SetAlarmState",
        &[
            ("AlarmName", name),
            ("StateValue", state),
            ("StateReason", reason),
        ],
    );
}

/// Turn the alarm's actions on or off and answer what `DescribeAlarms` then reports for
/// `ActionsEnabled`.
fn switch_actions(
    emulator: &Started,
    case: &str,
    wire: Wire,
    name: &str,
    enabled: bool,
) -> String {
    let operation = if enabled {
        "EnableAlarmActions"
    } else {
        "DisableAlarmActions"
    };
    cloudwatch(
        emulator,
        case,
        wire,
        operation,
        &[("AlarmNames.member.1", name)],
    );
    described(emulator, case, wire, name, "ActionsEnabled")
}

/// Take every notification waiting on the queue at `url`, unwrapped from the envelope SNS
/// delivers it in, deleting what it hands over so a later leg reads only what arrived since.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
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
    let mut notifications = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        });
        let inner = envelope["Message"].as_str().unwrap_or_default();
        notifications.push(serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: the message is not an alarm notification ({error}): {inner}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            );
        }
    }
    notifications
}

/// Drain until at least one notification has arrived, or the deadline passes.
fn drain_until_notified(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let held = drain(emulator, case, url);
        if !held.is_empty() || Instant::now() >= giving_up_at {
            return held;
        }
    }
}

/// How many messages the queue at `url` reports as available.
fn available(emulator: &Started, case: &str, url: &str) -> String {
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": ["ApproximateNumberOfMessages"],
        }),
    );
    attributes["Attributes"]["ApproximateNumberOfMessages"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The estate one row drives: a topic, a queue subscribed to it whose policy grants SNS the send,
/// and the alarm whose `AlarmActions` is that topic. Answers the topic's ARN and the queue's url.
fn given_estate(
    emulator: &Started,
    case: &str,
    wire: Wire,
    name: &str,
    grant: Grant,
) -> (String, String) {
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
            "Attributes": {"Policy": grant.policy(&queue_arn, &topic)},
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
    // The alarm the row forces: a static threshold over a series nothing publishes into, so the
    // only thing that moves its state is the force itself.
    cloudwatch(
        emulator,
        case,
        wire,
        "PutMetricAlarm",
        &[
            ("AlarmName", name),
            ("Namespace", "Started/ClientWritten"),
            ("MetricName", "Errors"),
            ("Statistic", "Average"),
            ("Period", "60"),
            ("EvaluationPeriods", "1"),
            ("Threshold", "1.0"),
            ("ComparisonOperator", "GreaterThanThreshold"),
            ("TreatMissingData", "missing"),
            ("AlarmActions.member.1", &topic),
        ],
    );
    (topic, url)
}

/// One notification is the alarm's own, carrying the state it moved to and the reason the call
/// that moved it supplied.
fn assert_notification(case: &str, notification: &Value, alarm: &str, reason: &str) {
    assert_eq!(
        notification["AlarmName"].as_str(),
        Some(alarm),
        "case {case:?}: the notification must name the alarm that produced it, got {notification}"
    );
    assert_eq!(
        notification["NewStateValue"].as_str(),
        Some("ALARM"),
        "case {case:?}: the notification must carry the state moved to, got {notification}"
    );
    assert_eq!(
        notification["NewStateReason"].as_str(),
        Some(reason),
        "case {case:?}: the notification must carry the reason the force supplied, got \
         {notification}"
    );
}

/// RULE: a client-written metric alarm forced into `ALARM` publishes its notification to the
/// topic its `AlarmActions` names, which the subscribed queue receives once — and only while the
/// alarm's `ActionsEnabled` is true, the transition itself happening either way.
#[test]
fn a_forced_alarms_action_reaches_its_queue_and_its_switch_decides_whether_it_does() {
    // (case, the wire the client drives CloudWatch over, how the queue grants SNS the send)
    let rows: &[(&str, Wire, Grant)] = &[
        (
            "the awsQuery form wire, the grant scoped to the publishing topic",
            Wire::Form,
            Grant::ScopedToTopic,
        ),
        (
            "the awsJson1_0 wire, an unscoped grant",
            Wire::Json,
            Grant::Unscoped,
        ),
    ];

    // ONE started emulator for the whole table: each row writes an estate of its own, so nothing
    // of one row reaches another, and the rows are driven against the same served process.
    let emulator = Started::start();

    for (index, (case, wire, grant)) in rows.iter().enumerate() {
        let alarm = format!("client-forced-{index}");
        let (_, url) = given_estate(&emulator, case, *wire, &alarm, *grant);

        // The positive control: the actions are on by default, so the transition into ALARM
        // publishes to the topic and the subscribed queue receives exactly that notification.
        force(&emulator, case, *wire, &alarm, "ALARM", "baseline");
        let delivered = drain_until_notified(&emulator, case, &url);
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the forced transition must reach the subscribed queue once, got \
             {delivered:?}; the log:\n{}",
            emulator.log()
        );
        assert_notification(case, &delivered[0], &alarm, "baseline");
        let settled = drain(&emulator, case, &url);
        assert!(
            settled.is_empty(),
            "case {case:?}: one transition owes one notification, got {settled:?} after it"
        );

        // With the switch off the state still moves and the publish is withheld. The force into
        // OK before each ALARM force is what makes that force a state CHANGE whatever the alarm's
        // own evaluation left behind; it fires nothing itself, this alarm carrying no `OKActions`.
        let reported = switch_actions(&emulator, case, *wire, &alarm, false);
        assert_eq!(
            reported, "false",
            "case {case:?}: DisableAlarmActions must turn the switch off, got {reported:?}"
        );
        force(&emulator, case, *wire, &alarm, "OK", "cleared while suppressed");
        force(&emulator, case, *wire, &alarm, "ALARM", "suppressed");
        // The suppressed transition is VISIBLE on the alarm before the queue is read, so an empty
        // queue is an absence rather than a delivery still in flight.
        let moved = until_reported(&emulator, case, *wire, &alarm, "StateReason", "suppressed");
        assert_eq!(
            moved, "suppressed",
            "case {case:?}: the suppressed transition must still move the alarm, got {moved:?}"
        );
        let withheld = drain(&emulator, case, &url);
        assert!(
            withheld.is_empty(),
            "case {case:?}: nothing may be delivered while the actions are off, got \
             {withheld:?}; the log:\n{}",
            emulator.log()
        );
        let waiting = available(&emulator, case, &url);
        assert_eq!(
            waiting, "0",
            "case {case:?}: the subscribed queue must hold nothing while the actions are off, \
             got {waiting:?} messages"
        );

        // And back on: the next transition into ALARM is delivered, carrying ITS own reason, and
        // the one withheld while the actions were off is not delivered late.
        let reported = switch_actions(&emulator, case, *wire, &alarm, true);
        assert_eq!(
            reported, "true",
            "case {case:?}: EnableAlarmActions must turn the switch back on, got {reported:?}"
        );
        force(&emulator, case, *wire, &alarm, "OK", "cleared before the restored force");
        force(&emulator, case, *wire, &alarm, "ALARM", "restored");
        let restored = drain_until_notified(&emulator, case, &url);
        assert_eq!(
            restored.len(),
            1,
            "case {case:?}: the transition made with the actions back on must be delivered, and \
             only that one, got {restored:?}; the log:\n{}",
            emulator.log()
        );
        assert_notification(case, &restored[0], &alarm, "restored");
    }
}
