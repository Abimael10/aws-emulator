// SPDX-License-Identifier: CC0-1.0

//! What an `https` SUBSCRIPTION's endpoint really receives, on the emulator a client STARTS:
//! the built binary, on its own port, with request authentication on, reached over TLS with
//! SigV4-signed calls.
//!
//! The endpoint is a Lambda function URL this same emulator published, whose handler records
//! every request it was sent on an Amazon SQS queue — so what SNS POSTed is read back out of
//! that queue, from outside the endpoint, exactly as a client observes a webhook.
//!
//! RULE: "https – delivery of JSON-encoded message via HTTPS POST"
//! (`SubscribeInput$Protocol`) — `Subscribe` POSTs a `SubscriptionConfirmation` carrying the
//! `Token` `ConfirmSubscription` is called with, and every `Publish` to a CONFIRMED
//! subscription is POSTed as a `Notification` whose `x-amz-sns-message-id` is the id the
//! publish answered with. The `Content-Type` of each is the one the subscription's effective
//! delivery policy states for the request (`requestPolicy.headerContentType`), which for a
//! subscription nobody gave one is the account system default. The subscription is confirmed
//! the way its endpoint's owner does it — "to confirm the subscription, visit the SubscribeURL
//! included in this message", a GET of that address carrying no credential — which is the one
//! caller such a link exists for even here, where every other request's signature is
//! verified.
//!
//! The owning crate's own `https_subscription_delivery` suite spans what that rule
//! generalises over — the retry budget, the redrive, raw delivery, and the final cancellation
//! an unsubscribe by someone other than the subscription's owner delivers — on a router the
//! test assembles with authentication off. The emulator a client runs is a different
//! configuration of the same code: the credentials are verified, each call arrives over its
//! own TLS connection, the delivery is made while the client is long-polling the capture
//! queue, and ONE process serves every call — so a delivery that works in-process and not on
//! the started emulator is a failure this suite reports rather than one only a client sees.
//! ONE wiring is driven here, since starting the binary costs what starting the emulator
//! costs, and the harness that starts it is the shared one
//! ([`crate::cfn_deployed_pipeline_on_a_started_emulator::Started`]) rather than a second
//! copy of the spawn, the port and the published credentials.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};
use crate::query_wire::opt_element;

/// The stack the execution role and the recording function are deployed by.
const STACK: &str = "started-emulator-https-subscription";

/// How long work the clock owes is waited for (the stack settling, the function reaching
/// `Active`, a POST reaching the endpoint and its record reaching the queue), and how often
/// this suite looks meanwhile. Every wait is on a CONDITION and leaves as soon as it holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the client waits on one `ReceiveMessage`, in seconds — "the duration (in seconds)
/// for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`): the wait a client makes for a delivery it cannot
/// otherwise be told about.
const WAIT_TIME_SECONDS: u64 = 2;

/// The `Content-Type` a POST carries when nothing configured one: the account system default
/// of the request policy an `EffectiveDeliveryPolicy` reports.
const DEFAULT_CONTENT_TYPE: &str = "text/plain; charset=UTF-8";

/// The `Content-Type` the subscription's own request policy then asks for.
const CONFIGURED_CONTENT_TYPE: &str = "application/json";

/// The handler behind the function URL: it records the request it was sent — the path, the
/// headers and the body — on the queue its environment names, and answers 200, which is how a
/// subscriber takes a delivery.
const SINK_HANDLER: &str = r#"import base64
import json
import os

import boto3


def handler(event, context):
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps(
            {
                "path": event.get("rawPath") or "/",
                "headers": event.get("headers") or {},
                "body": body,
            }
        ),
    )
    return {"statusCode": 200, "body": "taken"}
"#;

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// One SNS call that must be answered, as the XML text it answered.
fn sns(emulator: &Started, action: &str, members: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", "2010-03-31", action, members);
    assert_eq!(
        status, 200,
        "SNS {action} must answer 200, got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    text
}

/// One Amazon SQS call, which must answer 200.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let answered = parsed(&text);
    assert_eq!(
        status, 200,
        "Amazon SQS {operation} must answer 200, got {status} with {answered}; the log:\n{}",
        emulator.log()
    );
    answered
}

/// One Lambda `restJson1` call, as its status and parsed body.
fn lambda(emulator: &Started, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, text) = emulator.call(
        "lambda",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    (status, parsed(&text))
}

/// FOLLOW `url` as the owner of a subscription's endpoint does: a GET of the address it was
/// handed, carrying no credential at all, which must be ANSWERED — a link this service minted
/// and then refuses leads nowhere. Its body text.
///
/// The connection is made to this emulator either way; what the request states it is addressed
/// to is the link's own host, which is the position a client holding a URL is in.
fn followed(emulator: &Started, case: &str, url: &str) -> String {
    let addressed = url.split_once("://").map_or(url, |(_scheme, rest)| rest);
    let (host, path) = match addressed.find('/') {
        Some(at) => (&addressed[..at], &addressed[at..]),
        None => (addressed, "/"),
    };
    let (status, text) = emulator.unsigned(host, "GET", path, &[], Vec::new());
    assert_eq!(
        status, 200,
        "case {case:?}: following {url} must be answered, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    text
}

/// The stack this suite deploys: the execution role the handler's own write is judged against,
/// and the recording function running as it. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn wiring(queue_url: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "SinkRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The ONE action the handler itself takes and nothing else: the
                    // least-privilege wiring a client writes for a recording sink.
                    "Policies": [{
                        "PolicyName": "record-what-it-was-sent",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["sqs:SendMessage"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Sink": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["SinkRole", "Arn"] },
                    "Timeout": 30,
                    "Environment": { "Variables": { "QUEUE_URL": queue_url } },
                    "Code": { "ZipFile": SINK_HANDLER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Sink" } } },
    })
}

/// One request the endpoint recorded.
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: Value,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(Value::as_str)
    }

    fn document(&self) -> Value {
        parsed(&self.body)
    }
}

/// The requests the endpoint recorded on `path`, long-polled for until there are `expected` of
/// them — each message deleted, so a later look finds only later ones.
fn recorded_for(
    emulator: &Started,
    url: &str,
    path: &str,
    expected: usize,
    what: &str,
) -> Vec<Recorded> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<Recorded> = Vec::new();
    loop {
        let received = sqs(
            emulator,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            sqs(
                emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            );
            let captured = parsed(&body);
            let recorded = Recorded {
                path: captured["path"].as_str().unwrap_or_default().to_string(),
                headers: captured["headers"].clone(),
                body: captured["body"].as_str().unwrap_or_default().to_string(),
            };
            if recorded.path == path {
                seen.push(recorded);
            }
        }
        if seen.len() >= expected {
            return seen;
        }
        assert!(
            Instant::now() < giving_up_at,
            "expected {expected} {what} POST(s) at {path}, the endpoint recorded {seen:?}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Stand up the endpoint and answer the capture queue's url beside the function URL the
/// subscription addresses.
fn given_sink(emulator: &Started, case: &str) -> (String, String) {
    let created = sqs(
        emulator,
        "CreateQueue",
        &json!({"QueueName": format!("{STACK}-capture")}),
    );
    let capture_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("the capture queue reports no url, got {created}"))
        .to_string();

    let outputs = deployed(emulator, case, STACK, &wiring(&capture_url).to_string());
    let function = output(&outputs, case, "FunctionName");

    // An invocation needs the function Active, which is a span the service owes.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        );
        if configuration["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "the function {function} never reached Active, last read {configuration}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    let (status, url) = lambda(
        emulator,
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        Some(&json!({"AuthType": "NONE"})),
    );
    assert_eq!(
        status, 201,
        "CreateFunctionUrlConfig must answer 201, got {status} with {url}; the log:\n{}",
        emulator.log()
    );
    // `AuthType=NONE` leaves IAM out of it; the function's own resource policy is what admits
    // the anonymous caller an SNS delivery is.
    let (status, granted) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(&json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
    );
    assert_eq!(
        status, 201,
        "AddPermission must answer 201, got {status} with {granted}; the log:\n{}",
        emulator.log()
    );
    let sink = url["FunctionUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("no FunctionUrl in {url}"))
        .trim_end_matches('/')
        .to_string();
    (capture_url, sink)
}

/// RULE: on the emulator a client starts, an `https` subscription's endpoint is POSTed the
/// request that confirms it and, once confirmed, every published notification — each under the
/// `Content-Type` the subscription's effective delivery policy states.
#[test]
fn an_https_subscription_endpoint_is_posted_to_on_the_emulator_a_client_starts() {
    let case = "an https subscription on a started emulator";
    let emulator = Started::start();
    let (capture_url, sink) = given_sink(&emulator, case);

    let topic = opt_element(
        &sns(&emulator, "CreateTopic", &[("Name", "started-https-delivery")]),
        "TopicArn",
    )
    .unwrap_or_default();
    assert!(!topic.is_empty(), "CreateTopic must answer the topic's ARN");

    let path = "/take";
    let endpoint = format!("{sink}{path}");
    let subscribed = sns(
        &emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "https"),
            ("Endpoint", &endpoint),
            ("ReturnSubscriptionArn", "true"),
        ],
    );
    let subscription = opt_element(&subscribed, "SubscriptionArn").unwrap_or_default();

    // The confirmation request: POSTed by `Subscribe` itself, and what carries the token no
    // response of this service does.
    let confirmations = recorded_for(
        &emulator,
        &capture_url,
        path,
        1,
        "SubscriptionConfirmation",
    );
    let confirmation = &confirmations[0];
    assert_eq!(
        (
            confirmation.header("x-amz-sns-message-type"),
            confirmation.header("x-amz-sns-topic-arn"),
            confirmation.header("content-type"),
        ),
        (
            Some("SubscriptionConfirmation"),
            Some(topic.as_str()),
            Some(DEFAULT_CONTENT_TYPE),
        ),
        "case {case:?}: expected the confirmation's headers, got {:?}",
        confirmation.headers
    );
    let token = confirmation.document()["Token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !token.is_empty(),
        "case {case:?}: expected a Token in the confirmation document, got {:?}",
        confirmation.body
    );
    // The subscription is confirmed by FOLLOWING the link the request carries, as its
    // endpoint's owner does: "to confirm the subscription, visit the SubscribeURL included in
    // this message" — a plain GET of the address it was handed, carrying no credential, which
    // is the one caller such a link exists for even on an emulator that verifies every other
    // request's signature.
    let subscribe_url = confirmation.document()["SubscribeURL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        subscribe_url.contains(&token),
        "case {case:?}: expected a SubscribeURL presenting the token, got {subscribe_url:?}"
    );
    let confirmed = followed(&emulator, case, &subscribe_url);
    assert_eq!(
        opt_element(&confirmed, "SubscriptionArn").as_deref(),
        Some(subscription.as_str()),
        "case {case:?}: following the SubscribeURL must confirm the subscription it names, got \
         {confirmed}"
    );

    // A notification per publish, under the content type nothing configured.
    let published = sns(
        &emulator,
        "Publish",
        &[("TopicArn", &topic), ("Message", "the started body")],
    );
    let message_id = opt_element(&published, "MessageId").unwrap_or_default();
    let notifications = recorded_for(&emulator, &capture_url, path, 1, "Notification");
    let notification = &notifications[0];
    assert_eq!(
        (
            notification.header("x-amz-sns-message-type"),
            notification.header("x-amz-sns-message-id"),
            notification.header("x-amz-sns-subscription-arn"),
            notification.header("content-type"),
        ),
        (
            Some("Notification"),
            Some(message_id.as_str()),
            Some(subscription.as_str()),
            Some(DEFAULT_CONTENT_TYPE),
        ),
        "case {case:?}: expected the notification's headers of {message_id:?}, got {:?}",
        notification.headers
    );

    // The same subscription written a request policy of its own is POSTed to under THAT
    // content type: the delivery follows the policy in force on it.
    sns(
        &emulator,
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn", &subscription),
            ("AttributeName", "DeliveryPolicy"),
            (
                "AttributeValue",
                &json!({"requestPolicy": {"headerContentType": CONFIGURED_CONTENT_TYPE}})
                    .to_string(),
            ),
        ],
    );
    let published = sns(
        &emulator,
        "Publish",
        &[("TopicArn", &topic), ("Message", "the configured body")],
    );
    let message_id = opt_element(&published, "MessageId").unwrap_or_default();
    let configured = recorded_for(&emulator, &capture_url, path, 1, "Notification");
    assert_eq!(
        (
            configured[0].header("x-amz-sns-message-id"),
            configured[0].header("content-type"),
        ),
        (Some(message_id.as_str()), Some(CONFIGURED_CONTENT_TYPE)),
        "case {case:?}: a subscription whose request policy names a content type is POSTed to \
         under it, got {:?}",
        configured[0].headers
    );
}
