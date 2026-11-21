// SPDX-License-Identifier: CC0-1.0

//! What an execution role that withholds only SOME of the calls a pipe's source stage makes
//! decides — asked of a CloudFormation stack that declares such a pipe BESIDE a correctly
//! scoped one.
//!
//! RULE: **a pipe whose execution role withholds some, but not all, of the calls its source
//! stage makes on its source is a pipe EventBridge Pipes CREATES: it reaches the state its
//! `DesiredState` asked for and reports no failure, and the withheld call is one execution of
//! a running pipe failing — nothing reaches its target and its records stay on its source. So
//! a stack that declares one of those beside a correctly scoped pipe DEPLOYS, and the
//! correctly scoped pipe polls its source and delivers to its Lambda target with no client
//! action at all.** Only a role that leaves a pipe NO call on its source keeps the pipe from
//! coming up, which is `crate::cloudformation_deployed_pipe_delivery`'s rollback table.
//!
//! That asymmetry is what makes the under-scoped pipe OBSERVABLE: the correctly scoped pipe
//! in the same stack is the control that says the wiring works, so a client can tell
//! "withheld by IAM" from "the whole topology is dead". A pipe the deployment refuses takes
//! that control down with it — the stack it is in is rolled back — and then neither leg says
//! anything.
//!
//! Five crates have to agree for the rule to hold — CloudFormation provisions the queues, the
//! roles, the function and the two pipes; EventBridge Pipes runs them; Amazon SQS holds the
//! sources and the result queue; Lambda runs the target's deployment package; IAM answers for
//! every role — so the suite lives here rather than in any one of them.
//!
//! Nothing here invokes the function: the only data-plane calls a row makes are the
//! `SendMessage`s that put a record on each source and the `ReceiveMessage`s that read the
//! result queue. Every body read back off the result queue was sent by the handler's own
//! `boto3` call out of the batch it was handed.
//!
//! The rows differ along what the rule generalises over: HOW the sibling's role withholds the
//! call that takes its source's records (no statement about that action at all, an explicit
//! `Deny` over a wildcard allow, the action allowed on ANOTHER queue only), which of the source
//! stage's other calls it is left with, and whether the declaration NAMES its pipes at all —
//! the last row leaving both names to CloudFormation under a stack name long enough that the
//! name it generates has to be fitted to what a pipe's name may be.
//!
//! The member names are the model's own
//! (`smithy_query(query="shape", shape="PipeSourceParameters,PipeTargetParameters,PipeTargetLambdaFunctionParameters")`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every row deploys in.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

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

/// How long a row waits for work nothing asked for — the stack settling, a pipe resting, a
/// batch polled, the target invoked, the handler's own send landing — before it fails.
///
/// Every step of the chain is owed by the CLOCK, so each wait is on the CONDITION, looked for
/// again until it holds; this bound is generous slack whose only job is that the wait
/// TERMINATES. Nothing below asserts on elapsed time.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a row watches for a delivery the under-scoped pipe must NOT have made, after
/// everything the rule owes has arrived. A pipe that was going to move that record would have:
/// it is many times the poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often a queue, a stack or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The logical ids every row's template declares, and the outputs it publishes.
const OPEN_SOURCE: &str = "OpenSource";
const WITHHELD_SOURCE: &str = "WithheldSource";
const RESULT: &str = "PipeResult";
const FUNCTION_ROLE: &str = "ForwarderRole";
const FORWARDER: &str = "Forwarder";
const OPEN_ROLE: &str = "OpenPipeRole";
const WITHHELD_ROLE: &str = "WithheldPipeRole";
const OPEN_PIPE: &str = "OpenPipe";
const WITHHELD_PIPE: &str = "WithheldPipe";
const OPEN_SOURCE_URL_OUTPUT: &str = "OpenSourceUrl";
const WITHHELD_SOURCE_URL_OUTPUT: &str = "WithheldSourceUrl";
const RESULT_URL_OUTPUT: &str = "ResultUrl";
const OPEN_PIPE_NAME_OUTPUT: &str = "OpenPipeName";
const WITHHELD_PIPE_NAME_OUTPUT: &str = "WithheldPipeName";

/// The longest name a pipe may carry, and the characters one may be built from. Source:
/// `smithy_query(query="shape", shape="PipeName")` — `@length(min: 1, max: 64)` with pattern
/// `^[\.\-_A-Za-z0-9]+$`. A declaration that names no pipe is created under the name
/// CloudFormation generates for it, so that name has to be one `CreatePipe` takes.
const PIPE_NAME_MAX: usize = 64;

/// The environment variable the handler reads the result queue's url from. The emulator reads
/// none of it: it is the handler's own, passed to it by the declaration.
const RESULT_QUEUE_VARIABLE: &str = "RESULT_QUEUE";

/// The calls an Amazon SQS source's stage makes on the source. Source: AWS's documented
/// required permissions for an Amazon SQS pipe source.
const SOURCE_READS: [&str; 3] = [
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
];

/// The call the target stage of a Lambda function target makes.
const INVOKE: &str = "lambda:InvokeFunction";

/// The call the handler's own `boto3` makes.
const SEND: &str = "sqs:SendMessage";

/// The one call of [`SOURCE_READS`] every row's sibling role withholds: the one that TAKES the
/// records from an Amazon SQS source.
const TAKES_THE_RECORDS: &str = "sqs:ReceiveMessage";

/// The attributes that together say how many records a queue still HOLDS: the ones waiting on
/// it plus the ones a consumer has taken and not deleted ("messages that are in flight",
/// `ApproximateNumberOfMessagesNotVisible`), which is what a record a pipe could not move is
/// one of.
const HELD_ATTRIBUTES: [&str; 2] = [
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
];

/// The handler every row deploys inline, written the way a forwarder for this topology is: for
/// every record of the batch it was handed it reports the marker the record's own JSON body
/// carries, beside the envelope members that say where the record came from, to the result
/// queue the declaration named it through its environment — through the AWS SDK and the
/// standard library its runtime supplies — and refuses a payload that is not the JSON array a
/// pipe hands a function target.
const FORWARDING_HANDLER: &str = r#"
import json
import logging
import os

import boto3

LOGGER = logging.getLogger()
LOGGER.setLevel(logging.INFO)
RESULT_QUEUE = os.environ["RESULT_QUEUE"]
CLIENT = boto3.client("sqs")


def lambda_handler(event, context):
    if not isinstance(event, list):
        raise ValueError("a batch is handed over as a list, got " + type(event).__name__)
    LOGGER.info("forwarding %s record(s)", len(event))
    for record in event:
        payload = json.loads(record["body"])
        CLIENT.send_message(
            QueueUrl=RESULT_QUEUE,
            MessageBody=json.dumps(
                {
                    "marker": payload["marker"],
                    "messageId": record["messageId"],
                    "source": record["eventSourceARN"],
                    "region": record["awsRegion"],
                }
            ),
        )
    return {"forwarded": len(event)}
"#;

/// The whole emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`):
/// the pipes' own calls and the handler's SDK call are dispatched back into this same router,
/// so the queue this suite reads is the queue that run wrote.
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
/// slept for: the function's and the pipes' own creations hold the deployment open, so how
/// many spans it takes is the deployment's to decide and not this suite's to assume.
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

/// Every MARKER seen at `url` until `wanted` has been seen among them, or the budget lapses.
///
/// What is read off the result queue is the document the handler sent, so the marker is the
/// `marker` member of that document — the value the handler read out of the record's own body.
async fn markers_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        seen.extend(markers_of(receive_now(router, case, url).await));
        if seen.contains(wanted) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every MARKER seen at `url` over the whole of [`QUIET`], for the assertion that nothing more
/// arrives.
async fn markers_while_quiet(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeSet<String> {
    let until = Instant::now() + QUIET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < until {
        seen.extend(markers_of(receive_now(router, case, url).await));
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    seen
}

/// The `marker` member of each forwarded document, and the body itself for one that spells no
/// document with such a member — so a body the handler sent in another shape is still reported
/// by a failing assertion rather than silently dropped.
fn markers_of(bodies: Vec<String>) -> Vec<String> {
    bodies
        .into_iter()
        .map(|body| {
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|document| {
                    document
                        .get("marker")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or(body)
        })
        .collect()
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

/// One row: the statements the under-scoped sibling's execution role carries, built from the
/// row's own resource names.
struct Row {
    case: &'static str,
    /// The prefix every name this row's template declares carries, so two rows never share a
    /// resource.
    prefix: &'static str,
    /// The sibling role's statements. Every one of them withholds [`TAKES_THE_RECORDS`] on
    /// that pipe's own source and leaves it at least one other call on it.
    withheld: fn(&Row) -> Value,
    /// Whether the declaration NAMES its pipes. A declaration that does not is created under
    /// the name CloudFormation generates for it, which is what `Ref` then publishes.
    names_its_pipes: bool,
}

impl Row {
    fn stack(&self) -> String {
        format!("{}-stack", self.prefix)
    }

    fn open_source(&self) -> String {
        format!("{}-open-source", self.prefix)
    }

    fn withheld_source(&self) -> String {
        format!("{}-withheld-source", self.prefix)
    }

    fn result(&self) -> String {
        format!("{}-result", self.prefix)
    }

    /// The `Name` this row's declaration gives its OPEN pipe, or `None` for a row that names
    /// neither of its pipes.
    fn open_pipe(&self) -> Option<String> {
        self.names_its_pipes
            .then(|| format!("{}-open-pipe", self.prefix))
    }

    /// See [`Self::open_pipe`].
    fn withheld_pipe(&self) -> Option<String> {
        self.names_its_pipes
            .then(|| format!("{}-withheld-pipe", self.prefix))
    }

    fn function(&self) -> String {
        format!("{}-forwarder", self.prefix)
    }

    /// The ARN of the queue called `name`: the same total function of the Region and the name
    /// Amazon SQS mints one from, so a statement here names the very resource the pipe's call
    /// is made on.
    fn queue_arn(name: &str) -> String {
        format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
    }

    /// The marker this row sends to its OPEN source: the value the handler reports forward,
    /// which is what says the correctly scoped pipe delivered.
    fn open_marker(&self) -> String {
        format!("{}-open", self.prefix)
    }

    /// The marker this row sends to its WITHHELD source: the value nothing may report.
    fn withheld_marker(&self) -> String {
        format!("{}-withheld", self.prefix)
    }

    /// The body a record carrying `marker` is sent as: the JSON document the handler reads the
    /// marker out of.
    fn body_for(marker: &str) -> String {
        format!(r#"{{"marker":"{marker}"}}"#)
    }

    /// The statements of the role the correctly scoped pipe acts as: every call its stages
    /// make, on exactly the resource each is made on.
    fn admits_every_call(&self) -> Value {
        json!([
            {
                "Effect": "Allow",
                "Action": SOURCE_READS,
                "Resource": Self::queue_arn(&self.open_source()),
            },
            {
                "Effect": "Allow",
                "Action": [INVOKE],
                "Resource": { "Fn::GetAtt": [FORWARDER, "Arn"] },
            },
        ])
    }

    /// The one template this row deploys: three queues, the forwarder and its role, a role per
    /// pipe, and the two pipes side by side on one Lambda target.
    fn template(&self) -> String {
        json!({
            "Resources": {
                OPEN_SOURCE: {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.open_source() },
                },
                WITHHELD_SOURCE: {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.withheld_source() },
                },
                RESULT: {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.result() },
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
                        "FunctionName": self.function(),
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
                OPEN_ROLE: role(self.admits_every_call()),
                WITHHELD_ROLE: role((self.withheld)(self)),
                OPEN_PIPE: pipe(self.open_pipe().as_deref(), OPEN_SOURCE, OPEN_ROLE),
                WITHHELD_PIPE: pipe(
                    self.withheld_pipe().as_deref(),
                    WITHHELD_SOURCE,
                    WITHHELD_ROLE,
                ),
            },
            "Outputs": {
                OPEN_SOURCE_URL_OUTPUT: { "Value": { "Ref": OPEN_SOURCE } },
                WITHHELD_SOURCE_URL_OUTPUT: { "Value": { "Ref": WITHHELD_SOURCE } },
                RESULT_URL_OUTPUT: { "Value": { "Ref": RESULT } },
                // "Ref returns the pipe name", which is each pipe's own identity whether the
                // declaration stated it or CloudFormation generated it.
                OPEN_PIPE_NAME_OUTPUT: { "Value": { "Ref": OPEN_PIPE } },
                WITHHELD_PIPE_NAME_OUTPUT: { "Value": { "Ref": WITHHELD_PIPE } },
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

/// An `AWS::IAM::Role` a pipe acts as, carrying `statements` as its one inline policy.
fn role(statements: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": trust("pipes.amazonaws.com"),
            "Policies": [{
                "PolicyName": "move-records",
                "PolicyDocument": { "Version": "2012-10-17", "Statement": statements },
            }],
        },
    })
}

/// An `AWS::Pipes::Pipe` wiring the queue declared as `source` to the forwarder, acting as the
/// role declared as `role_id`, asking to RUN and polling one record per batch. A declaration
/// states its `Name` or leaves it to CloudFormation.
fn pipe(name: Option<&str>, source: &str, role_id: &str) -> Value {
    let mut properties = json!({
        "Source": { "Fn::GetAtt": [source, "Arn"] },
        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
        "Target": { "Fn::GetAtt": [FORWARDER, "Arn"] },
        "TargetParameters": {
            "LambdaFunctionParameters": { "InvocationType": "REQUEST_RESPONSE" },
        },
        "RoleArn": { "Fn::GetAtt": [role_id, "Arn"] },
        "DesiredState": RUNNING,
    });
    if let Some(name) = name {
        properties["Name"] = json!(name);
    }
    json!({ "Type": "AWS::Pipes::Pipe", "Properties": properties })
}

/// The table. Its columns span HOW a role withholds the call that takes its source's records
/// while leaving the pipe at least one other call on that same source, and whether the
/// declaration NAMES its pipes at all.
const ROWS: &[Row] = &[
    Row {
        // Nothing at all is stated about the record-taking call; the role may still look at
        // the queue and delete from it, and may invoke the target.
        case: "a sibling role that states nothing about taking its source's records",
        prefix: "cfn-pipe-sibling-unstated",
        names_its_pipes: true,
        withheld: |row| {
            json!([
                {
                    "Effect": "Allow",
                    "Action": ["sqs:GetQueueAttributes", "sqs:DeleteMessage"],
                    "Resource": Row::queue_arn(&row.withheld_source()),
                },
                {
                    "Effect": "Allow",
                    "Action": [INVOKE],
                    "Resource": { "Fn::GetAtt": [FORWARDER, "Arn"] },
                },
            ])
        },
    },
    Row {
        // Allowed everything and then denied the one call: IAM's deny wins over the wildcard
        // allow, so this pipe may do everything but take a record.
        case: "a sibling role whose wildcard grant is denied the record-taking call",
        prefix: "cfn-pipe-sibling-denied",
        names_its_pipes: true,
        withheld: |row| {
            json!([
                { "Effect": "Allow", "Action": ["sqs:*", "lambda:*"], "Resource": "*" },
                {
                    "Effect": "Deny",
                    "Action": [TAKES_THE_RECORDS],
                    "Resource": Row::queue_arn(&row.withheld_source()),
                },
            ])
        },
    },
    Row {
        // The record-taking call allowed on ANOTHER queue — the sibling's own source is left
        // with the attributes read alone.
        case: "a sibling role whose record-taking grant names another queue than its source",
        prefix: "cfn-pipe-sibling-elsewhere",
        names_its_pipes: true,
        withheld: |row| {
            json!([
                {
                    "Effect": "Allow",
                    "Action": [TAKES_THE_RECORDS],
                    "Resource": Row::queue_arn(&row.open_source()),
                },
                {
                    "Effect": "Allow",
                    "Action": ["sqs:GetQueueAttributes"],
                    "Resource": Row::queue_arn(&row.withheld_source()),
                },
                {
                    "Effect": "Allow",
                    "Action": [INVOKE],
                    "Resource": { "Fn::GetAtt": [FORWARDER, "Arn"] },
                },
            ])
        },
    },
    Row {
        // NEITHER pipe is named, under a stack name long enough that the name CloudFormation
        // generates (`<stack>-<logical id>-<unique>`) is longer than a pipe's name may be: both
        // are still pipes EventBridge Pipes creates, so the deployment stands on the generated
        // name being one `CreatePipe` takes.
        case: "a declaration that names neither of its pipes, under a long stack name",
        prefix: "cfn-pipe-sibling-with-no-declared-pipe-name",
        names_its_pipes: false,
        withheld: |row| {
            json!([
                {
                    "Effect": "Allow",
                    "Action": ["sqs:GetQueueAttributes", "sqs:DeleteMessage"],
                    "Resource": Row::queue_arn(&row.withheld_source()),
                },
                {
                    "Effect": "Allow",
                    "Action": [INVOKE],
                    "Resource": { "Fn::GetAtt": [FORWARDER, "Arn"] },
                },
            ])
        },
    },
];

/// RULE, every row: the stack that declares an under-scoped pipe beside a correctly scoped one
/// reaches `CREATE_COMPLETE`, both pipes rest `RUNNING` with no failure reason, the correctly
/// scoped pipe delivers its record to the Lambda target (observed as the body the handler
/// forwarded to the result queue) and drains its source, and the under-scoped sibling moves
/// nothing at all — its record is still its own source queue's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_under_scoped_sibling_neither_fails_the_stack_nor_moves_a_record() {
    // ONE emulator for every row, so a row that observes a delivery and a row's quiet window
    // are read in the same account.
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template()),
                ("Capabilities.member.1", IAM_CAPABILITY),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} {created}"
        );

        // The under-scoped pipe is a pipe the deployment CREATES, so the stack deploys rather
        // than rolling back — which is what leaves the correctly scoped sibling standing.
        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: a pipe whose role withholds {TAKES_THE_RECORDS} on its source is \
             still created, so the stack must deploy, got {reported:?} with {described}"
        );

        let outputs = outputs_of(&described);
        let published = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack must publish {name}, got {described}")
            })
        };
        let open_source_url = published(OPEN_SOURCE_URL_OUTPUT);
        let withheld_source_url = published(WITHHELD_SOURCE_URL_OUTPUT);
        let result_url = published(RESULT_URL_OUTPUT);

        // Both pipes are live pipes resting in the state their declarations asked for: a role
        // that withholds a stage's call does not change the state the pipe reports, and
        // neither pipe carries a failure reason. Each is addressed by the name the STACK
        // publishes for it, which is the name the declaration stated or the one CloudFormation
        // generated — and a generated name is one `CreatePipe` takes.
        let pipes = [
            (OPEN_PIPE_NAME_OUTPUT, row.open_pipe()),
            (WITHHELD_PIPE_NAME_OUTPUT, row.withheld_pipe()),
        ];
        for (output, declared) in pipes {
            let name = published(output);
            if let Some(declared) = declared {
                assert_eq!(
                    name, declared,
                    "case {case:?}: the stack must publish the declared pipe name"
                );
            }
            assert!(
                name.chars().count() <= PIPE_NAME_MAX
                    && name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || ['.', '-', '_'].contains(&character)),
                "case {case:?}: the pipe name the stack publishes has to be a name a pipe may \
                 carry (at most {PIPE_NAME_MAX} of its characters), got {name:?} of {} \
                 characters",
                name.chars().count()
            );
            let (state, reason) = resting_pipe(&router, case, &name).await;
            assert_eq!(
                state, RUNNING,
                "case {case:?}: the declared pipe {name:?} must rest in {RUNNING}, got \
                 {state:?} with reason {reason:?}"
            );
            assert!(
                !reason.contains(TAKES_THE_RECORDS),
                "case {case:?}: the pipe {name:?} was created, so no withheld call may be \
                 reported as the reason it is {RUNNING}, got {reason:?}"
            );
        }

        // One record on each source, sent after both pipes are up.
        let open_marker = row.open_marker();
        let withheld_marker = row.withheld_marker();
        send(&router, case, &open_source_url, &Row::body_for(&open_marker)).await;
        send(
            &router,
            case,
            &withheld_source_url,
            &Row::body_for(&withheld_marker),
        )
        .await;

        // The correctly scoped pipe delivers: the marker read back off the result queue is the
        // one the handler read out of the record of the batch it was handed.
        let seen = markers_until(&router, case, &result_url, &open_marker).await;
        assert!(
            seen.contains(&open_marker),
            "case {case:?}: the correctly scoped pipe must hand its record to the function \
             target, which forwarded {seen:?}"
        );
        let drained = wait_for_held(&router, case, &open_source_url, 0).await;
        assert_eq!(
            drained, 0,
            "case {case:?}: the record the correctly scoped pipe moved must have left its \
             source, which still holds {drained}"
        );

        // ...and the under-scoped sibling moves nothing, over a window in which a delivery
        // that was going to happen would have.
        let later = markers_while_quiet(&router, case, &result_url).await;
        assert!(
            !seen.contains(&withheld_marker) && !later.contains(&withheld_marker),
            "case {case:?}: the sibling may not make {TAKES_THE_RECORDS} on its source, so \
             nothing of {withheld_marker:?} may reach the function target, which forwarded \
             {seen:?} then {later:?}"
        );

        // Its record is still its own source's: never taken at all.
        let kept = wait_for_held(&router, case, &withheld_source_url, 1).await;
        assert_eq!(
            kept, 1,
            "case {case:?}: the source of the under-scoped sibling must still hold its one \
             record, it holds {kept}"
        );
    }
}
