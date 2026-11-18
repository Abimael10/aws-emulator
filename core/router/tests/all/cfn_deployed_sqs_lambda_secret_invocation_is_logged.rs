// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed SQS -> Lambda pipeline whose handler reads a secret really RUNS,
//! and the run is OBSERVABLE where a client looks for it: under the function's own CloudWatch
//! Logs group.
//!
//! The rule pinned here, in one sentence: **while an event source mapping on an Amazon SQS
//! queue reports `Enabled` it polls that queue and invokes the function it names with the
//! batch it took, each record carrying `eventSource` `aws:sqs` and the message's body, and
//! whatever that invocation writes to its runtime's output is delivered to the function's own
//! log group — the default `/aws/lambda/<function name>` where the function names none — which
//! the first invocation CREATES along with a stream in it, so `DescribeLogStreams` and
//! `FilterLogEvents` answer with the handler's lines.** None of that depends on how the queue,
//! the secret, the role, the function and the mapping came to exist: a stack's
//! `AWS::SQS::Queue`, `AWS::SecretsManager::Secret`, `AWS::IAM::Role`,
//! `AWS::Lambda::Function` and `AWS::Lambda::EventSourceMapping` are the same resources a
//! client's own `CreateQueue`, `CreateSecret`, `CreateRole`, `CreateFunction` and
//! `CreateEventSourceMapping` leave behind.
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the queue, the
//! secret, the role, the function and the mapping; Lambda stands the mapping's poller up, runs
//! the deployment package and delivers its output; Amazon SQS hands the batch over; Secrets
//! Manager serves the handler's own read; IAM answers for the execution role both the poller's
//! reads and the handler's call are judged against; CloudWatch Logs holds what was written —
//! so the suite lives here rather than in any one crate. Every observation is made where a
//! client makes it: on the stack's `Outputs`, on Lambda's own `GetEventSourceMapping`, and on
//! CloudWatch Logs' `DescribeLogGroups`/`DescribeLogStreams`/`FilterLogEvents`, never on an
//! emulator internal.
//!
//! The rows differ along exactly what the rule generalises over: how many messages are waiting
//! (one, two, three), how many a batch may hold (one, a few, the ten a queue mapping defaults
//! to), what the bodies are (plain text, JSON, non-ASCII text, a large body) and what shape the
//! secret the handler reads has (plain text, a JSON document, text carrying non-ASCII
//! characters).
//!
//! `crate::cloudformation_provisions_lambda_event_source_mappings` is the suite for whether the
//! declared mapping exists at all and what refuses it, and
//! `crate::cloudformation_sqs_redrive_policy_dead_letters_a_failing_consumer` the suite for what
//! happens to a message no invocation can process; this one is the suite for the end a working
//! deployment reaches — the invocation happening at all, and its output being readable under the
//! function's own log group.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    mapping_until_settled, MAPPING_STATE_ENABLED,
};
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request of this suite is signed for — the one `wait_for_stack_status` and
/// `mapping_until_settled` poll in — and so the Region the stack's resources belong to.
const REGION: &str = "us-east-1";

/// The terminal `StackStatus` a deployment that succeeded settles to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a deployment of a template whose IAM resources carry names of their own has
/// to be made with (`CreateStackInput$Capabilities`).
const NAMED_IAM_CAPABILITY: &str = "CAPABILITY_NAMED_IAM";

/// Where a function's output is delivered when the function names no group of its own: "Lambda
/// functions send logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The `eventSource` every record of the batch a queue mapping hands its function carries, as
/// AWS documents an Amazon SQS event.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// The runtime and handler every row's function declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.lambda_handler";

/// How long one invocation of this handler may run for, as the function's own `Timeout`, and
/// how long the queue hides a delivered message for, as its own `VisibilityTimeout`. Both are
/// generous: a queue hands a message to one consumer for the length of its visibility timeout,
/// so Lambda refuses a mapping onto a function that could outlast that window, and neither
/// bound is what this suite asserts on.
const FUNCTION_TIMEOUT: u64 = 60;
const VISIBILITY_TIMEOUT: u64 = 120;

/// How long a row waits for the deployed pipeline to reach what the rule owes before failing.
///
/// Every step of the chain is driven by the CLOCK (the stack settles, the function goes
/// `Active`, the mapping settles into `Enabled`, the poller's next pass falls due, the run's
/// output is delivered), so the wait is on that CONDITION — looked for again until it holds —
/// and this bound is generous slack whose only job is that the wait TERMINATES. Nothing below
/// asserts on elapsed time.
const SETTLE_BUDGET: Duration = Duration::from_secs(120);

/// How long a row waits before concluding that a group nothing has invoked into holds nothing
/// of its own, and how often either wait looks again.
const QUIET_BUDGET: Duration = Duration::from_secs(3);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The logical ids every row's template declares its resources under, and the output keys it
/// publishes them through.
const QUEUE: &str = "Source";
const SECRET: &str = "Credentials";
const ROLE: &str = "ExecutionRole";
const FUNCTION: &str = "Consumer";
const MAPPING: &str = "Trigger";
const QUEUE_URL_OUTPUT: &str = "QueueUrl";
const QUEUE_ARN_OUTPUT: &str = "QueueArn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const MAPPING_ID_OUTPUT: &str = "MappingId";

/// The reads the mapping's poller makes on the queue it polls, which the invoked function's
/// execution role has to be granted for the mapping to be created at all. Source: the actions
/// AWS's own `AWSLambdaSQSQueueExecutionRole` grants for an Amazon SQS event source.
const QUEUE_READS: [&str; 3] = [
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
];

/// The calls a run's own log delivery makes, which the same role has to be granted for the
/// group and the stream to be created: the three actions AWS's own
/// `AWSLambdaBasicExecutionRole` grants.
const LOG_WRITES: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The action the handler's own read performs ("Actions defined by AWS Secrets Manager":
/// `secretsmanager:GetSecretValue`).
const SECRET_READ: &str = "secretsmanager:GetSecretValue";

/// The handler every row deploys inline: it reads the secret the stack named it through its
/// environment with the AWS SDK its runtime supplies, and writes ONE line per record it was
/// handed, carrying the record's own `eventSource`, the message body and what it read. The line
/// is plain text so that a body or a secret travels through it verbatim.
const PROCESSING_HANDLER: &str = r#"
import os

import boto3


def lambda_handler(event, context):
    secret = boto3.client("secretsmanager").get_secret_value(
        SecretId=os.environ["SECRET_ID"]
    )["SecretString"]
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        print(
            "PROCESSED source=%s body=%s secret=%s"
            % (record["eventSource"], record["body"], secret)
        )
    return {"processed": len(records)}
"#;

/// The whole emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`):
/// the handler's own SDK call and its log delivery are dispatched back into this same router, so
/// the secret it reads is the secret this stack deployed and the group this suite reads is the
/// group that run wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation call over its awsQuery wire, as its status and its body verbatim.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One `X-Amz-Target` RPC call of `signing_name`, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon SQS call (`awsJson1_0`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", &format!("AmazonSQS.{operation}"), "1.0", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon CloudWatch Logs call (`awsJson1_1`), as its status and parsed body. A group no
/// invocation has created yet is read as the refusal CloudWatch Logs answers for a group that is
/// not there, which is exactly what a client polling for output that has not arrived sees.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "logs",
        &format!("Logs_20140328.{operation}"),
        "1.1",
        body,
    )
    .await
}

/// One row: how many messages are waiting, how many a batch may hold, what the bodies are and
/// what the secret the handler reads looks like.
struct Row {
    case: &'static str,
    /// `BatchSize`, or `None` for a mapping that states none and so takes the default a queue
    /// mapping carries.
    batch_size: Option<i64>,
    secret: &'static str,
    bodies: Vec<String>,
}

/// The catalog. The columns span one waiting message and several, a batch ceiling of one, of a
/// few and of none at all (the default), bodies that are plain text, JSON, non-ASCII text and
/// large, and a `SecretString` that is plain text, a JSON document and text carrying non-ASCII
/// characters — `SecretString` being free text whose only constraint is a length of at least one
/// (`smithy_query(query="shape", shape="SecretStringType")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one plain body, one message per batch, a plain secret",
            batch_size: Some(1),
            secret: "a-plain-secret-string",
            bodies: vec!["cfn-trigger-plain-0".to_string()],
        },
        Row {
            case: "a json body and a non-ascii one, a few per batch, a json secret",
            batch_size: Some(3),
            secret: r#"{"endpoint":"db.internal","port":5432}"#,
            bodies: vec![
                json!({ "order": 17, "marker": "cfn-trigger-json-1" }).to_string(),
                "cfn-trigger-ünïcödé-⚑-1".to_string(),
            ],
        },
        Row {
            case: "three bodies one of them large, the default batch ceiling, a non-ascii secret",
            batch_size: None,
            secret: "a-secret-with-unicode-£-and-a-space",
            bodies: vec![
                "cfn-trigger-plain-2".to_string(),
                format!("cfn-trigger-large-2-{}", "x".repeat(2_048)),
                "cfn-trigger-tail-2".to_string(),
            ],
        },
    ]
}

/// The identity policy the execution role carries: the poller reads the source queue acting as
/// that role, the handler's own read is judged against it, and so is the run's log delivery — so
/// a role short of any of the three leaves the rule unobservable for a reason that is not under
/// test here.
fn role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": QUEUE_READS,
                "Resource": { "Fn::GetAtt": [QUEUE, "Arn"] },
            },
            {
                "Effect": "Allow",
                "Action": SECRET_READ,
                "Resource": { "Ref": SECRET },
            },
            {
                "Effect": "Allow",
                "Action": LOG_WRITES,
                "Resource": "*",
            },
        ],
    })
}

/// The template one row deploys: the queue, the secret, the execution role granting the three
/// things the pipeline needs, the function whose package reads that secret and logs each record,
/// and the event source mapping that polls the queue and invokes that function.
fn template(stack: &str, row: &Row) -> String {
    let mut mapping = json!({
        "EventSourceArn": { "Fn::GetAtt": [QUEUE, "Arn"] },
        "FunctionName": { "Ref": FUNCTION },
        "Enabled": true,
    });
    if let Some(batch_size) = row.batch_size {
        mapping["BatchSize"] = json!(batch_size);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            QUEUE: {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": format!("{stack}-source"),
                    "VisibilityTimeout": VISIBILITY_TIMEOUT,
                },
            },
            SECRET: {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    "Name": format!("{stack}-secret"),
                    "SecretString": row.secret,
                },
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{stack}-exec"),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": format!("{stack}-run-the-pipeline"),
                        "PolicyDocument": role_policy(),
                    }],
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(stack),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                    "Environment": { "Variables": { "SECRET_ID": { "Ref": SECRET } } },
                    "Code": { "ZipFile": PROCESSING_HANDLER },
                },
            },
            MAPPING: {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": mapping,
            },
        },
        "Outputs": {
            QUEUE_URL_OUTPUT: { "Value": { "Ref": QUEUE } },
            QUEUE_ARN_OUTPUT: { "Value": { "Fn::GetAtt": [QUEUE, "Arn"] } },
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
            MAPPING_ID_OUTPUT: { "Value": { "Ref": MAPPING } },
        },
    })
    .to_string()
}

/// The name the row's template gives its function, which is also where its default log group
/// name comes from.
fn function_name(stack: &str) -> String {
    format!("{stack}-consumer")
}

/// The line the handler writes for one record of `body`, read with the secret the stack
/// deployed: the whole of what the rule owes for that message, in one string.
fn expected_line(body: &str, secret: &str) -> String {
    format!("PROCESSED source={SQS_EVENT_SOURCE} body={body} secret={secret}")
}

/// The names `DescribeLogStreams` reports for `group`, or none while CloudWatch Logs still holds
/// no such group.
async fn stream_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, described) =
        logs(router, "DescribeLogStreams", &json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    described["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|stream| {
            stream["logStreamName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every event `FilterLogEvents` answers for `group`, as `(message, logStreamName)` — the two
/// things a client reads off one event.
async fn filtered_events(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let (status, answered) =
        logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| {
            (
                event["message"].as_str().unwrap_or_default().to_string(),
                event["logStreamName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Whether CloudWatch Logs reports a group of exactly this name, through the call a client
/// lists groups with.
async fn group_exists(router: &MultiServiceRouter, group: &str) -> bool {
    let (status, listed) = logs(
        router,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": group }),
    )
    .await;
    if status != 200 {
        return false;
    }
    listed["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|held| held["logGroupName"].as_str() == Some(group))
}

/// Look at the function's log group again until every line of `wanted` has been written and
/// `DescribeLogStreams` reports the stream they were written into, answering the streams and the
/// events last read.
async fn awaited_output(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: &[String],
) -> (Vec<String>, Vec<(String, String)>) {
    let giving_up_at = Instant::now() + SETTLE_BUDGET;
    loop {
        let streams = stream_names(router, group).await;
        let events = filtered_events(router, group).await;
        let written = wanted.iter().all(|line| {
            events
                .iter()
                .any(|(message, _)| message.contains(line.as_str()))
        });
        if written && !streams.is_empty() {
            return (streams, events);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the mapping's invocation to write {wanted:?} under \
             {group:?}, after {SETTLE_BUDGET:?} {streams:?} held {events:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// RULE: an enabled event source mapping on an Amazon SQS queue invokes the function it names
/// with the batch it polled, and that invocation's output is readable under the function's own
/// default log group — however the queue, the function and the mapping came to exist.
#[tokio::test]
async fn a_stack_deployed_queue_mapping_logs_its_invocation_under_the_functions_own_group() {
    // ONE emulator for every row, so a row that observes output and a row's own quiet window
    // are read in the same account.
    let router = build_router();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-sqs-lambda-secret-logs-{index}");
        let function = function_name(&stack);
        let group = format!("{DEFAULT_GROUP_PREFIX}{function}");

        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template(&stack, row)),
                // The template names its own role, so the deployment has to be acknowledged:
                // "if you want to create a stack from a template that contains IAM resources
                // with custom names, you must specify CAPABILITY_NAMED_IAM"
                // (`CreateStackInput$Capabilities`).
                ("Capabilities.member.1", NAMED_IAM_CAPABILITY),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: the stack must deploy the queue, the secret, the role, the function \
             and the mapping, got {reached} with {described}"
        );

        // The identifiers a client drives the deployed pipeline by are the ones the stack
        // PUBLISHES, and nothing below is computed from an emulator internal.
        let outputs = outputs_of(&described);
        let published = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack must publish {name}, got {described}")
            })
        };
        let queue_url = published(QUEUE_URL_OUTPUT);
        let queue_arn = published(QUEUE_ARN_OUTPUT);
        let published_function = published(FUNCTION_NAME_OUTPUT);
        let mapping_id = published(MAPPING_ID_OUTPUT);
        assert_eq!(
            published_function, function,
            "case {case:?}: the published function name must be the function the stack \
             declared, got {published_function:?}"
        );

        // The mapping the stack published is one Lambda resolves, and it really polls the
        // declared queue: the round trip through that service's own read, before anything is
        // sent.
        let settled = mapping_until_settled(&router, case, &mapping_id).await;
        assert_eq!(
            (
                settled.get("State").and_then(Value::as_str),
                settled.get("EventSourceArn").and_then(Value::as_str),
            ),
            (Some(MAPPING_STATE_ENABLED), Some(queue_arn.as_str())),
            "case {case:?}: the deployed mapping must settle into {MAPPING_STATE_ENABLED} \
             polling the declared queue, got {settled}"
        );

        // Nothing has been sent, so nothing has been invoked: the group holds no line of this
        // row's. That is the positive control for everything below — the output read after the
        // send is output the DELIVERY produced, not output the deployment left behind.
        let quiet_until = Instant::now() + QUIET_BUDGET;
        while Instant::now() < quiet_until {
            let events = filtered_events(&router, &group).await;
            let early: Vec<&(String, String)> = events
                .iter()
                .filter(|(message, _)| message.contains("PROCESSED "))
                .collect();
            assert!(
                early.is_empty(),
                "case {case:?}: no message has been sent yet, so {group:?} must hold no \
                 processed line, got {early:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        for body in &row.bodies {
            sqs(
                &router,
                case,
                "SendMessage",
                &json!({ "QueueUrl": queue_url, "MessageBody": body }),
            )
            .await;
        }

        // The whole chain, waited for at its END: the poller takes a batch, the function runs
        // the deployed package, the handler reads the secret and writes its line, and the line
        // is delivered to the function's own group.
        let wanted: Vec<String> = row
            .bodies
            .iter()
            .map(|body| expected_line(body, row.secret))
            .collect();
        let (streams, events) = awaited_output(&router, case, &group, &wanted).await;

        // Every line the rule owes is there: the record carried `aws:sqs` and the body that was
        // sent, and the handler read the secret the stack deployed.
        for (body, line) in row.bodies.iter().zip(&wanted) {
            assert!(
                events
                    .iter()
                    .any(|(message, _)| message.contains(line.as_str())),
                "case {case:?}: the invocation handed {body:.48?} must have logged \
                 {line:.160?} under {group:?}, got {events:?}"
            );
        }

        // The first invocation CREATED the group and a stream in it, and the events a client
        // filters really belong to a stream that answer reports.
        assert!(
            group_exists(&router, &group).await,
            "case {case:?}: the invocation must create the function's own log group {group:?}, \
             which DescribeLogGroups does not report"
        );
        let described_streams: BTreeSet<&str> = streams.iter().map(String::as_str).collect();
        assert!(
            !described_streams.is_empty() && !described_streams.contains(""),
            "case {case:?}: DescribeLogStreams must report at least one named stream of \
             {group:?}, got {streams:?}"
        );
        for (message, stream) in &events {
            assert!(
                described_streams.contains(stream.as_str()),
                "case {case:?}: the event {message:.80?} names the stream {stream:?}, which \
                 DescribeLogStreams does not report among {streams:?}"
            );
        }
    }
}
