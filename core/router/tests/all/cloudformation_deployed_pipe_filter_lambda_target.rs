// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed `AWS::Pipes::Pipe` with a LAMBDA target and a declared
//! `FilterCriteria` does once its stack reports it created.
//!
//! RULE: a pipe a stack declares is a LIVE pipe — once the stack has created it and
//! EventBridge Pipes reports it resting in the state its `DesiredState` asked for, the pipe
//! polls the Amazon SQS source its declaration named with no client action at all, admits
//! exactly the records its declared `SourceParameters.FilterCriteria` match (every record
//! when it declares none), hands those to the declared Lambda target as ONE invocation per
//! polled batch carrying the batch as a JSON array of record envelopes, and consumes every
//! record it polled from the source — the admitted ones and the dropped ones alike. A
//! declaration is the same resource the client's own `CreatePipe` leaves behind, so none of
//! that depends on how the pipe came to exist.
//!
//! Five crates have to agree for the rule to hold — CloudFormation provisions the queues, the
//! roles, the function and the pipe; EventBridge Pipes runs the pipe; Amazon SQS holds the
//! source and the result queue; Lambda runs the target's deployment package; IAM answers for
//! both execution roles — so the suite lives here rather than in any one of them.
//!
//! Nothing in this suite invokes the function: the only data-plane calls a row makes are the
//! `SendMessage`s that put the records on the source queue and the `ReceiveMessage`s that read
//! the result queue. Every body read back off the result queue was sent by the handler's own
//! `boto3` call out of the batch it was handed, so a stack whose pipe reports healthy and
//! delivers nothing leaves that queue empty and the row fails.
//!
//! The rows differ along what the rule generalises over: WHICH form the filter clause takes (a
//! field of the record's JSON body, the body as the text it was sent as, a `prefix` match, two
//! patterns of which either admits, and no filter at all), how many of the records it admits
//! (one of two, two of three, every one, none of them beyond the control) and the `BatchSize`
//! the records are polled under (one per batch and the whole backlog in one).
//!
//! `crate::cloudformation_deployed_pipe_delivery` asks what the execution role of a declared
//! pipe decides; `crate::pipes_filtered_sqs_delivery` asks the filter question of an
//! API-created pipe with an Amazon SQS target. What is new here is a DECLARED pipe whose
//! target is a function, which is the topology a stack wires an SQS queue to a handler with.
//!
//! The member names are the model's own
//! (`smithy_query(query="shape", shape="PipeSourceParameters,FilterCriteria,Filter,PipeTargetParameters,PipeTargetLambdaFunctionParameters")`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every row deploys in.
const REGION: &str = "us-east-1";

/// The terminal `StackStatus` a deployment that succeeded settles to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a template declaring IAM resources has to be deployed with
/// (`CreateStackInput$Capabilities`).
const IAM_CAPABILITY: &str = "CAPABILITY_IAM";

/// The `PipeState` a pipe whose `DesiredState` is `RUNNING` comes to rest in.
const RUNNING: &str = "RUNNING";

/// The runtime and handler every row's function declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.lambda_handler";

/// How long one invocation of the handler may run, as the function's own `Timeout`: generous,
/// because what is under test is the delivery and not how fast an interpreter gets through a
/// handler.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long a row waits for work nothing asked for — the stack settling, the pipe resting, a
/// batch polled, the target invoked, the handler's own send landing — before it fails.
///
/// Every step of the chain is owed by the CLOCK, so each wait is on the CONDITION, looked for
/// again until it holds; this bound is generous slack whose only job is that the wait
/// TERMINATES. Nothing below asserts on elapsed time.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a row watches for a delivery the filter must have DROPPED, after everything the
/// rule owes has arrived. A pipe that was going to deliver the dropped record would have: it is
/// many times the poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often a queue, a stack or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The logical ids every row's template declares, and the outputs it publishes.
const SOURCE: &str = "PipeSource";
const RESULT: &str = "PipeResult";
const FUNCTION_ROLE: &str = "ForwarderRole";
const FORWARDER: &str = "Forwarder";
const PIPE_ROLE: &str = "PipeRole";
const PIPE: &str = "ThePipe";
const SOURCE_URL_OUTPUT: &str = "SourceUrl";
const RESULT_URL_OUTPUT: &str = "ResultUrl";
const PIPE_NAME_OUTPUT: &str = "PipeName";

/// The environment variable the handler reads the result queue's url from. The emulator reads
/// none of it: it is the handler's own, passed to it by the declaration.
const RESULT_QUEUE_VARIABLE: &str = "RESULT_QUEUE";

/// The calls an Amazon SQS source's stage makes on the source, which the pipe's execution role
/// has to admit for the pipe to poll at all. Source: AWS's documented required permissions for
/// an Amazon SQS pipe source.
const SOURCE_READS: [&str; 3] = [
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
];

/// The call the target stage of a Lambda function target makes.
const INVOKE: &str = "lambda:InvokeFunction";

/// The call the handler's own `boto3` makes.
const SEND: &str = "sqs:SendMessage";

/// The attributes that together say how many records the source still HOLDS: the ones waiting
/// on it plus the ones a consumer has taken and not deleted.
const HELD_ATTRIBUTES: [&str; 2] = [
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
];

/// The handler every row deploys inline: it forwards the body of every record of the batch it
/// was handed to the result queue the declaration named it through its environment, as one
/// message each, through the AWS SDK its runtime supplies — and refuses a payload that is not
/// the JSON array a pipe hands a function target.
const FORWARDING_HANDLER: &str = r#"
import os

import boto3

RESULT_QUEUE = os.environ["RESULT_QUEUE"]
CLIENT = boto3.client("sqs")


def lambda_handler(event, context):
    if not isinstance(event, list):
        raise ValueError("a batch is handed over as a list, got " + type(event).__name__)
    for record in event:
        CLIENT.send_message(QueueUrl=RESULT_QUEUE, MessageBody=record["body"])
    return {"forwarded": len(event)}
"#;

/// The whole emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`):
/// the pipe's own calls and the handler's SDK call are dispatched back into this same router, so
/// the queue this suite reads is the queue that run wrote.
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

/// One Amazon SQS call (`awsJson1_0`), as its status and parsed body.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
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

/// `DescribePipe` over EventBridge Pipes' own `restJson1` wire, on the URI that operation's
/// `@http` binding declares (`GET /v1/pipes/{Name}`).
async fn describe_pipe(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/v1/pipes/{name}"))
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The stack as it reads once its operation has reached a TERMINAL status, polled rather than
/// slept for: the function's and the pipe's own creations hold the deployment open, so how many
/// spans it takes is the deployment's to decide and not this suite's to assume.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack {stack:?} never reached a terminal status, last {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The pipe as EventBridge Pipes reports it once it has come to rest, answering its
/// `CurrentState` and `StateReason`.
async fn resting_pipe(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = describe_pipe(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe {name:?} answered {status} {described}"
        );
        let state = described["CurrentState"].as_str().unwrap_or_default();
        let reason = described["StateReason"].as_str().unwrap_or_default();
        if state == RUNNING || Instant::now() >= deadline {
            return (state.to_string(), reason.to_string());
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Send `body` to the queue at `url`.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, answered) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage answered {status} {answered}"
    );
}

/// One `ReceiveMessage` of up to ten messages at `url`, as bodies, left on the queue:
/// `VisibilityTimeout: 0` takes no checkout, so a later look sees them again and nothing this
/// suite reads is hidden from the next assertion.
async fn receive_now(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, answered) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 0 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {answered}"
    );
    answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every body seen at `url` until each of `wanted` has been seen among them, or the budget
/// lapses.
async fn collected_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: &[String],
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        seen.extend(receive_now(router, case, url).await);
        if wanted.iter().all(|body| seen.contains(body)) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every body seen at `url` over the whole of [`QUIET`], for the assertion that nothing more
/// arrives.
async fn collected_while_quiet(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeSet<String> {
    let until = Instant::now() + QUIET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < until {
        seen.extend(receive_now(router, case, url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    seen
}

/// How many records the queue at `url` still HOLDS ([`HELD_ATTRIBUTES`]).
async fn held(router: &MultiServiceRouter, case: &str, url: &str) -> i64 {
    let (status, answered) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": HELD_ATTRIBUTES }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {answered}"
    );
    HELD_ATTRIBUTES
        .iter()
        .map(|attribute| {
            answered["Attributes"][attribute]
                .as_str()
                .unwrap_or_default()
                .parse()
                .unwrap_or(-1)
        })
        .sum()
}

/// Wait until the queue at `url` holds `want` records, answering what it held last.
async fn wait_for_held(router: &MultiServiceRouter, case: &str, url: &str, want: i64) -> i64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let counted = held(router, case, url).await;
        if counted == want || Instant::now() >= deadline {
            return counted;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// HOW a row's declaration SPELLS the pattern the pipe filters by. The pattern a pipe filters
/// by is the one its declaration RESOLVES to, so the three spellings of one pattern configure
/// one pipe — which is what lets a template carry a per-deployment pattern at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// The pattern text stands in the declaration verbatim.
    Verbatim,
    /// The pattern text is a stack PARAMETER the declaration reads with `Ref`.
    Parameter,
    /// The declaration COMPOSES the pattern with `Fn::Sub` out of a parameter naming the one
    /// value the pattern turns on.
    Substituted,
}

/// The parameter name a row that does not spell its pattern verbatim declares.
const PATTERN_PARAMETER: &str = "FilterPattern";
/// The parameter name the `Fn::Sub` spelling declares, and the placeholder it fills.
const MARKER_PARAMETER: &str = "Marker";

/// One row: the filter the declaration carries, the records sent to the source, and which of
/// them the filter admits.
struct Row {
    case: &'static str,
    /// The prefix every name this row's template declares carries, so two rows never share a
    /// resource.
    prefix: &'static str,
    /// `SourceParameters.FilterCriteria`'s patterns, each the JSON text `Filter$Pattern`
    /// carries, or empty for a declaration that states no `FilterCriteria` at all.
    patterns: &'static [&'static str],
    /// How the declaration spells them ([`Spelling`]). Only a single-pattern row spells one
    /// other than [`Spelling::Verbatim`].
    spelling: Spelling,
    /// The value the `Fn::Sub` spelling's pattern turns on, which is the one part of the
    /// pattern that deployment states.
    marker: &'static str,
    /// `SourceParameters.SqsQueueParameters.BatchSize`.
    batch_size: i64,
    /// The bodies sent to the source, each paired with whether the filter ADMITS it.
    records: &'static [(&'static str, bool)],
}

impl Row {
    fn stack(&self) -> String {
        format!("{}-stack", self.prefix)
    }

    fn pipe(&self) -> String {
        format!("{}-pipe", self.prefix)
    }

    /// The bodies the target must be handed, in no particular order.
    fn admitted(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, admitted)| *admitted)
            .map(|(body, _)| (*body).to_string())
            .collect()
    }

    /// The bodies the filter must DROP.
    fn dropped(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|(_, admitted)| !*admitted)
            .map(|(body, _)| (*body).to_string())
            .collect()
    }

    /// The `(ParameterKey, ParameterValue)` pairs the deployment of this row's template is
    /// made with: none for a pattern spelled verbatim, the pattern itself for the `Ref`
    /// spelling, and the one value the pattern turns on for the `Fn::Sub` spelling.
    fn parameters(&self) -> Vec<(String, String)> {
        match self.spelling {
            Spelling::Verbatim => Vec::new(),
            Spelling::Parameter => vec![(
                PATTERN_PARAMETER.to_string(),
                self.patterns
                    .first()
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            )],
            Spelling::Substituted => {
                vec![(MARKER_PARAMETER.to_string(), self.marker.to_string())]
            }
        }
    }

    /// What the declaration states as each `Filter$Pattern`, under this row's spelling.
    fn declared_patterns(&self) -> Vec<Value> {
        match self.spelling {
            Spelling::Verbatim => self
                .patterns
                .iter()
                .map(|pattern| json!({ "Pattern": pattern }))
                .collect(),
            Spelling::Parameter => {
                vec![json!({ "Pattern": { "Ref": PATTERN_PARAMETER } })]
            }
            // The pattern with the value deployment states put back in as the placeholder
            // `Fn::Sub` fills, so what the declaration composes is this row's own pattern.
            Spelling::Substituted => {
                let composed = self
                    .patterns
                    .first()
                    .copied()
                    .unwrap_or_default()
                    .replace(self.marker, &format!("${{{MARKER_PARAMETER}}}"));
                vec![json!({ "Pattern": { "Fn::Sub": composed } })]
            }
        }
    }

    /// The `Parameters` section this row's template declares for its spelling.
    fn parameter_section(&self) -> Value {
        match self.spelling {
            Spelling::Verbatim => json!({}),
            Spelling::Parameter => json!({ PATTERN_PARAMETER: { "Type": "String" } }),
            Spelling::Substituted => json!({ MARKER_PARAMETER: { "Type": "String" } }),
        }
    }

    /// The one template this row deploys: the source and result queues, a role for the
    /// forwarder and a role for the pipe, the forwarder itself, and the pipe wiring the source
    /// to it under this row's filter.
    fn template(&self) -> String {
        let mut source_parameters = json!({
            "SqsQueueParameters": { "BatchSize": self.batch_size },
        });
        if !self.patterns.is_empty() {
            source_parameters["FilterCriteria"] = json!({ "Filters": self.declared_patterns() });
        }
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Parameters": self.parameter_section(),
            "Resources": {
                SOURCE: {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": format!("{}-source", self.prefix) },
                },
                RESULT: {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": format!("{}-result", self.prefix) },
                },
                FUNCTION_ROLE: {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": trust("lambda.amazonaws.com"),
                        "Policies": [{
                            "PolicyName": "forward-each-record",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": [SEND],
                                    "Resource": { "Fn::GetAtt": [RESULT, "Arn"] },
                                }],
                            },
                        }],
                    },
                },
                FORWARDER: {
                    "Type": "AWS::Lambda::Function",
                    "Properties": {
                        "FunctionName": format!("{}-forwarder", self.prefix),
                        "Runtime": RUNTIME,
                        "Handler": HANDLER,
                        "Timeout": FUNCTION_TIMEOUT,
                        "Role": { "Fn::GetAtt": [FUNCTION_ROLE, "Arn"] },
                        "Environment": {
                            "Variables": { RESULT_QUEUE_VARIABLE: { "Ref": RESULT } },
                        },
                        "Code": { "ZipFile": FORWARDING_HANDLER },
                    },
                },
                PIPE_ROLE: {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": trust("pipes.amazonaws.com"),
                        "Policies": [{
                            "PolicyName": "move-records",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [
                                    {
                                        "Effect": "Allow",
                                        "Action": SOURCE_READS,
                                        "Resource": { "Fn::GetAtt": [SOURCE, "Arn"] },
                                    },
                                    {
                                        "Effect": "Allow",
                                        "Action": [INVOKE],
                                        "Resource": { "Fn::GetAtt": [FORWARDER, "Arn"] },
                                    },
                                ],
                            },
                        }],
                    },
                },
                PIPE: {
                    "Type": "AWS::Pipes::Pipe",
                    "Properties": {
                        "Name": self.pipe(),
                        "Source": { "Fn::GetAtt": [SOURCE, "Arn"] },
                        "SourceParameters": source_parameters,
                        "Target": { "Fn::GetAtt": [FORWARDER, "Arn"] },
                        "TargetParameters": {
                            "LambdaFunctionParameters": {
                                "InvocationType": "REQUEST_RESPONSE",
                            },
                        },
                        "RoleArn": { "Fn::GetAtt": [PIPE_ROLE, "Arn"] },
                        "DesiredState": RUNNING,
                    },
                },
            },
            "Outputs": {
                SOURCE_URL_OUTPUT: { "Value": { "Ref": SOURCE } },
                RESULT_URL_OUTPUT: { "Value": { "Ref": RESULT } },
                PIPE_NAME_OUTPUT: { "Value": { "Ref": PIPE } },
            },
        })
        .to_string()
    }
}

/// The trust document a role one `service` principal acts as carries.
fn trust(service: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The table. Its columns span the clause FORMS a declared pattern takes — a field of the
/// record's JSON body, the body as the text it was sent as, a `prefix` match, two patterns of
/// which either admits, and no `FilterCriteria` at all — HOW the declaration spells the pattern
/// ([`Spelling`]), the share of the records admitted, and the `BatchSize` they are polled under.
const ROWS: &[Row] = &[
    Row {
        case: "a clause on a field of the record's JSON body, one of two records admitted",
        prefix: "cfn-pipe-filter-body-field",
        patterns: &[r#"{"body":{"marker":["keep"]}}"#],
        spelling: Spelling::Verbatim,
        marker: "keep",
        batch_size: 1,
        records: &[
            (r#"{"marker":"keep","order":1}"#, true),
            (r#"{"marker":"drop","order":2}"#, false),
        ],
    },
    Row {
        case: "a clause on the body as the TEXT it was sent as, one of three records admitted",
        prefix: "cfn-pipe-filter-body-text",
        patterns: &[r#"{"body":["keep-the-text"]}"#],
        spelling: Spelling::Verbatim,
        marker: "keep-the-text",
        batch_size: 10,
        records: &[
            ("keep-the-text", true),
            ("another-text", false),
            ("keep-the-text-and-more", false),
        ],
    },
    Row {
        case: "a prefix clause on a body field, two of three records admitted",
        prefix: "cfn-pipe-filter-prefix",
        patterns: &[r#"{"body":{"marker":[{"prefix":"keep"}]}}"#],
        spelling: Spelling::Verbatim,
        marker: "keep",
        batch_size: 2,
        records: &[
            (r#"{"marker":"keep-one"}"#, true),
            (r#"{"marker":"keep-two"}"#, true),
            (r#"{"marker":"leave-three"}"#, false),
        ],
    },
    Row {
        case: "two patterns of which either admits, two of three records admitted",
        prefix: "cfn-pipe-filter-two-patterns",
        patterns: &[
            r#"{"body":{"marker":["first"]}}"#,
            r#"{"body":{"marker":["second"]}}"#,
        ],
        spelling: Spelling::Verbatim,
        marker: "first",
        batch_size: 10,
        records: &[
            (r#"{"marker":"first"}"#, true),
            (r#"{"marker":"second"}"#, true),
            (r#"{"marker":"third"}"#, false),
        ],
    },
    Row {
        case: "no FilterCriteria at all, every record admitted",
        prefix: "cfn-pipe-filter-none",
        patterns: &[],
        spelling: Spelling::Verbatim,
        marker: "",
        batch_size: 1,
        records: &[
            (r#"{"marker":"anything"}"#, true),
            ("plain text body", true),
        ],
    },
    Row {
        case: "the pattern read from a stack parameter, one of two records admitted",
        prefix: "cfn-pipe-filter-parameter",
        patterns: &[r#"{"body":{"marker":["from-a-parameter"]}}"#],
        spelling: Spelling::Parameter,
        marker: "from-a-parameter",
        batch_size: 1,
        records: &[
            (r#"{"marker":"from-a-parameter"}"#, true),
            (r#"{"marker":"not-this-one"}"#, false),
        ],
    },
    Row {
        case: "the pattern composed by Fn::Sub out of a parameter, one of two records admitted",
        prefix: "cfn-pipe-filter-substituted",
        patterns: &[r#"{"body":{"marker":["from-a-substitution"]}}"#],
        spelling: Spelling::Substituted,
        marker: "from-a-substitution",
        batch_size: 10,
        records: &[
            (r#"{"marker":"from-a-substitution"}"#, true),
            (r#"{"marker":"not-this-one"}"#, false),
        ],
    },
];

/// The change set a deploying tool plans under.
const CHANGE_SET: &str = "a-deploy";

/// The awsQuery members one deployment of `row`'s template is made with: the stack, the
/// template, the capability its IAM resources need, and the `Parameters` its spelling of the
/// pattern takes.
fn deployment_members(row: &Row, stack: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut members: Vec<(String, String)> = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), row.template()),
        (
            "Capabilities.member.1".to_string(),
            IAM_CAPABILITY.to_string(),
        ),
    ];
    for (name, value) in extra {
        members.push(((*name).to_string(), (*value).to_string()));
    }
    for (index, (key, value)) in row.parameters().iter().enumerate() {
        let position = index + 1;
        members.push((
            format!("Parameters.member.{position}.ParameterKey"),
            key.clone(),
        ));
        members.push((
            format!("Parameters.member.{position}.ParameterValue"),
            value.clone(),
        ));
    }
    members
}

/// [`deployment_members`] as the borrowed pairs [`cloudformation`] takes.
fn as_pairs(members: &[(String, String)]) -> Vec<(&str, &str)> {
    members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// RULE, over the SAME declaration deployed the way a deploying tool deploys one — a `CREATE`
/// change set planned and then executed, which is what `aws cloudformation deploy` does: the
/// pipe that stack creates is as live as the one a `CreateStack` creates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_set_deployed_pipes_filter_decides_what_reaches_its_function_target() {
    let router = build_router();
    let row = &ROWS[0];
    let case = row.case;
    let stack = format!("{}-change-set", row.prefix);

    let members = deployment_members(
        row,
        &stack,
        &[("ChangeSetName", CHANGE_SET), ("ChangeSetType", "CREATE")],
    );
    let (status, planned) = cloudformation(&router, "CreateChangeSet", &as_pairs(&members)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChangeSet answered {status} {planned}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = cloudformation(
            &router,
            "DescribeChangeSet",
            &[("StackName", &stack), ("ChangeSetName", CHANGE_SET)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChangeSet answered {status} {described}"
        );
        let reported = opt_element(&described, "Status").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the change set never became executable, last {reported:?} in \
             {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let (status, executed) = cloudformation(
        &router,
        "ExecuteChangeSet",
        &[("StackName", &stack), ("ChangeSetName", CHANGE_SET)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ExecuteChangeSet answered {status} {executed}"
    );

    let described = settled_stack(&router, case, &stack).await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the executed change set must deploy the stack, got {reported:?} with \
         {described}"
    );
    let outputs = outputs_of(&described);
    let published = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish {name}, got {described}")
        })
    };
    let source_url = published(SOURCE_URL_OUTPUT);
    let result_url = published(RESULT_URL_OUTPUT);
    let pipe_name = published(PIPE_NAME_OUTPUT);
    let (state, reason) = resting_pipe(&router, case, &pipe_name).await;
    assert_eq!(
        state, RUNNING,
        "case {case:?}: the pipe the executed change set created must rest in {RUNNING}, got \
         {state:?} with reason {reason:?}"
    );

    for (body, _) in row.records {
        send(&router, case, &source_url, body).await;
    }
    let wanted = row.admitted();
    let seen = collected_until(&router, case, &result_url, &wanted).await;
    for body in &wanted {
        assert!(
            seen.contains(body),
            "case {case:?}: the record {body:?} is admitted by {:?}, so the pipe a change set \
             deployed must hand it to the function target, which forwarded {seen:?}",
            row.patterns
        );
    }
    let later = collected_while_quiet(&router, case, &result_url).await;
    for body in row.dropped() {
        assert!(
            !seen.contains(&body) && !later.contains(&body),
            "case {case:?}: the record {body:?} is dropped by {:?}, so nothing of it may reach \
             the function target, which forwarded {seen:?} then {later:?}",
            row.patterns
        );
    }
}

/// RULE, every row: the stack creates the pipe, EventBridge Pipes reports it `RUNNING`, every
/// record the declared `FilterCriteria` admit reaches the Lambda target (observed as the body
/// the handler forwarded to the result queue), no record it drops ever does, and the source
/// queue is drained of all of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declared_pipes_filter_decides_what_reaches_its_function_target() {
    // ONE emulator for every row, so a row that observes a delivery and a row's quiet window
    // are read in the same account.
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();
        let members = deployment_members(row, &stack, &[]);
        let (status, created) = cloudformation(&router, "CreateStack", &as_pairs(&members)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} {created}"
        );

        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: the stack must deploy the queues, the roles, the function and the \
             pipe, got {reported:?} with {described}"
        );

        // The identifiers this row drives the deployment by are the ones the STACK publishes.
        let outputs = outputs_of(&described);
        let published = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack must publish {name}, got {described}")
            })
        };
        let source_url = published(SOURCE_URL_OUTPUT);
        let result_url = published(RESULT_URL_OUTPUT);
        let pipe_name = published(PIPE_NAME_OUTPUT);
        assert_eq!(
            pipe_name,
            row.pipe(),
            "case {case:?}: the stack must publish the declared pipe name"
        );

        // The pipe the stack says it created is the live pipe EventBridge Pipes holds, resting
        // in the state the declaration asked for.
        let (state, reason) = resting_pipe(&router, case, &pipe_name).await;
        assert_eq!(
            state, RUNNING,
            "case {case:?}: the declared pipe must rest in {RUNNING}, got {state:?} with reason \
             {reason:?}"
        );

        for (body, _) in row.records {
            send(&router, case, &source_url, body).await;
        }

        // Every admitted record reaches the function target: the body read back here is one the
        // handler forwarded out of the batch it was handed.
        let wanted = row.admitted();
        let seen = collected_until(&router, case, &result_url, &wanted).await;
        for body in &wanted {
            assert!(
                seen.contains(body),
                "case {case:?}: the record {body:?} is admitted by {:?}, so the pipe must hand \
                 it to the function target, which forwarded {seen:?}",
                row.patterns
            );
        }

        // ...and nothing the filter drops ever does, over a window in which a delivery that was
        // going to happen would have.
        let later = collected_while_quiet(&router, case, &result_url).await;
        for body in row.dropped() {
            assert!(
                !seen.contains(&body) && !later.contains(&body),
                "case {case:?}: the record {body:?} is dropped by {:?}, so nothing of it may \
                 reach the function target, which forwarded {seen:?} then {later:?}",
                row.patterns
            );
        }

        // Every record the pipe polled — admitted or dropped — was consumed from the source.
        let still_held = wait_for_held(&router, case, &source_url, 0).await;
        assert_eq!(
            still_held, 0,
            "case {case:?}: the source must be drained of every record the pipe polled, it still \
             holds {still_held}"
        );
    }
}
