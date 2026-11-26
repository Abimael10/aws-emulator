// SPDX-License-Identifier: CC0-1.0

//! Which of a CloudFormation-deployed alarm's action lists fires, and HOW OFTEN, over the whole
//! life of the forced state — through the mounted service set `core/router/src/lib.rs` assembles
//! for the binary.
//!
//! The RULE under test: **a state transition of an alarm executes the actions configured for the
//! state the alarm moved TO, once per transition — not once per evaluation — so an SNS topic
//! among those actions is published exactly one notification for that transition and none for a
//! transition into a state whose own action list does not name it; and a topic whose access
//! policy states no `SNS:Publish` grant for the `cloudwatch.amazonaws.com` service principal is
//! published nothing, for any of those transitions, however many evaluations the clock
//! afterwards performs.**
//!
//! Three transitions of one deployed alarm are observed per row, which is what makes "once per
//! transition" an assertion rather than a count of one: the first evaluation of a freshly
//! deployed alarm moves it out of `INSUFFICIENT_DATA`, `SetAlarmState` then forces it into
//! `ALARM`, and the alarm's own next evaluation takes that forced state back
//! ("Metric alarms returns to their actual state quickly, often within seconds",
//! `SetAlarmState`) — after which the clock goes on evaluating the alarm, at its period, with
//! nothing more owed to any topic.
//!
//! The rows differ along which action list the stack points at the topic — `AlarmActions` only,
//! `OKActions` only, both — and, for the row that declares both, whether the topic policy the
//! same stack declares grants the publish at all. The sibling suites table the policy FORMS
//! (`cloudwatch_cfn_alarm_action_topic_policy`, and the crate-local
//! `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/sns_action_topic_policy.rs`)
//! and the same chain on the emulator a client STARTS
//! (`cloudwatch_cfn_alarm_notification_on_a_started_emulator`); what this one adds is WHICH
//! transitions a deployed estate owes its topic, and that a withheld publish is not retried by a
//! later evaluation.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region the whole wiring lives in, and so the Region the stacks belong to.
const REGION: &str = "us-west-2";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The service principal SNS performs its delivery to a subscribed queue as, which that queue's
/// own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// How the alarm's history words a publish no document allowed, as the recorded `error` carries
/// it.
const REFUSED_ACTION: &str = "not authorized to perform: SNS:Publish";

/// The alarm states a row reads and asserts on.
const OK: &str = "OK";
const ALARM: &str = "ALARM";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The period each row's alarm declares, in seconds: a high-resolution period, so the forced
/// state is taken back by the alarm's own next evaluation inside one short session rather than
/// at the end of a standard-resolution minute.
const PERIOD_SECONDS: u32 = 10;

/// How long a wait for a deployment, a state change or a delivery lasts, and how often it looks
/// again. Generous on purpose: every wait is on the CONDITION it is about, so a loaded host is
/// given room instead of being failed.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` form call, answering the HTTP status and the body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call, which must answer 200.
async fn cfn(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "cloudformation", "2010-05-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call, which must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch call (`awsJson1_0`).
async fn cw(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", operation, body).await
}

/// One row: which action lists the stack points at the topic, whether the policy it declares
/// grants the publish, and the transitions the subscribed queue is therefore owed.
struct Row {
    case: &'static str,
    /// The row's own suffix, so no two rows share a stack, an alarm or a physical name.
    suffix: &'static str,
    /// Whether the alarm's `AlarmActions` names the topic.
    alarm_actions: bool,
    /// Whether the alarm's `OKActions` names the topic.
    ok_actions: bool,
    /// Whether the stack declares a topic policy that WITHHOLDS the publish.
    withholds: bool,
    /// `(OldStateValue, NewStateValue)` of every notification the queue must receive, as a set:
    /// a standard queue promises no order, so the transitions are compared as a multiset.
    owed: &'static [(&'static str, &'static str)],
}

impl Row {
    fn stack(&self) -> String {
        format!("alarm-actions-{}", self.suffix)
    }

    fn alarm(&self) -> String {
        format!("alarm-actions-{}-alarm", self.suffix)
    }

    fn queue(&self) -> String {
        format!("alarm-actions-{}-queue", self.suffix)
    }

    fn topic(&self) -> String {
        format!("alarm-actions-{}-topic", self.suffix)
    }
}

/// The rows, spanning which action list names the topic and whether the publish is granted.
///
/// Every row's alarm goes through the SAME three transitions: the first evaluation of the freshly
/// deployed alarm (`INSUFFICIENT_DATA` -> `OK`, since `TreatMissingData` is `notBreaching` and
/// nothing is published into the series), the force into `ALARM`, and the evaluation that takes
/// the forced state back (`ALARM` -> `OK`). What differs is which of them the topic is owed.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the topic named by AlarmActions only",
            suffix: "alarm-only",
            alarm_actions: true,
            ok_actions: false,
            withholds: false,
            owed: &[(OK, ALARM)],
        },
        Row {
            case: "the topic named by OKActions only",
            suffix: "ok-only",
            alarm_actions: false,
            ok_actions: true,
            withholds: false,
            owed: &[(INSUFFICIENT_DATA, OK), (ALARM, OK)],
        },
        Row {
            case: "the topic named by both action lists",
            suffix: "both",
            alarm_actions: true,
            ok_actions: true,
            withholds: false,
            owed: &[(INSUFFICIENT_DATA, OK), (OK, ALARM), (ALARM, OK)],
        },
        Row {
            case: "both action lists, and a declared topic policy that omits the publish",
            suffix: "both-withheld",
            alarm_actions: true,
            ok_actions: true,
            withholds: true,
            owed: &[],
        },
    ]
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

/// The template a row deploys: a topic, a queue subscribed to it (granting SNS the send), the
/// alarm whose action lists the row decides, and — for a withholding row — the topic policy that
/// decides the alarm's publish.
fn template(row: &Row) -> String {
    let mut alarm = json!({
        "AlarmName": row.alarm(),
        "Namespace": "Custom/Deployed",
        "MetricName": "Backlog",
        "Statistic": "Average",
        "Period": PERIOD_SECONDS,
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "notBreaching",
    });
    if row.alarm_actions {
        alarm["AlarmActions"] = json!([{"Ref": "Topic"}]);
    }
    if row.ok_actions {
        alarm["OKActions"] = json!([{"Ref": "Topic"}]);
    }
    let mut resources = json!({
        "Topic": {
            "Type": "AWS::SNS::Topic",
            "Properties": {"TopicName": row.topic()},
        },
        "Queue": {
            "Type": "AWS::SQS::Queue",
            "Properties": {"QueueName": row.queue()},
        },
        "QueuePolicy": {
            "Type": "AWS::SQS::QueuePolicy",
            "Properties": {
                "Queues": [{"Ref": "Queue"}],
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Sid": "AllowSNSDelivery",
                        "Effect": "Allow",
                        "Principal": {"Service": SNS_PRINCIPAL},
                        "Action": "sqs:SendMessage",
                        "Resource": {"Fn::GetAtt": ["Queue", "Arn"]},
                    }],
                },
            },
        },
        "Subscription": {
            "Type": "AWS::SNS::Subscription",
            "Properties": {
                "TopicArn": {"Ref": "Topic"},
                "Protocol": "sqs",
                "Endpoint": {"Fn::GetAtt": ["Queue", "Arn"]},
            },
        },
        "Alarm": {"Type": "AWS::CloudWatch::Alarm", "Properties": alarm},
    });
    if row.withholds {
        resources["TopicPolicy"] = json!({
            "Type": "AWS::SNS::TopicPolicy",
            "Properties": {
                "Topics": [{"Ref": "Topic"}],
                "PolicyDocument": least_privilege_document(),
            },
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an alarm, the topic its action lists name and the queue subscribed to it",
        "Resources": resources,
    })
    .to_string()
}

/// Deploy the row's stack and wait until it reports a terminal status, which must be
/// `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    cfn(
        router,
        case,
        "CreateStack",
        &[
            ("StackName", &row.stack()),
            ("TemplateBody", &template(row)),
        ],
    )
    .await;
    let giving_up_at = Instant::now() + DEADLINE;
    let mut reported = String::new();
    while Instant::now() < giving_up_at {
        let described = cfn(router, case, "DescribeStacks", &[("StackName", &row.stack())]).await;
        reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported != "CREATE_IN_PROGRESS" {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: the stack must reach CREATE_COMPLETE, got {reported:?}"
    );
}

/// The url of the queue the row's stack declared, as Amazon SQS reports it for that name.
async fn queue_url(router: &MultiServiceRouter, row: &Row) -> String {
    let answered = sqs(
        router,
        row.case,
        "GetQueueUrl",
        &json!({"QueueName": row.queue()}),
    )
    .await;
    answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {:?}: GetQueueUrl reported no url: {answered}", row.case))
        .to_string()
}

/// The alarm's `StateValue` as CloudWatch reports it, and the whole described alarm for a
/// failure to quote.
async fn alarm_state(router: &MultiServiceRouter, row: &Row) -> (String, Value) {
    let described = cw(
        router,
        row.case,
        "DescribeAlarms",
        &json!({"AlarmNames": [row.alarm()]}),
    )
    .await;
    let state = described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (state, described)
}

/// Wait until the row's alarm reports `wanted`, which is a state the clock either already owes or
/// is about to owe.
async fn wait_for_state(router: &MultiServiceRouter, row: &Row, wanted: &str) {
    let case = row.case;
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (state, described) = alarm_state(router, row).await;
        if state == wanted {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the alarm must reach {wanted}, got {state:?} with {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Take every notification waiting on the queue at `url`, as `(OldStateValue, NewStateValue)` of
/// the alarm document SNS enveloped, DELETING each message so no later read counts it twice.
async fn taken(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<(String, String)> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut transitions = Vec::new();
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        });
        let inner = envelope["Message"].as_str().unwrap_or_default();
        let notification: Value = serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: the message is not an alarm notification ({error}): {inner}")
        });
        transitions.push((
            notification["OldStateValue"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            notification["NewStateValue"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    transitions
}

/// Take notifications off the queue at `url` until `wanted` of them have been taken or the
/// deadline passes, reading it at least once — so a row owed NOTHING has really read its queue
/// rather than asserting about a queue nobody looked at.
async fn taken_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<(String, String)> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut held = taken(router, case, url).await;
    while held.len() < wanted && Instant::now() < giving_up_at {
        held.extend(taken(router, case, url).await);
        if held.len() >= wanted {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    held
}

/// Every `HistoryData` document the alarm recorded for an `Action` item.
async fn action_records(router: &MultiServiceRouter, row: &Row) -> Vec<Value> {
    let history = cw(
        router,
        row.case,
        "DescribeAlarmHistory",
        &json!({"AlarmName": row.alarm(), "HistoryItemType": "Action"}),
    )
    .await;
    history["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["HistoryData"].as_str())
        .map(|data| serde_json::from_str(data).unwrap_or(Value::Null))
        .collect()
}

/// The transitions as a multiset, so two rows' expectations are compared without an order a
/// standard queue never promised.
fn as_multiset(transitions: &[(String, String)]) -> BTreeMap<(String, String), usize> {
    let mut counted: BTreeMap<(String, String), usize> = BTreeMap::new();
    for transition in transitions {
        *counted.entry(transition.clone()).or_default() += 1;
    }
    counted
}

/// The same, for a row's declared expectation.
fn expected_multiset(owed: &[(&str, &str)]) -> BTreeMap<(String, String), usize> {
    let mut counted: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (old, new) in owed {
        *counted
            .entry(((*old).to_string(), (*new).to_string()))
            .or_default() += 1;
    }
    counted
}

/// RULE: a deployed alarm's transition executes the actions of the state it moved TO, once per
/// transition — so each row's subscribed queue receives exactly the notifications its action
/// lists are owed, and a topic whose declared policy omits the publish receives none of them,
/// through the forced state's expiry and every evaluation that follows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deployed_alarm_publishes_the_actions_of_the_state_it_moved_to_once_each() {
    let router = router();
    let rows = rows();

    // Every row's estate stands before any alarm is forced — the position a client that deploys a
    // control stack and a restricted one beside it is in.
    for row in &rows {
        deploy(&router, row).await;
    }

    // The first evaluation of each freshly deployed alarm is the first of the three transitions,
    // and it has to be behind us before the force, or the force would be the one that moves the
    // alarm out of INSUFFICIENT_DATA and the row would be owed a different set.
    for row in &rows {
        wait_for_state(&router, row, OK).await;
    }

    // The withholding row is forced FIRST, so a refusal taken for its alarm is in the
    // dispatcher's hands before any row that expects a delivery is forced at all.
    let forced_first = rows.iter().filter(|row| row.withholds);
    let forced_after = rows.iter().filter(|row| !row.withholds);
    for row in forced_first.chain(forced_after) {
        cw(
            &router,
            row.case,
            "SetAlarmState",
            &json!({
                "AlarmName": row.alarm(),
                "StateValue": ALARM,
                "StateReason": "forced by the deployed-action-list suite",
            }),
        )
        .await;
    }

    // The forced state is taken back by the alarm's own next evaluation, which is the third
    // transition. Waiting for it also means the clock has woken up for every row many times over,
    // which is what makes the counts below "once per transition" rather than "once per
    // evaluation".
    for row in &rows {
        wait_for_state(&router, row, OK).await;
    }

    let urls: Vec<String> = {
        let mut collected = Vec::new();
        for row in &rows {
            collected.push(queue_url(&router, row).await);
        }
        collected
    };

    // The rows owed a delivery are read first: their notifications are the barrier that makes an
    // empty queue on the withholding row an absence rather than a delivery still in flight.
    let owed_something = rows.iter().zip(&urls).filter(|(row, _)| !row.owed.is_empty());
    let owed_nothing = rows.iter().zip(&urls).filter(|(row, _)| row.owed.is_empty());
    for (row, url) in owed_something.chain(owed_nothing) {
        let case = row.case;
        let mut held = taken_until(&router, case, url, row.owed.len()).await;
        assert_eq!(
            as_multiset(&held),
            expected_multiset(row.owed),
            "case {case:?}: expected the transitions {:?} on the subscribed queue, got {held:?}",
            row.owed
        );
        // Nothing more is owed: the alarm is back in the state its evaluation decides, and no
        // later evaluation publishes a transition that did not happen.
        held.extend(taken(&router, case, url).await);
        assert_eq!(
            as_multiset(&held),
            expected_multiset(row.owed),
            "case {case:?}: the queue must hold nothing beyond {:?}, got {held:?}",
            row.owed
        );

        let records = action_records(&router, row).await;
        if row.owed.is_empty() {
            // The alarm's own history accounts for every publish that was withheld, which is
            // where a client sees WHY nothing arrived.
            assert!(
                !records.is_empty(),
                "case {case:?}: the alarm must account for the actions it could not execute, got \
                 {records:?}"
            );
            for record in &records {
                assert_eq!(
                    record["actionState"].as_str(),
                    Some("Failed"),
                    "case {case:?}: a withheld action is recorded as Failed, got {record}"
                );
                let error = record["error"].as_str().unwrap_or_default();
                assert!(
                    error.contains(REFUSED_ACTION) && error.contains(&row.topic()),
                    "case {case:?}: the recorded error must name the refused publish and the \
                     topic {:?}, got {error:?}",
                    row.topic()
                );
            }
            continue;
        }
        assert!(
            records
                .iter()
                .all(|record| record["actionState"].as_str() == Some("Succeeded")),
            "case {case:?}: every executed action is recorded as Succeeded, got {records:?}"
        );
    }
}
