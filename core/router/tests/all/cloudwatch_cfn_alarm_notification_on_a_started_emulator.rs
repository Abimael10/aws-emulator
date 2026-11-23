// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed alarm's SNS action reaches the subscribed queue on the emulator a
//! client STARTS: the built binary, on its own port, with request authentication on, reached
//! over TLS with SigV4-signed calls.
//!
//! The RULE under test: **a metric alarm moved into `ALARM` executes the `AlarmActions`
//! configured for that state, so an SNS topic ARN among them is published the alarm's
//! notification, and the topic hands that notification to every confirmed subscription — an
//! `sqs` subscription in envelope mode receiving one SQS message whose `Body` is the SNS
//! notification envelope and whose `Message` is the alarm document naming the alarm and the
//! state it moved to — for a topic whose access policy permits that publish, and for no topic
//! whose policy withholds it, where the withheld publish is accounted for in the alarm's own
//! history instead.** Which identifiers the estate's resources carry is no part of that rule: a
//! template that NAMES its queue and topic and one that leaves CloudFormation to generate their
//! physical names deliver alike.
//!
//! [`crate::cloudwatch_cfn_alarm_action_topic_policy`] pins the policy half over a router
//! assembled inside the test process, with authentication off, on a runtime the test chose. The
//! emulator a client runs is a different configuration of the same code: every call is verified,
//! the publish and the fan-out are made through the callback of THAT process on its own
//! multi-thread runtime, and the consumer LONG-POLLS the queue the way a client waiting for an
//! alarm notification does. This suite is the one that pins the whole chain THERE, so an alarm
//! notification that is delivered in-process and not on the started emulator is a failure a test
//! reports rather than one only a client sees.
//!
//! The rows differ along what the rule generalises over: whether the template declares the
//! queue's and the topic's names or leaves them to CloudFormation, and whether the stack declares
//! a topic policy that withholds the publish. Every row's estate stands while every other row's
//! alarm is forced, and a row that expects nothing is read only after a row that expects a
//! delivery has observed one and after the withholding alarm's own history item is there — so an
//! empty queue is an absence rather than a delivery still in flight.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};

/// The service principal SNS performs its delivery to a subscribed queue as, which the queue's
/// own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How the alarm's history words a publish no document allowed, as the recorded `error` carries
/// it: the action that was refused.
const REFUSED_ACTION: &str = "not authorized to perform: SNS:Publish";

/// How long a wait for a delivery or for a history item lasts. Generous: every wait is on the
/// CONDITION, so a loaded host is given room rather than failed.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often a wait that is not itself a long poll looks again.
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long one `ReceiveMessage` long-polls, which is what a consumer of such a queue does.
const WAIT_TIME_SECONDS: u32 = 5;

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One CloudWatch call that must answer 200.
fn cloudwatch(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "monitoring",
        &format!("GraniteServiceVersion20100801.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// The policy document a withholding row's `AWS::SNS::TopicPolicy` declares: one least-privilege
/// statement whose action list omits the publish, so the only thing it decides is that the
/// publish is not granted. The action names are Amazon SNS's own as a POLICY spells them.
fn least_privilege_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DeclaredByTheStack",
            "Effect": "Allow",
            "Principal": {"AWS": "*"},
            "Action": ["SNS:Subscribe", "SNS:Receive", "SNS:GetTopicAttributes"],
            "Resource": {"Ref": "Topic"},
        }],
    })
}

/// The template a row deploys: a queue, a topic the queue is subscribed to, the queue policy
/// that lets SNS write to it, an alarm whose `AlarmActions` is that topic, and — for a row that
/// declares one — the topic policy that decides the alarm's publish.
///
/// A row that declares no `QueueName` and no `TopicName` is deployed under the physical names
/// CloudFormation generates for them. Every reference between the resources is an intrinsic
/// (`Ref`, `Fn::GetAtt`), including the ones NESTED inside the two policy documents, so such a
/// row resolves every identifier through CloudFormation.
fn template(row: &Row, topic_policy: Option<Value>) -> String {
    let mut queue = json!({"Type": "AWS::SQS::Queue"});
    let mut topic = json!({"Type": "AWS::SNS::Topic"});
    if row.named {
        queue["Properties"] = json!({"QueueName": format!("{}-queue", row.stack())});
        topic["Properties"] = json!({"TopicName": format!("{}-topic", row.stack())});
    }
    let mut resources = json!({
        "Queue": queue,
        "Topic": topic,
        "Subscription": {
            "Type": "AWS::SNS::Subscription",
            "Properties": {
                "TopicArn": {"Ref": "Topic"},
                "Protocol": "sqs",
                "Endpoint": {"Fn::GetAtt": ["Queue", "Arn"]},
            },
        },
        "QueuePolicy": {
            "Type": "AWS::SQS::QueuePolicy",
            "Properties": {
                "Queues": [{"Ref": "Queue"}],
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Sid": "AllowTheTopicToSend",
                        "Effect": "Allow",
                        "Principal": {"Service": SNS_PRINCIPAL},
                        "Action": "sqs:SendMessage",
                        "Resource": {"Fn::GetAtt": ["Queue", "Arn"]},
                        "Condition": {"ArnEquals": {"aws:SourceArn": {"Ref": "Topic"}}},
                    }],
                },
            },
        },
        "Alarm": {
            "Type": "AWS::CloudWatch::Alarm",
            "Properties": {
                "AlarmName": row.alarm(),
                "Namespace": "Started/Behavioral",
                "MetricName": "check",
                "Statistic": "Average",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1,
                "ComparisonOperator": "GreaterThanThreshold",
                "TreatMissingData": "notBreaching",
                "AlarmActions": [{"Ref": "Topic"}],
            },
        },
    });
    if let Some(document) = topic_policy {
        resources["TopicPolicy"] = json!({
            "Type": "AWS::SNS::TopicPolicy",
            "Properties": {"Topics": [{"Ref": "Topic"}], "PolicyDocument": document},
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an alarm, the topic it notifies and the queue subscribed to it",
        "Resources": resources,
        "Outputs": {
            "QueueUrl": {"Value": {"Ref": "Queue"}},
            "TopicArn": {"Value": {"Ref": "Topic"}},
        },
    })
    .to_string()
}

/// Every alarm notification waiting on the queue at `url`, as the document SNS enveloped, read
/// the way a consumer reads it: one LONG-POLLING receive.
fn arrived(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
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
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
            });
            let inner = envelope["Message"].as_str().unwrap_or_default();
            serde_json::from_str(inner).unwrap_or_else(|error| {
                panic!("case {case:?}: the message is not an alarm notification ({error}): {inner}")
            })
        })
        .collect()
}

/// Look for the notifications on the queue at `url` until `wanted` have arrived or the deadline
/// passes, reading it at least once — so a row expecting NOTHING has really read the queue rather
/// than asserting about a queue nobody looked at.
fn arrived_until(emulator: &Started, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    let mut held = arrived(emulator, case, url);
    while held.len() < wanted && Instant::now() < giving_up_at {
        held.extend(arrived(emulator, case, url));
    }
    held
}

/// The `HistoryData` document of the newest `Action` item the alarm `name` recorded, once it has
/// recorded one: the record is written as the action completes, so this reads it the way a client
/// polling the history does.
fn settled_action_record(emulator: &Started, case: &str, name: &str) -> Value {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let history = cloudwatch(
            emulator,
            case,
            "DescribeAlarmHistory",
            &json!({"AlarmName": name, "HistoryItemType": "Action"}),
        );
        let data = history["AlarmHistoryItems"][0]["HistoryData"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if let Ok(record) = serde_json::from_str::<Value>(&data) {
            return record;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the alarm {name} must record the action it executed, got {history}; \
             the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// One row: how the estate is declared, and how many notifications its subscribed queue is owed.
struct Row {
    case: &'static str,
    /// The row's own suffix, so no two rows share a stack, an alarm or a physical name.
    suffix: &'static str,
    /// Whether the template declares the queue's and the topic's names.
    named: bool,
    /// Whether the stack declares the least-privilege topic policy that withholds the publish.
    withholds: bool,
    /// How many notifications the queue subscribed to the topic receives.
    delivered: usize,
}

impl Row {
    fn stack(&self) -> String {
        format!("started-alarm-{}", self.suffix)
    }

    fn alarm(&self) -> String {
        format!("started-alarm-{}-alarm", self.suffix)
    }
}

/// The rows, spanning how the estate is identified and whether the publish is granted.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a stack naming its queue and topic, keeping the topic's own default policy",
            suffix: "named",
            named: true,
            withholds: false,
            delivered: 1,
        },
        Row {
            case: "a stack leaving the queue's and the topic's names to CloudFormation",
            suffix: "generated",
            named: false,
            withholds: false,
            delivered: 1,
        },
        Row {
            case: "a named estate whose declared topic policy omits the publish",
            suffix: "named-restricted",
            named: true,
            withholds: true,
            delivered: 0,
        },
        Row {
            case: "a generated-name estate whose declared topic policy omits the publish",
            suffix: "generated-restricted",
            named: false,
            withholds: true,
            delivered: 0,
        },
    ]
}

/// What one row's deployment published: the queue a notification would arrive on, and the topic
/// the alarm's action names.
struct Deployed {
    queue_url: String,
    topic_arn: String,
}

/// RULE: a deployed alarm forced into `ALARM` publishes its notification to the topic its
/// `AlarmActions` names, which the subscribed queue receives, whichever way the estate's
/// resources are identified — and a topic whose declared policy withholds the publish delivers
/// nothing and says so in the alarm's history.
#[test]
fn a_deployed_alarms_notification_reaches_its_queue_on_the_emulator_a_client_starts() {
    let emulator = Started::start();
    let rows = rows();

    // Every row's estate is deployed first, so each row's alarm is forced with every other row's
    // estate already standing — the position a client that deploys a control stack and a
    // restricted one beside it is in.
    let mut published: Vec<Deployed> = Vec::new();
    for row in &rows {
        let body = template(row, row.withholds.then(least_privilege_document));
        let outputs = deployed(&emulator, row.case, &row.stack(), &body);
        published.push(Deployed {
            queue_url: output(&outputs, row.case, "QueueUrl"),
            topic_arn: output(&outputs, row.case, "TopicArn"),
        });
    }

    // The withholding rows are forced FIRST, so a refusal taken for one alarm is in the
    // dispatcher's hands before any row that expects a delivery is forced at all.
    let forced_first = rows.iter().filter(|row| row.withholds);
    let forced_after = rows.iter().filter(|row| !row.withholds);
    for row in forced_first.chain(forced_after) {
        cloudwatch(
            &emulator,
            row.case,
            "SetAlarmState",
            &json!({
                "AlarmName": row.alarm(),
                "StateValue": "ALARM",
                "StateReason": "forced by the started-emulator suite",
            }),
        );
    }

    // The rows that expect a delivery are read first: their notification is the barrier that
    // makes a row expecting nothing an absence rather than a delivery still in flight.
    let delivering = rows.iter().zip(&published).filter(|(row, _)| row.delivered > 0);
    let withholding = rows.iter().zip(&published).filter(|(row, _)| row.delivered == 0);
    for (row, estate) in delivering.chain(withholding) {
        let case = row.case;
        // The alarm's own account of the action comes first either way: the dispatcher has
        // executed (or refused) this action by the time the item exists.
        let record = settled_action_record(&emulator, case, &row.alarm());
        let held = arrived_until(&emulator, case, &estate.queue_url, row.delivered);
        assert_eq!(
            held.len(),
            row.delivered,
            "case {case:?}: expected {} notification(s) on the subscribed queue, got {held:?}; \
             the log:\n{}",
            row.delivered,
            emulator.log()
        );
        for notification in &held {
            assert_eq!(
                notification["AlarmName"].as_str(),
                Some(row.alarm().as_str()),
                "case {case:?}: the notification must name the alarm that produced it, got \
                 {notification}"
            );
            assert_eq!(
                notification["NewStateValue"].as_str(),
                Some("ALARM"),
                "case {case:?}: the notification must carry the state moved to, got \
                 {notification}"
            );
        }
        if row.delivered > 0 {
            assert_eq!(
                record["actionState"].as_str(),
                Some("Succeeded"),
                "case {case:?}: an executed action is recorded as Succeeded, got {record}"
            );
            continue;
        }
        // Nothing arrived, and the alarm's own history says WHY: the publish was refused for this
        // service principal on this topic, so a client can tell a withheld notification from one
        // that was never owed.
        assert_eq!(
            record["actionState"].as_str(),
            Some("Failed"),
            "case {case:?}: a withheld action is recorded as Failed, got {record}"
        );
        let error = record["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(REFUSED_ACTION) && error.contains(&estate.topic_arn),
            "case {case:?}: the recorded error must name the refused publish and the topic {:?}, \
             got {error:?}",
            estate.topic_arn
        );
    }
}
