// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed `AWS::IoT::TopicRule`, end to end over the real
//! `MultiServiceRouter`: the whole emulator on one engine, one stack that provisions the queue,
//! the execution role, the forwarding function, the `AWS::Lambda::Permission` that admits the
//! invocation and the topic rule that makes it — and a message published to the deployed rule's
//! topic followed all the way to the queue the function forwards to.
//!
//! RULE: a stack resource is LIVE in the service that owns it, so a declared
//! `AWS::IoT::TopicRule` is a rule AWS IoT holds under the name the declaration gives it — a
//! matching publish runs its actions exactly as an API-created rule's do, and the
//! `AWS::Lambda::Permission` the template declares beside it is what admits the invocation, so
//! a sibling pair whose permission the template OMITS receives nothing. And a declared property
//! a later deployment CHANGES is applied to the live resource: an `UpdateStack` that changes an
//! `AWS::Lambda::Function`'s `Environment` settles the stack `UPDATE_COMPLETE` and every
//! invocation after it observes the new value, while the one before it observed the old.
//!
//! Three crates have to agree for that to hold — CloudFormation provisions the resources into
//! their owning services, Lambda holds the function's resource policy and its environment, and
//! the IoT control plane's rules engine dispatches the action a publish matched — so the suite
//! lives here: a crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the deployed pair carries its
//! `AWS::Lambda::Permission` at all, and whether the invocation is made before or after the
//! update that changes the function's environment. Each is judged where a client sees it — the
//! queue the function forwards to, and that queue's own
//! `ApproximateNumberOfMessages` for the pair that must receive nothing.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the whole estate is deployed by, and the two statuses it has to reach.
const STACK: &str = "cfn-iot-rule-lambda";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The names the template declares, so a rule's ARN — the `aws:SourceArn` the permission is
/// narrowed by — is known before the stack is deployed. A rule name carries letters, digits and
/// underscores alone (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const PERMITTED_RULE: &str = "cfn_iot_rule_permitted";
const UNPERMITTED_RULE: &str = "cfn_iot_rule_unpermitted";
const PERMITTED_TOPIC: &str = "cfn/iot/permitted";
const UNPERMITTED_TOPIC: &str = "cfn/iot/unpermitted";
const PERMITTED_QUEUE: &str = "cfn-iot-rule-permitted-sink";
const UNPERMITTED_QUEUE: &str = "cfn-iot-rule-unpermitted-sink";

/// Where Lambda binds the collection the functions are read under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime both functions carry, and how long one invocation is given: well past what a run
/// needs on any host, because what is under test is the deployed wiring and not how fast an
/// interpreter gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` a function has to report before it can be invoked.
const ACTIVE: &str = "Active";

/// The principal AWS IoT invokes a rule's function as, and the one Lambda runs a function as.
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The environment value the first deployment states and the one the update changes it to:
/// what the pre-update invocation has to report, and what every invocation after the update has
/// to.
const FIRST_VERSION: &str = "v1";
const SECOND_VERSION: &str = "v2";

/// The marker each publish carries: one before the update, one after it, and one to the pair
/// whose permission the template omits.
const BEFORE_MARKER: &str = "before-the-update";
const AFTER_MARKER: &str = "after-the-update";
const UNPERMITTED_MARKER: &str = "unpermitted";

/// How long an observation of work the CLOCK owes — a stack settling, a function going
/// `Active`, a forwarded message reaching the queue — waits before calling it a hang, and how
/// long between two looks. Generous: a row fails only when the work did not happen at all, so a
/// loaded host cannot decide the outcome. It is also BOUNDED on purpose — the first exhausted
/// wait ends the test, so an exhausted one cannot make the suite itself slow.
const DEADLINE: Duration = Duration::from_secs(90);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of a queue makes, in seconds.
const WAIT_TIME_SECONDS: u64 = 1;

/// The handler both functions run: it forwards the event it was handed to the queue its
/// environment names, beside the environment value whose propagation the update row is about.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3

SQS = boto3.client("sqs")


def handler(event, context):
    SQS.send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps({"version": os.environ["VERSION"], "event": event}),
    )
    return {"ok": True}
"#;

/// The ARN of one topic rule, which is the `aws:SourceArn` a grant for its actions is narrowed
/// by.
fn rule_arn(rule: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:rule/{rule}")
}

/// The template the stack is deployed and updated with: a queue, an execution role and a
/// forwarding function per leg, the topic rule over each leg's topic, and the
/// `AWS::Lambda::Permission` for the PERMITTED leg alone.
///
/// `version` is the value the functions' `Environment` states, which the update changes.
fn template(version: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "PermittedSink": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": PERMITTED_QUEUE },
            },
            "UnpermittedSink": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": UNPERMITTED_QUEUE },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": LAMBDA_PRINCIPAL },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // A function calls another service as its execution role, so the role
                    // GRANTS the forward its handler makes: without it the handler's own
                    // `sqs:SendMessage` is refused, which is a different rule than the
                    // permission boundary these rows read.
                    "Policies": [{
                        "PolicyName": "forward-to-the-sink",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sqs:SendMessage",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "PermittedFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Environment": { "Variables": {
                        "QUEUE_URL": { "Ref": "PermittedSink" },
                        "VERSION": version,
                    }},
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
            "UnpermittedFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Environment": { "Variables": {
                        "QUEUE_URL": { "Ref": "UnpermittedSink" },
                        "VERSION": version,
                    }},
                    "Code": { "ZipFile": FORWARDING_HANDLER },
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["PermittedFunction", "Arn"] },
                    "Principal": IOT_PRINCIPAL,
                    "SourceArn": rule_arn(PERMITTED_RULE),
                },
            },
            "PermittedRule": {
                "Type": "AWS::IoT::TopicRule",
                "Properties": {
                    "RuleName": PERMITTED_RULE,
                    "TopicRulePayload": {
                        "Sql": format!("SELECT * FROM '{PERMITTED_TOPIC}'"),
                        "RuleDisabled": false,
                        "AwsIotSqlVersion": "2016-03-23",
                        "Actions": [{ "Lambda": {
                            "FunctionArn": { "Fn::GetAtt": ["PermittedFunction", "Arn"] },
                        }}],
                    },
                },
            },
            "UnpermittedRule": {
                "Type": "AWS::IoT::TopicRule",
                "Properties": {
                    "RuleName": UNPERMITTED_RULE,
                    "TopicRulePayload": {
                        "Sql": format!("SELECT * FROM '{UNPERMITTED_TOPIC}'"),
                        "RuleDisabled": false,
                        "Actions": [{ "Lambda": {
                            "FunctionArn": { "Fn::GetAtt": ["UnpermittedFunction", "Arn"] },
                        }}],
                    },
                },
            },
        },
        "Outputs": {
            "PermittedSinkUrl": { "Value": { "Ref": "PermittedSink" } },
            "UnpermittedSinkUrl": { "Value": { "Ref": "UnpermittedSink" } },
            "PermittedFunctionName": { "Value": { "Ref": "PermittedFunction" } },
            "UnpermittedFunctionName": { "Value": { "Ref": "UnpermittedFunction" } },
        },
    })
    .to_string()
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request, and a handler's OWN calls are signed by the runtime with
    // the credential it was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One HTTP call into the emulator: the status and the body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// One awsQuery call, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, payload) = call(
        router,
        "POST",
        signing_name,
        "/",
        "application/x-www-form-urlencoded",
        &[],
        body.into_bytes(),
    )
    .await;
    (status, String::from_utf8_lossy(&payload).to_string())
}

/// One Amazon SQS call (`awsJson1_0`) that must be served, answered with its document.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(
        router,
        "POST",
        "sqs",
        "/",
        "application/x-amz-json-1.0",
        &[("x-amz-target", &format!("AmazonSQS.{operation}"))],
        body.to_string().into_bytes(),
    )
    .await;
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// One restJson1 call to a service of this emulator, answered with its status and its document.
async fn rest(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Option<Vec<u8>>,
) -> (u16, Value) {
    let (status, payload) = call(
        router,
        method,
        signing_name,
        uri,
        "application/json",
        &[],
        body.unwrap_or_default(),
    )
    .await;
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Look again until the stack `stack` first reports `wanted`, and answer the last status it
/// reported with the answer it was read from.
///
/// Waits on that CONDITION rather than on a span: the stack's settling is owed from the instant
/// the services it provisions into report their resources made, which no fixed sleep can name.
async fn wait_for_stack(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({stack}) must succeed, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == wanted || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until `function` reports `Active`, which is the state "you can't invoke or modify
/// the function" before. Waits on the CONDITION, not on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (status, configuration) = rest(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({function}) must succeed, got {status} with \
             {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// Publish one message to the IoT data plane, which is what the rules engine runs on. An
/// accepted publish is answered 200 whatever the rules did with it.
///
/// The topic travels as ONE path segment: a `{topic}` label carries no `/` of its own, so a
/// topic name's separators are escaped by this crate's one encoder of an unreserved-set
/// component ([`crate::query_wire::form_encode`]) — which is how an SDK sends one.
async fn publish(router: &MultiServiceRouter, case: &str, topic: &str, marker: &str) {
    let (status, answered) = rest(
        router,
        "iotdata",
        "POST",
        &format!("/topics/{}?qos=1", form_encode(topic)),
        Some(json!({ "marker": marker }).to_string().into_bytes()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Publish({topic}) must be accepted, got {status} with {answered}"
    );
}

/// Drain `queue_url` until the message marked `marker` has been seen, and answer the version
/// that message reported beside it.
///
/// Waits on that CONDITION rather than on a span, and reports every body it saw when the
/// message never arrives, so a failure says what the queue did hold.
async fn forwarded_version(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    marker: &str,
) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    while Instant::now() < giving_up_at {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            let parsed: Value = serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a forwarded event must be a JSON document ({error}): {body}")
            });
            if parsed["event"]["marker"] == json!(marker) {
                return parsed["version"].as_str().unwrap_or_default().to_string();
            }
            seen.push(parsed);
        }
    }
    panic!(
        "case {case:?}: the deployed rule never invoked its function for the message marked \
         {marker:?}; the queue held {seen:?}"
    );
}

/// What one queue reports it holds, which is how the pair that must receive NOTHING is judged.
async fn approximate_messages(router: &MultiServiceRouter, case: &str, queue_url: &str) -> String {
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": queue_url,
            "AttributeNames": ["ApproximateNumberOfMessages"],
        }),
    )
    .await;
    reported["Attributes"]["ApproximateNumberOfMessages"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployed_topic_rule_invokes_the_function_its_declared_permission_admits() {
    let router = emulator();

    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template(FIRST_VERSION)),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK}) must succeed, got {status} with {answered}"
    );
    let (reached, described) = wait_for_stack(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case \"a deployed IoT estate\": expected the stack to reach {CREATE_COMPLETE}, got \
         {reached:?} in {described}"
    );
    let outputs = outputs_of(&described);
    let permitted_sink = outputs
        .get("PermittedSinkUrl")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must publish its sink's URL, got {outputs:?}"));
    let unpermitted_sink = outputs
        .get("UnpermittedSinkUrl")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must publish both sinks' URLs, got {outputs:?}"));
    for key in ["PermittedFunctionName", "UnpermittedFunctionName"] {
        let function = outputs
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must publish {key}, got {outputs:?}"));
        wait_for_active(&router, &function).await;
    }

    // The rule the stack deployed is one AWS IoT really holds: a client that reads it back by
    // the name the template gave it gets the statement and the action the template declared.
    let (status, read) = rest(
        &router,
        "iot",
        "GET",
        &format!("/rules/{PERMITTED_RULE}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a deployed rule read back\": expected GetTopicRule({PERMITTED_RULE}) to resolve \
         it, got {status} with {read}"
    );
    assert_eq!(
        read["ruleArn"],
        json!(rule_arn(PERMITTED_RULE)),
        "case \"a deployed rule read back\": expected the ARN the permission names, got {read}"
    );
    assert_eq!(
        read["rule"]["sql"],
        json!(format!("SELECT * FROM '{PERMITTED_TOPIC}'")),
        "case \"a deployed rule read back\": expected the declared statement, got {read}"
    );
    assert!(
        read["rule"]["actions"][0]["lambda"]["functionArn"]
            .as_str()
            .is_some(),
        "case \"a deployed rule read back\": expected the declared lambda action, got {read}"
    );
    assert_eq!(
        read["rule"]["ruleDisabled"],
        json!(false),
        "case \"a deployed rule read back\": expected the declared enablement, got {read}"
    );

    // The leg whose permission the template OMITS is published to FIRST, so by the time the
    // permitted leg's message has been forwarded the unpermitted leg has had at least as long.
    publish(
        &router,
        "a deployed pair whose AWS::Lambda::Permission is omitted",
        UNPERMITTED_TOPIC,
        UNPERMITTED_MARKER,
    )
    .await;
    publish(
        &router,
        "a deployed pair carrying its AWS::Lambda::Permission",
        PERMITTED_TOPIC,
        BEFORE_MARKER,
    )
    .await;
    let before = forwarded_version(
        &router,
        "a deployed pair carrying its AWS::Lambda::Permission",
        &permitted_sink,
        BEFORE_MARKER,
    )
    .await;
    assert_eq!(
        before, FIRST_VERSION,
        "case \"an invocation before the update\": expected the declared environment value, got \
         {before:?}"
    );
    let held = approximate_messages(
        &router,
        "a deployed pair whose AWS::Lambda::Permission is omitted",
        &unpermitted_sink,
    )
    .await;
    assert_eq!(
        held, "0",
        "case \"a deployed pair whose AWS::Lambda::Permission is omitted\": expected its sink to \
         hold no message, got {held:?}"
    );

    // The UPDATE: the same template with a new environment value, applied to the live function.
    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "UpdateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template(SECOND_VERSION)),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an update that changes the function's Environment\": expected UpdateStack to be \
         accepted, got {status} with {answered}"
    );
    let (reached, described) = wait_for_stack(&router, STACK, UPDATE_COMPLETE).await;
    assert_eq!(
        reached, UPDATE_COMPLETE,
        "case \"an update that changes the function's Environment\": expected the stack to reach \
         {UPDATE_COMPLETE}, got {reached:?} in {described}"
    );
    publish(
        &router,
        "an invocation after the update",
        PERMITTED_TOPIC,
        AFTER_MARKER,
    )
    .await;
    let after = forwarded_version(
        &router,
        "an invocation after the update",
        &permitted_sink,
        AFTER_MARKER,
    )
    .await;
    assert_eq!(
        after, SECOND_VERSION,
        "case \"an invocation after the update\": expected the updated environment value, got \
         {after:?}"
    );
}
