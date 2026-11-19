// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-declared `AWS::Pipes::Pipe` with a LAMBDA target, driven against the
//! emulator a client STARTS: the built binary, on its own port, with request authentication
//! on, reached over TLS with SigV4-signed calls.
//!
//! RULE: **a stack that declares a queue, a function and a pipe between them leaves a LIVE
//! pipe — once the stack reports it created and EventBridge Pipes reports it resting in the
//! state its `DesiredState` asked for, the pipe polls the source with no client action at all,
//! admits exactly the records its declared `SourceParameters.FilterCriteria` match, invokes
//! the declared function target with them, and consumes every record it polled — and that
//! holds on the emulator as it is STARTED, not only on a router a test assembles in its own
//! process.**
//!
//! [`crate::cloudformation_deployed_pipe_filter_lambda_target`] spans what the rule
//! generalises over (the clause forms a declared pattern takes, how the declaration spells it,
//! the share of the records admitted, the batch size, and a deployment made through a change
//! set) on a router built inside the test process. The emulator a client runs is a different
//! configuration of the same code: the credentials are verified, each call arrives over its
//! own TLS connection, and the pipe's own polling, its target invocation and the handler's own
//! SDK call are all made on the binary's own runtime inside one process. This suite is the one
//! that pins the chain THERE, so a declared pipe that moves records in process and not on the
//! started emulator is a failure a test reports rather than one only a client sees. ONE
//! deployment is driven here, because starting the binary costs what starting the emulator
//! costs.
//!
//! Nothing here invokes the function: the only data-plane calls are the two `SendMessage`s
//! that put the records on the source queue and the `ReceiveMessage`s that read the result
//! queue. Every body read back off that queue was sent by the handler's own `boto3` call out
//! of the batch the pipe handed it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The terminal `StackStatus` a deployment that succeeded settles to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a template whose IAM resources this deployment creates has to be made with
/// (`CreateStackInput$Capabilities`).
const IAM_CAPABILITY: &str = "CAPABILITY_IAM";

/// The `PipeState` a pipe whose `DesiredState` is `RUNNING` comes to rest in.
const RUNNING: &str = "RUNNING";

/// How long the deployment, the pipe's own settling and the delivery are given before this
/// suite fails. Every step is owed by the CLOCK, so each wait is on the CONDITION — looked for
/// again until it holds — and this bound is generous slack whose only job is that the wait
/// TERMINATES. Nothing below asserts on elapsed time.
const SETTLE: Duration = Duration::from_secs(120);

/// How long the result queue is watched for the record the filter DROPPED, after the admitted
/// one has arrived. A pipe that was going to deliver it would have: it is many times the
/// poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often the stack, the pipe or a queue is looked at again while waiting.
const LOOK: Duration = Duration::from_millis(200);

/// The prefix every name this suite's template declares carries.
const PREFIX: &str = "started-pipe-lambda";

/// The pattern the declared `FilterCriteria` carries: a clause on a field of the record's JSON
/// body, which is the form a pipe filtering SQS records by their payload is written in.
const PATTERN: &str = r#"{"body":{"marker":["keep"]}}"#;

/// The record the pattern ADMITS and the one it drops, as the bodies they are sent as.
const ADMITTED_BODY: &str = r#"{"marker":"keep","leg":"started"}"#;
const DROPPED_BODY: &str = r#"{"marker":"drop","leg":"started"}"#;

/// The handler the stack declares inline: it forwards the body of every record of the batch it
/// was handed to the result queue the declaration named it through its environment, and
/// refuses a payload that is not the JSON array a pipe hands a function target.
const FORWARDING_HANDLER: &str = r#"import os

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

/// The one template this suite deploys: the source and result queues, a role for the forwarder
/// and a role for the pipe, the forwarder itself, and the pipe wiring the source to it under
/// the declared filter.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "PipeSource": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": format!("{PREFIX}-source") },
            },
            "PipeResult": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": format!("{PREFIX}-result") },
            },
            "ForwarderRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust("lambda.amazonaws.com"),
                    "Policies": [{
                        "PolicyName": "forward-each-record",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["sqs:SendMessage"],
                                "Resource": { "Fn::GetAtt": ["PipeResult", "Arn"] },
                            }],
                        },
                    }],
                },
            },
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{PREFIX}-forwarder"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 60,
                    "Role": { "Fn::GetAtt": ["ForwarderRole", "Arn"] },
                    "Environment": {
                        "Variables": { "RESULT_QUEUE": { "Ref": "PipeResult" } },
                    },
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
            "PipeRole": {
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
                                    // The calls an Amazon SQS source's stage makes on the
                                    // source, as AWS documents them for a pipe source.
                                    "Action": [
                                        "sqs:ReceiveMessage",
                                        "sqs:DeleteMessage",
                                        "sqs:GetQueueAttributes",
                                    ],
                                    "Resource": { "Fn::GetAtt": ["PipeSource", "Arn"] },
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": ["lambda:InvokeFunction"],
                                    "Resource": { "Fn::GetAtt": ["Forwarder", "Arn"] },
                                },
                            ],
                        },
                    }],
                },
            },
            "ThePipe": {
                "Type": "AWS::Pipes::Pipe",
                "Properties": {
                    "Name": format!("{PREFIX}-pipe"),
                    "Source": { "Fn::GetAtt": ["PipeSource", "Arn"] },
                    "SourceParameters": {
                        "SqsQueueParameters": { "BatchSize": 1 },
                        "FilterCriteria": { "Filters": [{ "Pattern": PATTERN }] },
                    },
                    "Target": { "Fn::GetAtt": ["Forwarder", "Arn"] },
                    "TargetParameters": {
                        "LambdaFunctionParameters": { "InvocationType": "REQUEST_RESPONSE" },
                    },
                    "RoleArn": { "Fn::GetAtt": ["PipeRole", "Arn"] },
                    "DesiredState": RUNNING,
                },
            },
        },
        "Outputs": {
            "SourceUrl": { "Value": { "Ref": "PipeSource" } },
            "ResultUrl": { "Value": { "Ref": "PipeResult" } },
            "PipeName": { "Value": { "Ref": "ThePipe" } },
        },
    })
    .to_string()
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

/// One CloudFormation call over its awsQuery wire, signed for the started emulator.
fn cloudformation(emulator: &Started, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    emulator.call(
        "cloudformation",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
}

/// One Amazon SQS call (`awsJson1_0`), as its status and parsed body.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> (u16, Value) {
    let (status, text) = emulator.call(
        "sqs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
        ],
        body.to_string().into_bytes(),
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The bodies waiting on the queue at `url` right now, left there: `VisibilityTimeout: 0`
/// takes no checkout, so a later look sees them again.
fn received(emulator: &Started, url: &str) -> Vec<String> {
    let (status, answered) = sqs(
        emulator,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 0 }),
    );
    assert_eq!(
        status, 200,
        "ReceiveMessage({url:?}) must answer 200, got {status} with {answered}"
    );
    answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// How many records the queue at `url` still HOLDS: the ones waiting on it plus the ones a
/// consumer has taken and not deleted.
fn held(emulator: &Started, url: &str) -> i64 {
    let attributes = [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ];
    let (status, answered) = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": attributes }),
    );
    assert_eq!(
        status, 200,
        "GetQueueAttributes({url:?}) must answer 200, got {status} with {answered}"
    );
    attributes
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

/// RULE: the stack deploys, EventBridge Pipes reports the declared pipe `RUNNING`, the record
/// the declared filter admits reaches the function target, the one it drops never does, and the
/// source is drained of both — on the emulator a client starts.
#[test]
fn a_declared_pipe_moves_its_records_on_the_emulator_a_client_starts() {
    let emulator = Started::start();
    let stack = format!("{PREFIX}-stack");

    let (status, created) = cloudformation(
        &emulator,
        "CreateStack",
        &[
            ("StackName", &stack),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", IAM_CAPABILITY),
        ],
    );
    assert_eq!(
        status, 200,
        "CreateStack must answer 200, got {status} with {created}"
    );

    // The stack as it reads once its operation has reached a TERMINAL status, polled rather
    // than slept for: how many spans the function's and the pipe's own creations take is the
    // deployment's to decide and not this suite's to assume.
    let giving_up_at = Instant::now() + SETTLE;
    let (reported, described) = loop {
        let (status, body) = cloudformation(&emulator, "DescribeStacks", &[("StackName", &stack)]);
        assert_eq!(
            status, 200,
            "DescribeStacks must answer 200, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            break (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "the stack {stack:?} must reach a terminal status, last {reported:?}; the \
             emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK);
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "the stack must deploy the queues, the roles, the function and the pipe, got \
         {reported:?} with {described}; the emulator's log:\n{}",
        emulator.log()
    );

    // The identifiers this suite drives the deployment by are the ones the STACK publishes.
    let outputs = outputs_of(&described);
    let published = |name: &str| {
        outputs
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("the stack must publish {name}, got {described}"))
    };
    let source_url = published("SourceUrl");
    let result_url = published("ResultUrl");
    let pipe_name = published("PipeName");

    // The pipe the stack says it created is the live pipe EventBridge Pipes holds, resting in
    // the state the declaration asked for.
    let giving_up_at = Instant::now() + SETTLE;
    let pipe = loop {
        let (status, text) = emulator.call(
            "pipes",
            "GET",
            &format!("/v1/pipes/{pipe_name}"),
            &[("content-type", "application/json")],
            Vec::new(),
        );
        assert_eq!(
            status, 200,
            "DescribePipe({pipe_name:?}) must answer 200, got {status} with {text}"
        );
        let pipe: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if pipe["CurrentState"].as_str() == Some(RUNNING) || Instant::now() >= giving_up_at {
            break pipe;
        }
        std::thread::sleep(LOOK);
    };
    assert_eq!(
        pipe["CurrentState"].as_str(),
        Some(RUNNING),
        "the declared pipe must rest in {RUNNING}, got {pipe}; the emulator's log:\n{}",
        emulator.log()
    );

    for body in [ADMITTED_BODY, DROPPED_BODY] {
        let (status, answered) = sqs(
            &emulator,
            "SendMessage",
            &json!({ "QueueUrl": source_url, "MessageBody": body }),
        );
        assert_eq!(
            status, 200,
            "SendMessage({body:?}) must answer 200, got {status} with {answered}"
        );
    }

    // The admitted record reaches the function target: the body read back here is the one the
    // handler forwarded out of the batch the pipe handed it.
    let giving_up_at = Instant::now() + SETTLE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(received(&emulator, &result_url));
        if seen.iter().any(|body| body == ADMITTED_BODY) || Instant::now() >= giving_up_at {
            break;
        }
        std::thread::sleep(LOOK);
    }
    assert!(
        seen.iter().any(|body| body == ADMITTED_BODY),
        "case {PATTERN:?}: the record {ADMITTED_BODY:?} is admitted, so the declared pipe must \
         hand it to its function target, which forwarded {seen:?}; the emulator's log:\n{}",
        emulator.log()
    );

    // ...and the record the filter drops never does, over a window in which a delivery that
    // was going to happen would have.
    let watching_until = Instant::now() + QUIET;
    while Instant::now() < watching_until {
        seen.extend(received(&emulator, &result_url));
        std::thread::sleep(LOOK);
    }
    assert!(
        !seen.iter().any(|body| body == DROPPED_BODY),
        "case {PATTERN:?}: the record {DROPPED_BODY:?} is dropped, so nothing of it may reach \
         the function target, which forwarded {seen:?}"
    );

    // Every record the pipe polled — the admitted one and the dropped one — was consumed from
    // the source.
    let giving_up_at = Instant::now() + SETTLE;
    let mut still_held = held(&emulator, &source_url);
    while still_held != 0 && Instant::now() < giving_up_at {
        std::thread::sleep(LOOK);
        still_held = held(&emulator, &source_url);
    }
    assert_eq!(
        still_held, 0,
        "the source must be drained of every record the pipe polled, it still holds \
         {still_held}; the emulator's log:\n{}",
        emulator.log()
    );
}
