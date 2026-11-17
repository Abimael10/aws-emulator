// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function's own `sns:Publish` DOES: the body it published arrives
//! on every queue confirmed on that topic, byte for byte.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK on the handler's import path, so a
//! client the handler builds AT MODULE SCOPE resolves and is signed as a session of the
//! function's execution role; a publication that session's policies permit is CARRIED OUT by
//! Amazon SNS rather than answered as a no-op — the topic answers the handler one message id
//! and the message then takes the topic's normal delivery path, so every confirmed
//! `sqs`-protocol subscription whose queue policy admits the service principal receives one
//! notification envelope naming that topic and that id, whose `Message` member is the published
//! body VERBATIM, whatever bytes that body carries. The invocation that made the call is HTTP
//! 200 with no `X-Amz-Function-Error`, carrying the handler's own return value, because what
//! decides that header is whether the handler returned (`InvocationResponse$FunctionError`).
//!
//! Nothing in Amazon SNS's model makes a permitted publication a no-op:
//! `Publish(Message required, TopicArn) -> {MessageId}` declares
//! `AuthorizationErrorException`, `NotFoundException`, `InvalidParameterException` and the KMS
//! family and nothing else (`smithy_query(query="operation", operation="Publish")`), so a
//! handler whose role grants the action on the topic either gets a `MessageId` and a delivery,
//! or one of those refusals — silence is neither.
//!
//! Five crates have to agree for that to hold — CloudFormation archives the inline handler into
//! the deployment package and deploys the role and the function, IAM holds the role and its
//! inline document, Lambda runs the handler and supplies it the SDK, Amazon SNS takes the
//! publication and fans it out, and Amazon SQS holds what arrived — so the suite lives here: a
//! crate-local suite can only ever see one side of it.
//!
//! What the neighbours pin and this one does not repeat:
//! `lambda_handler_fans_out_to_sns_sqs_and_eventbridge` drives ONE body to three destination
//! services and the refusal each answers an ungranted role with (Amazon SNS's is
//! `AuthorizationError`, HTTP 403), `lambda_python_runtime_bundles_the_aws_sdk` drives the
//! runtimes the SDK is on the import path of, and
//! `cloudwatch_alarm_action_reaches_its_subscribed_queue` (whose topic-and-subscriber setup this
//! suite reuses) drives a publication another service makes. What only this suite reaches is
//! what the PUBLISHED BODY is when it comes out the other end, over a table of bodies, and that
//! one publication reaches EVERY confirmed subscription rather than only the first.
//!
//! The rows differ along what the rule generalises over: the bytes of the published body — a
//! plain token, a body that is itself a JSON document, non-ASCII text, XML markup and the form
//! separators an `awsQuery` request is encoded with, control whitespace, one character, and a
//! body far longer than one line — because the body crosses a form-encoded request, an XML
//! answer and a JSON envelope between the handler and the subscriber, and each of those has its
//! own way of mangling bytes.
//!
//! Nothing here sleeps for a delivery: it polls each queue until what it is waiting for has
//! arrived, and the closing "nothing else was queued" read is taken at the instant a message
//! published through the same path AFTER the last invocation has arrived, rather than after a
//! span of the clock.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{element, form_encode};

/// The Region every request here is signed for, and so the Region the topic, the queues, the
/// role and the function belong to. The shared CloudFormation wait reads the stack in this
/// Region.
const REGION: &str = "us-east-1";

/// Where Lambda binds the function collection, one configuration read and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The topic the handler publishes to, and the second queue confirmed on it beside the one the
/// shared setup subscribes.
const TOPIC: &str = "xsvc-fn-publish-topic";
const SECOND_SINK: &str = "xsvc-fn-publish-second-sink";

/// The execution role, the function running as it, and the stack deploying both.
const ROLE: &str = "xsvc-fn-publish-role";
const FUNCTION: &str = "xsvc-fn-publish-caller";
const STACK: &str = "xsvc-fn-publish";

/// The module and attribute the deployment package carries the handler as: CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.lambda_handler";

/// The runtime the function is created with, and the `State` Lambda serves invocations in.
const RUNTIME: &str = "python3.12";
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` the stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Amazon SNS performs its delivery to a subscribed queue as, which is
/// what that queue's own policy has to admit.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action the handler's own call performs, which the execution role's inline document grants
/// on the topic alone.
const PUBLISH_ACTION: &str = "sns:Publish";

/// The `Type` member an Amazon SNS delivery to a `sqs`-protocol subscription carries.
const NOTIFICATION: &str = "Notification";

/// The timeout the function is created with. Nothing here is about a function's own timeout, so
/// it is given generous room: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long this suite waits for work the clock owes — a stack settling, a function becoming
/// invocable, a notification arriving — before giving up, and how often it looks again. The wait
/// is on the CONDITION; the deadline is generous so a loaded host delays a leg instead of
/// breaking it.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The handler the function runs, in the shape a handler that notifies the rest of a system is
/// written in: `boto3.client("sns")` at MODULE scope — where a runtime that resolved no `sns`
/// client would fail every invocation at import — and one publication per call.
///
/// A refusal is caught and reported in the return value rather than left to fail the run, so a
/// publication the policies in force did not permit is visible as the service's own code instead
/// of as a bare function error.
const PUBLISHING_HANDLER: &str = r#"
import boto3
import botocore.exceptions

SNS = boto3.client("sns")


def lambda_handler(event, context):
    try:
        answered = SNS.publish(TopicArn=event["topic"], Message=event["message"])
    except botocore.exceptions.ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
        }
    return {"ok": True, "message_id": answered["MessageId"]}
"#;

/// What one Lambda call answered.
struct Answer {
    status: u16,
    function_error: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda", REGION))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        // "X-Amz-Function-Error — If present, indicates that an error occurred during function
        // execution" (`Invoke`'s own output).
        function_error: parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: answered.to_vec(),
    }
}

/// One form-wire (`awsQuery`) call against the service whose signing name is `signing_name`,
/// which must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, REGION))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), which must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs", REGION))
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
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A SECOND queue confirmed on `topic`, admitting the service principal the delivery is
/// performed as without scoping the grant to one source — the other spelling of the documented
/// queue policy. Answers the queue's url.
///
/// The first subscriber comes from the shared setup this suite reuses; what is added here is a
/// second one, because a publication that reached only the first subscription is not a fan-out.
async fn given_second_subscriber(router: &MultiServiceRouter, case: &str, topic: &str) -> String {
    let created = sqs(
        router,
        case,
        "CreateQueue",
        &json!({"QueueName": SECOND_SINK}),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
    query(
        router,
        case,
        "sns",
        "2010-03-31",
        "Subscribe",
        &[
            ("TopicArn", topic),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    )
    .await;
    url
}

/// The execution role, trusted by Lambda — which `CreateFunction` checks — and carrying an
/// inline document that grants the log writes every execution role makes plus the publication on
/// `topic` and on nothing else. Answers its ARN.
async fn given_execution_role(router: &MultiServiceRouter, case: &str, topic: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": PUBLISH_ACTION,
                "Resource": topic,
            },
        ],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "publish-to-the-topic"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Deploy the function running [`PUBLISHING_HANDLER`] as `role_arn` and wait until Lambda reports
/// it invocable.
///
/// The handler is declared as the stack's inline source, which CloudFormation archives into the
/// deployment package — which is what makes the package hold exactly ONE module, the handler's,
/// and no SDK of its own.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Caller": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PUBLISHING_HANDLER},
                },
            },
        },
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (status, answer) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: the stack {STACK:?} must reach {CREATE_COMPLETE}, last saw {answer}"
    );

    // The provisioning window the create documents ("During this time, you can't invoke or
    // modify the function") is owed by the clock, which no fixed sleep names: this looks until
    // the function reports itself invocable.
    let giving_up_at = Instant::now() + DEADLINE;
    let mut configuration;
    loop {
        configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE)
            || Instant::now() >= giving_up_at
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration["State"].as_str(),
        Some(FUNCTION_ACTIVE),
        "case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}"
    );
}

/// Invoke the function synchronously, telling it which topic to publish `message` to.
async fn invoke(router: &MultiServiceRouter, topic: &str, message: &str) -> Answer {
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        Some(&json!({"topic": topic, "message": message})),
    )
    .await
}

/// Take every notification waiting on the queue at `url`, as the envelope the queue carries it
/// in, deleting what it hands over so a later read sees only what arrived since. The envelope is
/// handed back WHOLE: its `Message` is the published body, which is not a document.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not an SNS envelope ({error}): {body}")
        }));
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
    envelopes
}

/// Drain until `wanted` envelopes have arrived, or the deadline passes. Driven off what has
/// ARRIVED rather than off a span of the clock, and it keeps draining past `wanted` only in the
/// sense that one read may hand over several — a duplicate therefore shows up as a count the
/// caller refuses rather than being hidden.
async fn drain_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        if arrived.len() >= wanted || Instant::now() >= giving_up_at {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    arrived
}

/// The one envelope `url` holds for this row, with the shape every notification carries asserted
/// on it: the delivery kind, the topic it came from and the id the publication answered.
fn the_one_envelope(
    case: &str,
    where_from: &str,
    arrived: &[Value],
    topic: &str,
    id: &str,
) -> String {
    assert_eq!(
        arrived.len(),
        1,
        "case {case:?}: expected exactly one notification on {where_from}, got {arrived:?}"
    );
    let envelope = &arrived[0];
    assert_eq!(
        envelope["Type"],
        json!(NOTIFICATION),
        "case {case:?}: expected a notification envelope on {where_from}, got {envelope}"
    );
    assert_eq!(
        envelope["TopicArn"],
        json!(topic),
        "case {case:?}: expected the envelope on {where_from} to name the published topic, got \
         {envelope}"
    );
    assert_eq!(
        envelope["MessageId"],
        json!(id),
        "case {case:?}: expected the envelope on {where_from} to carry the id the publication \
         answered ({id}), got {envelope}"
    );
    envelope["Message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Message member on {where_from}: {envelope}"))
        .to_string()
}

/// The bodies the rule is asserted over, each differing in the bytes it carries.
fn bodies() -> Vec<(&'static str, String)> {
    vec![
        ("a plain token", "from-lambda-sdk".to_string()),
        (
            "a body that is itself a JSON document",
            json!({"marker": "published", "attempt": 1}).to_string(),
        ),
        (
            "a body carrying non-ASCII characters",
            "paiement validé — 10 € ☑".to_string(),
        ),
        (
            "a body carrying XML markup and the form separators",
            "<Message>a &amp; b</Message>?x=1&y=2+3%20".to_string(),
        ),
        (
            "a body carrying control whitespace",
            "line one\n\tline two\r\nline three".to_string(),
        ),
        ("a single character", "x".to_string()),
        ("a body far longer than one line", "z".repeat(4096)),
    ]
}

/// The published body arrives on every queue confirmed on the topic, byte for byte, and the
/// invocation that published it reports the handler's own return value.
#[tokio::test]
async fn a_handlers_publication_reaches_every_subscriber_verbatim() {
    let setup = "the topic, its two subscribers and the granted function";
    let router = router();
    // The first subscriber, with the grant conditioned on the publishing topic — the form the
    // documented queue policy takes — from the setup this suite shares with its neighbour.
    let (topic, first_url) =
        given_topic_with_subscriber(&router, setup, REGION, TOPIC, Grant::ScopedToTopic).await;
    let second_url = given_second_subscriber(&router, setup, &topic).await;
    let role = given_execution_role(&router, setup, &topic).await;
    given_function(&router, setup, &role).await;

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (case, body) in bodies() {
        let answer = invoke(&router, &topic, &body).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.function_error, None,
            "case {case:?}: expected no function error, got {:?} with {}",
            answer.function_error,
            answer.text()
        );
        let report = answer.json();
        assert_eq!(
            report["ok"],
            json!(true),
            "case {case:?}: expected the handler's publication to be carried out, got {report}"
        );
        let id = report["message_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !id.is_empty(),
            "case {case:?}: expected the topic's own message id in the handler's answer, got \
             {report}"
        );
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: expected a message id of this publication's own, but {id:?} was \
             already answered for an earlier one"
        );

        for (where_from, url) in [
            ("the conditioned subscriber", &first_url),
            ("the unconditioned subscriber", &second_url),
        ] {
            let arrived = drain_until(&router, case, url, 1).await;
            let delivered = the_one_envelope(case, where_from, &arrived, &topic, &id);
            assert_eq!(
                delivered, body,
                "case {case:?}: expected the body published through the handler's own SDK to \
                 arrive on {where_from} verbatim; expected {body:?}, got {delivered:?}"
            );
        }
    }

    // Nothing else was queued behind those deliveries. The read is taken at the instant a
    // message published through the SAME path after the last invocation has arrived, so it is
    // an observation rather than a span of the clock.
    let sentinel = "planted-after-the-last-invocation";
    let planted = element(
        &query(
            &router,
            setup,
            "sns",
            "2010-03-31",
            "Publish",
            &[("TopicArn", &topic), ("Message", sentinel)],
        )
        .await,
        "MessageId",
    );
    for (where_from, url) in [
        ("the conditioned subscriber", &first_url),
        ("the unconditioned subscriber", &second_url),
    ] {
        let arrived = drain_until(&router, setup, url, 1).await;
        let delivered = the_one_envelope(setup, where_from, &arrived, &topic, &planted);
        assert_eq!(
            delivered, sentinel,
            "case {setup:?}: the only thing left on {where_from} must be the planted message, \
             got {delivered:?}"
        );
    }
}
