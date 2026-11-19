// SPDX-License-Identifier: CC0-1.0

//! The alarm-action notification a DEPLOYED estate delivers, and the topic policy the same
//! stack declared that decides it.
//!
//! RULE under test: **publishing an alarm's notification to the SNS topic its `AlarmActions`
//! names is a cross-service request made as the `cloudwatch.amazonaws.com` service principal,
//! so the DESTINATION topic's own access policy decides it: the notification is handed over
//! only where the document in force on the topic allows the publish action for that principal
//! on that topic, and a document that states no such grant withholds it — whoever wrote that
//! document, including an `AWS::SNS::TopicPolicy` a stack declared, which REPLACES the policy
//! the topic came up with.** A withheld publish reaches no subscriber of the topic and is
//! recorded in the alarm's own history as the authorization failure it was.
//!
//! Four services have to agree for that to hold, and this suite drives all four through one
//! router over their own wires: CloudFormation provisions the queue, the topic, the
//! subscription, the queue policy, the topic policy and the alarm; CloudWatch decides the
//! grant and publishes; SNS fans the notification out; SQS holds what arrived. So what each
//! row asserts is what a client that deploys the template and then forces the alarm observes
//! — the depth of the subscribed queue, and the `Action` item the alarm recorded.
//!
//! The rows differ along what the rule generalises over: the GRANT the declared document makes
//! — no document at all (the one the topic came up with stands), a least-privilege document
//! whose action list omits the publish, that same document with the publish added, the publish
//! granted through an action wildcard, the publish granted to another service principal only,
//! and the publish granted to this one by name. Between the two rows that differ only by the
//! presence of the publish action the ACTION SET is the only difference there is, so what
//! decides them is the grant and not the mere presence of a declared document.
//!
//! Nothing here waits out a span: the stack is polled to its terminal status and the queue is
//! polled until the notification the row expects has arrived or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::dynamodb_table_resources::failure_reasons;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, output_value, sns, xml_blocks, xml_text, REGION,
};

/// A router carrying CloudFormation and the four services this estate lives in, all on ONE
/// engine — the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's: mounting Amazon SQS for every suite of
/// this target would change what the accounts those suites build hold, and a queue ARN a
/// template merely NAMES is then one its owning service can be asked about.
fn deployed_estate_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// The service principal CloudWatch executes an alarm action as, which a topic policy names
/// under `Principal` as `{"Service": …}`.
const CLOUDWATCH_PRINCIPAL: &str = "cloudwatch.amazonaws.com";

/// The service principal SNS performs a delivery to a subscribed queue as, which is what that
/// queue's own policy has to allow.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// Amazon SQS's `awsJson1_0` wire: the media type and the service shape its `X-Amz-Target`
/// names an operation under.
const SQS_JSON: &str = "application/x-amz-json-1.0";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// How the alarm's history words a publish no document allowed, as the recorded `error`
/// carries it: the action that was refused.
const REFUSED_ACTION: &str = "not authorized to perform: SNS:Publish";

/// How long a row looks for the notification it expects, and how often it looks again.
/// Generous, so a loaded host cannot decide a row: every row waits for the CONDITION it is
/// about (a notification on the queue, a settled stack) and never for a span of the clock.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(30);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One Amazon SQS call through the same router, signed for `sqs` the way a client's SDK signs
/// it: the status and the parsed answer document.
///
/// `pub(crate)` because it is the ONE Amazon SQS caller of this target: a suite that reads a
/// queue back calls this rather than spelling that service's awsJson1_0 wire again
/// (`resource_schema_validation.rs` reads the queues its templates declare through it).
pub(crate) async fn sqs(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://sqs.{REGION}.amazonaws.com/"))
        .header("content-type", SQS_JSON)
        .header("x-amz-target", format!("{SQS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// The policy document a row's `AWS::SNS::TopicPolicy` declares: one statement allowing
/// `actions` for `principal` on the topic the same stack declares, which the intrinsic in its
/// `Resource` names.
fn topic_policy_document(actions: Value, principal: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DeclaredByTheStack",
            "Effect": "Allow",
            "Principal": principal,
            "Action": actions,
            "Resource": {"Ref": "Topic"},
        }],
    })
}

/// The names one row's stack declares, each carrying the row's own index so no two rows share
/// a resource.
struct Names {
    stack: String,
    topic: String,
    queue: String,
    alarm: String,
}

impl Names {
    fn of(index: usize) -> Self {
        Self {
            stack: format!("deployed-alarm-topic-policy-{index}"),
            topic: format!("deployed-alarm-topic-{index}"),
            queue: format!("deployed-alarm-queue-{index}"),
            alarm: format!("deployed-alarm-{index}"),
        }
    }
}

/// The template a row deploys: a queue, a topic the queue is subscribed to, the queue policy
/// that lets SNS write to it, an alarm whose `AlarmActions` is that topic, and — for a row
/// that declares one — the topic policy that decides the alarm's publish.
///
/// The queue policy's grant is scoped to the publishing topic with `aws:SourceArn`, the form
/// AWS documents for a queue behind a topic, so the delivery has to be decided against a
/// CONDITIONED statement rather than a bare allow. Every reference between the resources is an
/// intrinsic (`Ref`, `Fn::GetAtt`), including the ones NESTED inside the two policy documents.
fn template(names: &Names, topic_policy: Option<Value>) -> String {
    let mut resources = json!({
        "Queue": {
            "Type": "AWS::SQS::Queue",
            "Properties": {"QueueName": names.queue},
        },
        "Topic": {
            "Type": "AWS::SNS::Topic",
            "Properties": {"TopicName": names.topic},
        },
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
                "AlarmName": names.alarm,
                "Namespace": "Deployed/Behavioral",
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
        "Description": "an alarm, the topic it notifies and the policy that decides the publish",
        "Resources": resources,
        "Outputs": {
            "QueueUrl": {"Value": {"Ref": "Queue"}},
            "TopicArn": {"Value": {"Ref": "Topic"}},
        },
    })
    .to_string()
}

/// Deploy a row's stack and answer the `(queue url, topic ARN)` its outputs publish.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    names: &Names,
    body: &str,
) -> (String, String) {
    let (status, created) = create_stack(router, &names.stack, body, &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    let described = poll_to_terminal(router, case, &names.stack).await;
    let reported = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reported:?} because {:?}",
        failure_reasons(router, &names.stack).await
    );
    let queue_url = output_value(&described, "QueueUrl").unwrap_or_else(|| {
        panic!("case {case:?}: the stack must publish the queue's url, got {described}")
    });
    let topic_arn = output_value(&described, "TopicArn").unwrap_or_else(|| {
        panic!("case {case:?}: the stack must publish the topic's ARN, got {described}")
    });
    (queue_url, topic_arn)
}

/// The `Policy` attribute Amazon SNS reports for the topic `arn`, parsed.
///
/// The attributes arrive as `<entry><key>…</key><value>…</value></entry>` pairs with the
/// document's own JSON escaped inside one of them, so the entry is found by its key and its
/// value unescaped with the harness's own reader.
async fn reported_topic_policy(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, body) = sns(router, "GetTopicAttributes", &[("TopicArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetTopicAttributes({arn}) must answer 200, got {status} {body}"
    );
    let entry = xml_blocks(&body, "entry")
        .into_iter()
        .find(|entry| entry.contains("<key>Policy</key>"))
        .unwrap_or_else(|| panic!("case {case:?}: no Policy attribute in {body}"));
    let text = xml_text(&entry, "value").unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("case {case:?}: the reported Policy is not JSON ({error}): {text}")
    })
}

/// Force the alarm into `ALARM`, the transition that fires its `AlarmActions`.
async fn force_into_alarm(router: &MultiServiceRouter, case: &str, alarm: &str) {
    let (status, answered) = cloudwatch(
        router,
        "SetAlarmState",
        &json!({
            "AlarmName": alarm,
            "StateValue": "ALARM",
            "StateReason": "forced by the deployed-estate suite",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SetAlarmState, got {status} with {answered}"
    );
}

/// Every alarm notification waiting on the queue at `url`, as the document SNS enveloped.
async fn arrived(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ReceiveMessage, got {status} with {received}"
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

/// Look for the notifications on the queue at `url` until `wanted` of them have arrived or
/// the deadline passes, reading it at least once — so a row expecting NOTHING has really read
/// the queue rather than asserting about a queue nobody looked at.
async fn arrived_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut held = arrived(router, case, url).await;
    while held.len() < wanted && Instant::now() < deadline {
        tokio::time::sleep(LOOK_AGAIN).await;
        held.extend(arrived(router, case, url).await);
    }
    held
}

/// The `Action` items the alarm `name` recorded, as the parsed `HistoryData` of each.
async fn action_items(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let (status, history) = cloudwatch(
        router,
        "DescribeAlarmHistory",
        &json!({"AlarmName": name, "HistoryItemType": "Action"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAlarmHistory, got {status} {history}"
    );
    history["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let data = item["HistoryData"].as_str().unwrap_or_default();
            serde_json::from_str(data).unwrap_or_else(|error| {
                panic!("case {case:?}: HistoryData is not JSON ({error}): {data}")
            })
        })
        .collect()
}

/// Read the alarm's `Action` history until it holds one item or the deadline passes: the
/// record is written as the action completes, so a row reads it the way a client polling the
/// history does.
async fn settled_action_item(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let items = action_items(router, case, name).await;
        if let Some(item) = items.into_iter().next() {
            return item;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the alarm {name} must record the action it executed"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the document the stack declares for its topic, and how many notifications the
/// subscribed queue is then owed.
struct Row {
    case: &'static str,
    /// The `PolicyDocument` the row's `AWS::SNS::TopicPolicy` declares, or `None` for a stack
    /// that declares no topic policy at all.
    document: Option<Value>,
    /// How many notifications the queue subscribed to the topic receives.
    delivered: usize,
}

/// The rows, spanning the grants a declared document can make.
///
/// The action names are Amazon SNS's own as a POLICY spells them — `SNS:<action>`, where
/// `Publish`, `Subscribe` and `GetTopicAttributes` are three of this service's operations
/// (`smithy_query(query="list_operations")`) and `Receive` is one of the permission-only
/// actions a topic policy may name beside them — so the only thing that matters about the
/// three-action list is that `SNS:Publish` is not in it. The principal is the account
/// wildcard in those two rows, so between them the ACTION SET is the only difference there is.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a stack declaring no topic policy, so the one the topic came up with stands",
            document: None,
            delivered: 1,
        },
        Row {
            case: "a least-privilege document whose action list omits the publish",
            document: Some(topic_policy_document(
                json!(["SNS:Subscribe", "SNS:Receive", "SNS:GetTopicAttributes"]),
                json!({"AWS": "*"}),
            )),
            delivered: 0,
        },
        Row {
            case: "the same document with the publish added to its action list",
            document: Some(topic_policy_document(
                json!([
                    "SNS:Subscribe",
                    "SNS:Receive",
                    "SNS:GetTopicAttributes",
                    "SNS:Publish"
                ]),
                json!({"AWS": "*"}),
            )),
            delivered: 1,
        },
        Row {
            case: "a document granting every action of the service through a wildcard",
            document: Some(topic_policy_document(json!("SNS:*"), json!({"AWS": "*"}))),
            delivered: 1,
        },
        Row {
            case: "a document granting the publish to another service principal only",
            document: Some(topic_policy_document(
                json!("SNS:Publish"),
                json!({"Service": "events.amazonaws.com"}),
            )),
            delivered: 0,
        },
        Row {
            case: "a document granting the publish to this service principal by name",
            document: Some(topic_policy_document(
                json!("SNS:Publish"),
                json!({"Service": CLOUDWATCH_PRINCIPAL}),
            )),
            delivered: 1,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declared_topic_policy_decides_the_deployed_alarms_action() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = deployed_estate_router();
        let names = Names::of(index);
        let declared = row.document.clone();
        let (queue_url, topic_arn) =
            deploy(&router, case, &names, &template(&names, declared.clone())).await;

        // The document the stack declared is the one the TOPIC holds: that is what the
        // publish is decided against, and the intrinsic nested in its `Resource` resolves to
        // the topic the same stack created.
        let held = reported_topic_policy(&router, case, &topic_arn).await;
        match declared {
            Some(document) => {
                let expected = json!([{
                    "Sid": "DeclaredByTheStack",
                    "Effect": "Allow",
                    "Principal": document["Statement"][0]["Principal"].clone(),
                    "Action": document["Statement"][0]["Action"].clone(),
                    "Resource": topic_arn.clone(),
                }]);
                assert_eq!(
                    held["Statement"], expected,
                    "case {case:?}: the topic must hold the document the stack declared, got \
                     {held}"
                );
            }
            None => assert_eq!(
                held["Id"].as_str(),
                Some("__default_policy_ID"),
                "case {case:?}: a topic no stack wrote a policy for keeps the one it came up \
                 with, got {held}"
            ),
        }

        force_into_alarm(&router, case, &names.alarm).await;

        // The alarm's own account of the action comes FIRST, because it is what makes a row
        // expecting nothing honest: the dispatcher has executed (or refused) this action by
        // the time the item exists, so an empty queue read after it is an absence rather than
        // a delivery still in flight.
        let item = settled_action_item(&router, case, &names.alarm).await;
        let notifications = arrived_until(&router, case, &queue_url, row.delivered).await;
        assert_eq!(
            notifications.len(),
            row.delivered,
            "case {case:?}: expected {} notification(s) on the subscribed queue, got {:?}",
            row.delivered,
            notifications
        );
        if row.delivered > 0 {
            assert_eq!(
                notifications[0]["AlarmName"].as_str(),
                Some(names.alarm.as_str()),
                "case {case:?}: the notification must name the alarm that produced it, got {}",
                notifications[0]
            );
            assert_eq!(
                notifications[0]["NewStateValue"].as_str(),
                Some("ALARM"),
                "case {case:?}: the notification must carry the state moved to, got {}",
                notifications[0]
            );
            assert_eq!(
                item["actionState"].as_str(),
                Some("Succeeded"),
                "case {case:?}: an executed action is recorded as Succeeded, got {item}"
            );
            continue;
        }
        // Nothing arrived, and the alarm's own history says WHY: the publish was refused for
        // this service principal on this topic, so a client can tell a withheld notification
        // from one that was never owed.
        assert_eq!(
            item["actionState"].as_str(),
            Some("Failed"),
            "case {case:?}: a withheld action is recorded as Failed, got {item}"
        );
        let error = item["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(REFUSED_ACTION) && error.contains(&topic_arn),
            "case {case:?}: the recorded error must name the refused publish and the topic \
             {topic_arn:?}, got {error:?}"
        );
    }
}
