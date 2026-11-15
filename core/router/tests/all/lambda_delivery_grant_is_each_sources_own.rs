// SPDX-License-Identifier: CC0-1.0

//! What reaches ONE Lambda function from the three sources that deliver to one — an
//! EventBridge rule target, an Amazon SNS `lambda`-protocol subscription and an Amazon SQS
//! event source mapping — while the document each of those deliveries is decided by is, and
//! then is not, in force.
//!
//! The rule pinned here: **a delivery to a Lambda function is made by the identity that
//! performs it and happens only while the document in force on THAT identity allows it,
//! decided afresh at every delivery: a service that PUSHES an event invokes the function as
//! its own service principal, so the FUNCTION's resource-based policy has to allow that
//! principal `lambda:InvokeFunction` ("for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies", `PutTargets`; the same for a
//! `lambda` subscription, "delivery of JSON-encoded message to an Lambda function",
//! `SubscribeInput$Protocol`), while a mapping that POLLS reads its source as the invoked
//! function's execution role ("Lambda reads items from the event source and invokes the
//! function", `CreateEventSourceMapping`, as "the ARN of the function's execution role",
//! `FunctionConfiguration$Role`), so the identity-based policies in force on that role decide
//! it. A delivery nothing allows does not happen — and a POLLED source keeps every record
//! such a pass did not take, so the backlog is delivered once the role's grant is back.**
//!
//! Six crates have to agree for that to hold (CloudFormation archives the inline handler into
//! the function's deployment package, IAM answers for the execution role, EventBridge matches
//! and hands over, Amazon SNS fans a publish out, Amazon SQS holds the polled queue and the
//! sink, and Lambda decides each invocation and RUNS the handler), so the suite drives the
//! assembled emulator rather than any one service crate. Every observation is a client's: the
//! sink queue the handler forwards to, and the source queue's own counts.
//!
//! The rows differ along what the rule generalises over — WHICH source delivers, and so which
//! document decides it and which identity it is decided for: a rule target and a subscription,
//! both decided by the function's own policy but for two different service principals, and a
//! mapping decided by the execution role's own policies instead. One function, one sink and one
//! engine serve all three, so a grant written for one source is shown to decide only that
//! source's delivery. The marker each delivery leaves is what the delivered record names
//! ITSELF with (`source` for a bus event, `EventSource` for an SNS record, `eventSource` for a
//! queue record), so a delivery that arrived by another path cannot be read as this one's, and
//! the SNS row asserts the members of the notification its handler was handed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, outputs_of};
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, sns, sqs, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the shared awsQuery helpers and the
/// shared stack reader ([`wait_for_stack_status`]) sign for.
const REGION: &str = "us-east-1";

/// The `source` and `detail-type` the rule of this suite matches on, and so the `source` a
/// delivered bus event names itself with.
const BUS_SOURCE: &str = "lambda.delivery.grant";
const DETAIL_TYPE: &str = "Grant Probe";

/// The service principals the two PUSHING services invoke a function as, which is what the
/// function's own resource-based policy has to allow.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action an invocation of a function performs, which is what a grant for either of those
/// principals has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The execution role of the one function, and the two inline policies it carries: the writes
/// its handler makes to the sink, which stay in force throughout, and the reads its mapping's
/// poller makes on the source queue, which this suite takes away and puts back.
const ROLE_NAME: &str = "lambda-delivery-grant-exec";
const FORWARD_POLICY: &str = "forward-to-the-sink";
const POLL_POLICY: &str = "poll-the-source";

/// The reads consuming a QUEUE source takes, which the mapping's poller makes as the role —
/// the actions AWS's own `AWSLambdaSQSQueueExecutionRole` grants a queue's reader, less the log
/// writes, which nothing here reads.
const QUEUE_READS: [&str; 3] = [
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
];

/// Where `CreateEventSourceMapping` and `GetEventSourceMapping` are bound.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `State` a mapping reports once it polls its event source.
const STATE_ENABLED: &str = "Enabled";

/// How long the source queue hides a record it hands a consumer, in seconds, and the function's
/// own `Timeout`: Lambda accepts a queue mapping only while the queue's `VisibilityTimeout` is
/// at least the function's `Timeout`.
const VISIBILITY_TIMEOUT: i64 = 30;
const FUNCTION_TIMEOUT: i64 = 10;

/// How long an observation of work the clock owes — a stack coming up, a function going
/// `Active`, a mapping settling, a delivery arriving — keeps looking before it is a failure.
/// Generous: the loops leave as soon as what they wait for holds, so this only decides how long
/// a LOADED machine is given.
const DELIVERY_BUDGET: Duration = Duration::from_secs(90);

/// How long the half that must observe NOTHING keeps looking before concluding nothing came:
/// many of the queue poller's own intervals, so a mapping that was going to poll has had many
/// passes in which to do it, and well past the span the granted half of this suite takes to
/// leave its first marker.
const QUIET_BUDGET: Duration = Duration::from_secs(6);

/// The handler the stack declares inline: it forwards ONE marker per record it is handed to the
/// sink queue, carrying what that record names ITSELF with, so the sink says which source
/// delivered it — and, for a notification, the members the SNS event carries.
///
/// The sink is addressed by NAME, resolved through the service's own `GetQueueUrl`, so the
/// source text carries nothing the deployment had to compute.
const FORWARDING_SOURCE: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_NAME")["QueueUrl"]
    markers = []
    for record in event.get("Records") or []:
        if "Sns" in record:
            notification = record["Sns"]
            markers.append(
                {
                    "source": record.get("EventSource"),
                    "message": notification.get("Message"),
                    "topic_arn": notification.get("TopicArn"),
                    "message_id": notification.get("MessageId"),
                    "subject": notification.get("Subject"),
                    "timestamp": notification.get("Timestamp"),
                }
            )
        else:
            markers.append(
                {
                    "source": record.get("eventSource"),
                    "message": record.get("body"),
                }
            )
    if not markers:
        markers.append(
            {
                "source": event.get("source"),
                "message": json.dumps(event.get("detail")),
                "detail_type": event.get("detail-type"),
            }
        )
    for marker in markers:
        client.send_message(QueueUrl=url, MessageBody=json.dumps(marker))
    return {"delivered": len(markers)}
"#;

/// The whole emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is
/// dispatched back into this same router, so the sink it writes is the queue this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the service it belongs to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// A queue called `name` hiding a delivered record for `visibility_timeout` seconds; answers
/// its url and the ARN Amazon SQS itself reports for it.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    visibility_timeout: i64,
) -> (String, String) {
    let created = sqs(
        router,
        "CreateQueue",
        &json!({
            "QueueName": name,
            "Attributes": { "VisibilityTimeout": visibility_timeout.to_string() },
        }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue {name:?} reports no QueueUrl"))
        .to_string();
    let read = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = read["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no QueueArn"))
        .to_string();
    (url, arn)
}

/// The identity policy the handler's own forward is judged against: a handler calls another
/// service with temporary credentials for its execution role, so resolving the sink's url and
/// the write itself are both the ROLE's calls. Named by action rather than by resource: the sink
/// is one of several queues this engine holds and the poller's own reads are the OTHER policy,
/// which is the one each half of this suite takes away and puts back.
fn forward_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// The reads the mapping's poller makes on the source queue, granted on THAT queue.
fn poll_policy(source_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": QUEUE_READS,
            "Resource": source_arn,
        }],
    })
    .to_string()
}

/// Create the execution role where IAM holds it — trusted by Lambda, granting the handler's
/// forward and the poller's reads — and answer the ARN the template names it by.
async fn given_role(router: &MultiServiceRouter, source_arn: &str) -> String {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", FORWARD_POLICY),
            ("PolicyDocument", &forward_policy()),
        ],
    )
    .await;
    write_poll_policy(router, source_arn).await;
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}")
}

/// Put the poller's reads back on the role, through IAM's own `PutRolePolicy`.
async fn write_poll_policy(router: &MultiServiceRouter, source_arn: &str) {
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", POLL_POLICY),
            ("PolicyDocument", &poll_policy(source_arn)),
        ],
    )
    .await;
}

/// Take the poller's reads away again, through IAM's own `DeleteRolePolicy`.
async fn delete_poll_policy(router: &MultiServiceRouter) {
    iam(
        router,
        "DeleteRolePolicy",
        &[("RoleName", ROLE_NAME), ("PolicyName", POLL_POLICY)],
    )
    .await;
}

/// Deploy a stack holding ONE function with the inline handler above, and answer its
/// `(name, arn)`.
///
/// The function comes from a template because that is what archives inline source into a
/// deployment package this emulator can run: `Code.ZipFile` in a template is the function's
/// SOURCE TEXT while the API's member is the archive's bytes. Everything the rule is about —
/// the rule, its target, the subscription, the mapping and the grants — is wired through each
/// service's OWN API below, the way a client wires it.
async fn deployed_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    sink_name: &str,
    role_arn: &str,
) -> (String, String) {
    let name = format!("{stack}-handler");
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": FORWARDING_SOURCE.replace("SINK_NAME", sink_name) },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Handler", "Arn"] } },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the handler's stack to deploy, got {reached} with {described}"
    );
    let arn = outputs_of(&described)
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish a FunctionArn, got {described}")
        });

    // The function has to be invocable before anything is delivered to it: "During this time,
    // you can't invoke or modify the function" (`CreateFunction`), so a delivery withheld here
    // would be the lifecycle's refusal rather than a document's.
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let (status, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return (name, arn);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {name} Active, last read {status} {reported}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Allow `principal` the invocation on the function, through `AddPermission` — which is how a
/// client writes the statement a pushed delivery is decided by.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    principal: &str,
) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": principal,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission({principal}), got {status} \
         with {answered}"
    );
}

/// The enabled mapping on the source queue, one message per batch; answers once Lambda reports
/// it `Enabled`, so every observation below is made of a mapping that polls.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    source_arn: &str,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        Some(json!({
            "FunctionName": function,
            "EventSourceArn": source_arn,
            "BatchSize": 1,
            "Enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created mapping reports no UUID: {created}"))
        .to_string();
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if read.get("State").and_then(Value::as_str) == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported {STATE_ENABLED}, last read \
             {status} {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// `SendMessage` of one body onto the queue `url` addresses.
async fn send_message(router: &MultiServiceRouter, url: &str, body: &str) {
    sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
}

/// What the queue at `url` reports as `(ApproximateNumberOfMessages,
/// ApproximateNumberOfMessagesNotVisible)` — what a client watching its own queue reads, and
/// the whole of what a blocked mapping must leave untouched.
async fn queue_counts(router: &MultiServiceRouter, case: &str, url: &str) -> (u64, u64) {
    let read = sqs(
        router,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    let count = |name: &str| {
        read["Attributes"][name]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the queue reports no {name}: {read}"))
            .parse::<u64>()
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {name} is not a number: {read} ({error})")
            })
    };
    (
        count("ApproximateNumberOfMessages"),
        count("ApproximateNumberOfMessagesNotVisible"),
    )
}

/// Take every marker waiting on the sink, deleting each so a later look finds only later ones,
/// and answer them parsed.
async fn take_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut markers = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default().to_string();
        sqs(
            router,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        markers.push(serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a forwarded marker as JSON, got {body:?} ({error})")
        }));
    }
    markers
}

/// The marker a `source` delivery left carrying `probe`, if one has arrived.
fn marker_of<'a>(markers: &'a [Value], source: &str, probe: &str) -> Option<&'a Value> {
    markers.iter().find(|marker| {
        marker["source"].as_str() == Some(source)
            && marker["message"]
                .as_str()
                .is_some_and(|message| message.contains(probe))
    })
}

/// One source that delivers to the function, and the document its delivery is decided by.
struct Source {
    case: &'static str,
    /// What a record delivered by this source names itself with, which is the marker's
    /// `source`: a bus event's own `source`, an SNS record's `EventSource`, a queue record's
    /// `eventSource`.
    names_itself: &'static str,
    /// The document that decides this source's delivery, as the failure message names it.
    decided_by: &'static str,
}

/// The three sources, which is what the rule generalises over: two that PUSH, decided by the
/// function's own resource-based policy for the pushing service's principal, and one that
/// POLLS, decided by the identity-based policies in force on the function's execution role.
const SOURCES: &[Source] = &[
    Source {
        case: "an EventBridge rule target",
        names_itself: BUS_SOURCE,
        decided_by: "the function's resource-based policy for events.amazonaws.com",
    },
    Source {
        case: "an Amazon SNS lambda-protocol subscription",
        names_itself: "aws:sns",
        decided_by: "the function's resource-based policy for sns.amazonaws.com",
    },
    Source {
        case: "an Amazon SQS event source mapping",
        names_itself: "aws:sqs",
        decided_by: "the execution role's own policies on the source queue",
    },
];

#[tokio::test]
async fn each_source_delivers_to_the_function_only_while_its_own_grant_is_in_force() {
    let case = "one function reached by a rule target, a lambda subscription and a queue mapping";
    let router = build_router();

    // The sink every delivery is observed on, and the queue the mapping polls.
    let (sink_url, _) = given_queue(&router, case, "delivery-grant-sink", VISIBILITY_TIMEOUT).await;
    let (source_url, source_arn) =
        given_queue(&router, case, "delivery-grant-source", VISIBILITY_TIMEOUT).await;

    // The role carries both policies while the pair is WIRED, so what the first half changes is
    // the policy in force while the mapping polls — not whether the mapping could be created.
    let role_arn = given_role(&router, &source_arn).await;
    let (function, function_arn) = deployed_function(
        &router,
        case,
        "delivery-grant",
        "delivery-grant-sink",
        &role_arn,
    )
    .await;
    let uuid = given_mapping(&router, case, &function, &source_arn).await;

    // The rule target.
    let rule = "delivery-grant-rule";
    events(
        &router,
        case,
        "PutRule",
        &json!({
            "Name": rule,
            "EventPattern": json!({
                "source": [BUS_SOURCE],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    let targeted = events(
        &router,
        case,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{ "Id": "function", "Arn": function_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: expected the function target to be accepted, got {targeted}"
    );

    // The lambda subscription.
    let created = sns(&router, "CreateTopic", &[("Name", "delivery-grant-topic")]).await;
    let topic_arn = element(&created, "TopicArn");
    let subscribed = sns(
        &router,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "lambda"),
            ("Endpoint", &function_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    assert!(
        !element(&subscribed, "SubscriptionArn").is_empty(),
        "case {case:?}: expected the lambda subscription to be created, got {subscribed}"
    );

    // ------------------------------------------------------------------
    // Nothing allowed: no statement for either principal, and the poller's reads taken off the
    // role. No delivery happens, and the polled queue keeps what it holds.
    // ------------------------------------------------------------------
    delete_poll_policy(&router).await;
    let blocked_detail = "blocked-bus-probe";
    let blocked_notification = "blocked-notification-probe";
    let blocked_record = "blocked-queue-probe";
    events(
        &router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": BUS_SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "probe": blocked_detail }).to_string(),
            }],
        }),
    )
    .await;
    sns(
        &router,
        "Publish",
        &[("TopicArn", &topic_arn), ("Message", blocked_notification)],
    )
    .await;
    send_message(&router, &source_url, blocked_record).await;

    let quiet_until = Instant::now() + QUIET_BUDGET;
    loop {
        let arrived = take_markers(&router, case, &sink_url).await;
        assert!(
            arrived.is_empty(),
            "case {case:?}: no source's delivery is allowed yet — each is withheld by {} — so \
             the handler must not have run, got {arrived:?}",
            SOURCES
                .iter()
                .map(|source| format!("{} by {}", source.case, source.decided_by))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let counts = queue_counts(&router, case, &source_url).await;
        assert_eq!(
            counts,
            (1, 0),
            "case {case:?}: the mapping's role may not make the reads its poller makes, so the \
             message must stay visible on the source queue, got {counts:?} as (visible, in \
             flight)"
        );
        if Instant::now() >= quiet_until {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    let (status, read) = lambda(&router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
    assert_eq!(
        read.get("State").and_then(Value::as_str),
        Some(STATE_ENABLED),
        "case {case:?}: a mapping that may not read its source is still the enabled mapping it \
         was, got {status} {read}"
    );

    // ------------------------------------------------------------------
    // Each grant written, each delivery then made. The backlog the blocked passes left is
    // delivered too: nothing a withheld pass declined to take was lost.
    // ------------------------------------------------------------------
    add_permission(&router, case, &function, "events-invoke", EVENTS_PRINCIPAL).await;
    add_permission(&router, case, &function, "sns-invoke", SNS_PRINCIPAL).await;
    write_poll_policy(&router, &source_arn).await;

    let granted_detail = "granted-bus-probe";
    let granted_notification = "granted-notification-probe";
    let granted_subject = "a subject line";
    let granted_record = "granted-queue-probe";
    events(
        &router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": BUS_SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "probe": granted_detail }).to_string(),
            }],
        }),
    )
    .await;
    let published = sns(
        &router,
        "Publish",
        &[
            ("TopicArn", &topic_arn),
            ("Message", granted_notification),
            ("Subject", granted_subject),
        ],
    )
    .await;
    let message_id = element(&published, "MessageId");
    assert!(
        !message_id.is_empty(),
        "case {case:?}: expected the publish to report a MessageId, got {published}"
    );
    send_message(&router, &source_url, granted_record).await;

    // What each source must have delivered: the probe of this half for each of the three, and
    // the record the blocked passes left on the queue.
    let expected: [(&str, &str, &str); 4] = [
        (SOURCES[0].case, SOURCES[0].names_itself, granted_detail),
        (
            SOURCES[1].case,
            SOURCES[1].names_itself,
            granted_notification,
        ),
        (SOURCES[2].case, SOURCES[2].names_itself, granted_record),
        (SOURCES[2].case, SOURCES[2].names_itself, blocked_record),
    ];
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut markers: Vec<Value> = Vec::new();
    loop {
        markers.extend(take_markers(&router, case, &sink_url).await);
        let missing: Vec<&(&str, &str, &str)> = expected
            .iter()
            .filter(|(_, source, probe)| marker_of(&markers, source, probe).is_none())
            .collect();
        if missing.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every source's delivery is allowed now, so each must reach the \
             handler; still missing {missing:?} of {expected:?}, got {markers:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // The notification the subscribed function was handed carries the publish's own members.
    let notification = marker_of(&markers, SOURCES[1].names_itself, granted_notification)
        .unwrap_or_else(|| panic!("case {case:?}: the notification marker was just found"));
    assert_eq!(
        notification["topic_arn"].as_str(),
        Some(topic_arn.as_str()),
        "case {case:?}: expected the record to name the publishing topic {topic_arn:?}, got \
         {notification}"
    );
    assert_eq!(
        notification["message_id"].as_str(),
        Some(message_id.as_str()),
        "case {case:?}: expected the record to carry the publish's own MessageId \
         {message_id:?}, got {notification}"
    );
    assert_eq!(
        notification["subject"].as_str(),
        Some(granted_subject),
        "case {case:?}: expected the record to carry the published Subject \
         {granted_subject:?}, got {notification}"
    );
    assert!(
        notification["timestamp"]
            .as_str()
            .is_some_and(|stamp| !stamp.is_empty()),
        "case {case:?}: expected the record to carry the notification's Timestamp, got \
         {notification}"
    );

    // And the polled queue is empty: a batch is deleted once the invocation it was handed to
    // succeeded, so both records are gone and neither is still in flight.
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let counts = queue_counts(&router, case, &source_url).await;
        if counts == (0, 0) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: with the role's grant back the mapping must consume the queue, which \
             reports {counts:?} as (visible, in flight)"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}
