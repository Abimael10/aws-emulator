// SPDX-License-Identifier: CC0-1.0

//! The IAM boundary around an in-function iot-data `Publish`, taken CALL BY CALL: one running
//! handler, one topic, one topic rule forwarding to one queue, and the policies in force on the
//! calling function's execution role rewritten between invocations.
//!
//! RULE: the authorization of a data-plane call is decided afresh for every request, from the
//! identity-based policies the IAM store holds for the calling principal at that moment — so a
//! grant written after the function was deployed takes effect on its very next call with no
//! redeploy, taking it away refuses the next one again, an explicit `Deny` refuses whatever an
//! `Allow` beside it says, and no decision is carried over from an earlier call or borrowed from
//! another identity's. A call the policies in force allow is served (HTTP 200) and its message is
//! what the matched rule's `sqs` action forwards, exactly once; one they do not allow is refused
//! `ForbiddenException` (HTTP 403) in this plane's own envelope — a NULL message — and performs
//! nothing at all, so no rule runs for it and nothing of it reaches the queue.
//!
//! Five crates have to agree for that to hold — Lambda runs the handler and supplies it the SDK,
//! IAM holds each execution role and the inline document in force on it, STS mints the session
//! the handler's calls are signed with, the IoT data plane takes the decision and the IoT control
//! plane's rules engine forwards what it took to Amazon SQS — so the suite lives here: a
//! crate-local suite can only ever see one side of it.
//!
//! What this suite is about that its neighbours are not: WHEN the decision is taken, not what it
//! decides. The SHAPES an identity policy can take — the exact ARN, a wildcard over it, another
//! action, no document at all, a `Deny` beside a broad `Allow`, a permissions boundary — are
//! already spanned, for this same gate, by `iot_publish_policy_shape_decides_its_rule_dispatch.rs`,
//! and the handler's path into the plane by `iot_data_plane_from_a_lambda_handler.rs`. Neither of
//! them moves a document while the calling functions are deployed and running, publishes twice
//! through one identity, or makes one identity's call while another's grants are changing. This
//! file does exactly those three things, over one topic and one rule, so a gate that cached a
//! verdict, snapshotted a role's grants when its session was minted, or let one caller's refusal
//! decide another's call fails here. The three documents below are the BEFORE and AFTER states of
//! those rewrites, not a catalog of policy shapes.
//!
//! The rows differ along what the rule generalises over: the ORDER a call is made in relative to
//! the change in force (before the grant is written, straight after it, after it is withdrawn, and
//! after it is overridden by an explicit `Deny`), WHICH caller makes it (the role whose document is
//! being rewritten, and a sibling role nothing touches), the COUNT of calls one identity makes
//! under one grant, and the QoS the publish asks for — both ends of the modelled range
//! (`PublishRequest$qos`, "range 0..1"). Every call carries its own marker, so no row can pass by
//! reading another's, and the queue is read once at the end against the markers of exactly the
//! calls that had to be served, counting each.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The one topic every call of this suite publishes to, the rule over it, and the queue that
/// rule's `sqs` action forwards to. A rule name carries letters, digits and underscores alone
/// (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const TOPIC: &str = "xsvc-iot-grant-telemetry";
const RULE: &str = "xsvc_iot_grant_telemetry";
const QUEUE: &str = "xsvc-iot-grant-forwarded";

/// The role the rules engine performs its `sqs` action under, and the service principal that role
/// has to trust.
const RULE_ROLE: &str = "xsvc-iot-grant-rule-role";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";

/// The two execution roles: the one whose inline document the steps rewrite, and the sibling
/// nothing ever touches.
const MUTABLE_ROLE: &str = "xsvc-iot-grant-mutable-role";
const UNTOUCHED_ROLE: &str = "xsvc-iot-grant-untouched-role";

/// The function running as each of those roles, both from the same handler source.
const MUTABLE_FUNCTION: &str = "xsvc-iot-grant-mutable";
const UNTOUCHED_FUNCTION: &str = "xsvc-iot-grant-untouched";

/// The ONE inline policy name every document below is written under, so each `PutRolePolicy`
/// REPLACES what the previous step put in force rather than adding beside it.
const POLICY_NAME: &str = "grants";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The stack the functions are deployed by, and the `StackStatus` it has to reach.
const STACK: &str = "xsvc-iot-grant-callers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where Lambda binds the collection the functions are read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime both functions carry, and how long one invocation is given: well past what a run
/// needs on any host, because what is under test is the decision and not how fast an interpreter
/// gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` a function has to report before it is invoked.
const ACTIVE: &str = "Active";

/// The code and status the IoT data plane refuses a call the policies in force do not permit
/// with.
const FORBIDDEN: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u16 = 403;

/// How long an observation of work the CLOCK owes — a function going `Active`, a stack
/// completing, a forwarded message reaching the queue — waits before calling it a hang, and how
/// long between two looks. Generous — the suite needs seconds of it when nothing is wrong, so a
/// loaded host cannot decide the outcome — and bounded, so a path that is actually broken is cheap
/// to diagnose rather than a minutes-long stall.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of the queue makes, in seconds.
const WAIT_TIME_SECONDS: u64 = 1;

/// The marker the LAST publish of the suite carries, made through the caller that may always make
/// it: once it is in the queue, every publish made before it has been forwarded or was never
/// sent, so what the queue holds can be compared against what the steps expected without waiting
/// out a fixed time for the steps that expect nothing.
const SENTINEL: &str = "sentinel";

/// The handler both functions run: `import boto3` at MODULE scope, the registry client and the
/// data-plane address resolved there too — so a runtime shipping no SDK fails initialization
/// rather than this call — and then the ONE publish the event describes, reporting what the data
/// plane answered. The invocation's payload IS what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

REGISTRY = boto3.client("iot")
ENDPOINT = REGISTRY.describe_endpoint(endpointType="iot:Data-ATS")["endpointAddress"]
DATA = boto3.client("iot-data", endpoint_url="https://" + ENDPOINT)


def handler(event, context):
    try:
        DATA.publish(
            topic=event["topic"],
            qos=event["qos"],
            payload=json.dumps({"marker": event["marker"]}),
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"ok": True, "endpoint": ENDPOINT}
"#;

/// One document a step puts in force on the mutable caller's role.
///
/// Every one of them carries `iot:DescribeEndpoint`, because the handler resolves the data
/// plane's address at module scope: what the rows vary is the PUBLISH, not the discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InForce {
    /// The endpoint lookup alone: nothing of the data plane at all.
    EndpointOnly,
    /// The endpoint lookup, and the publish scoped to the ARN of the topic published to.
    PublishOnTheTopic,
    /// Every action of the service, with an explicit `Deny` of the publish over it.
    EveryActionDenyingThePublish,
}

impl InForce {
    /// The identity-based document this state writes.
    fn document(self) -> Value {
        let discovery = json!({
            "Effect": "Allow",
            "Action": "iot:DescribeEndpoint",
            "Resource": "*",
        });
        let statements = match self {
            Self::EndpointOnly => vec![discovery],
            Self::PublishOnTheTopic => vec![
                discovery,
                json!({
                    "Effect": "Allow",
                    "Action": "iot:Publish",
                    "Resource": topic_arn(TOPIC),
                }),
            ],
            Self::EveryActionDenyingThePublish => vec![
                json!({
                    "Effect": "Allow",
                    "Action": "iot:*",
                    "Resource": "*",
                }),
                json!({
                    "Effect": "Deny",
                    "Action": "iot:Publish",
                    "Resource": topic_arn(TOPIC),
                }),
            ],
        };
        json!({ "Version": "2012-10-17", "Statement": statements })
    }
}

/// One step of the sequence: what is put in force before it, who publishes, how, and what has to
/// come of it.
struct Step {
    case: &'static str,
    /// The document to put in force on the mutable role before this step runs, or `None` to leave
    /// the policies exactly as the previous step left them.
    put: Option<InForce>,
    /// `true` for the caller whose role's document the steps rewrite, `false` for the sibling
    /// nothing touches.
    mutable: bool,
    /// The QoS the publish asks for: the steps span both ends of the modelled range.
    qos: i64,
    /// This call's own marker, which is what the queue is read for.
    marker: &'static str,
    /// `true` for a call the policies in force have to serve, `false` for one they have to refuse.
    served: bool,
}

/// The sequence, in the order it runs: every step but the first stands on what the earlier ones
/// put in force, which is what makes this about the decision being taken PER CALL.
fn steps() -> Vec<Step> {
    vec![
        Step {
            case: "a publish whose role grants nothing of the data plane yet",
            put: Some(InForce::EndpointOnly),
            mutable: true,
            qos: 0,
            marker: "ungranted-first",
            served: false,
        },
        Step {
            case: "the untouched caller publishing after that refusal",
            put: None,
            mutable: false,
            qos: 0,
            marker: "untouched-after-a-refusal",
            served: true,
        },
        Step {
            case: "a publish under a grant written after the function was deployed",
            put: Some(InForce::PublishOnTheTopic),
            mutable: true,
            qos: 1,
            marker: "granted-once-written",
            served: true,
        },
        Step {
            case: "the same caller publishing again under the same grant",
            put: None,
            mutable: true,
            qos: 0,
            marker: "granted-again",
            served: true,
        },
        Step {
            case: "a publish an explicit deny covers over an allow of every action",
            put: Some(InForce::EveryActionDenyingThePublish),
            mutable: true,
            qos: 1,
            marker: "denied-explicitly",
            served: false,
        },
        Step {
            case: "a publish whose grant has been taken away again",
            put: Some(InForce::EndpointOnly),
            mutable: true,
            qos: 0,
            marker: "ungranted-again",
            served: false,
        },
        Step {
            case: "the untouched caller publishing after every change to the other role",
            put: None,
            mutable: false,
            qos: 1,
            marker: "untouched-at-the-end",
            served: true,
        },
    ]
}

/// The ARN of one topic, which is the resource a statement about a publish to it names.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}")
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential scope
    // is what routes a request, and the handler's OWN calls are signed by the runtime with the
    // credential it was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One HTTP call into the emulator: the status, the headers and the body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, http::HeaderMap, Vec<u8>) {
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
    let (parts, payload) = response.into_parts();
    (status, parts.headers, payload.to_vec())
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
    let (status, _headers, payload) = call(
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

/// One IAM call that must be served, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Amazon SQS call (`awsJson1_0`) that must be served, answered with its document.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, _headers, payload) = call(
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

/// One restJson1 call to a service of this emulator, answered with its status and its document:
/// the IoT control plane and Lambda both bind their operations that way.
async fn rest(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Option<Vec<u8>>,
) -> (u16, Value) {
    let (status, _headers, payload) = call(
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

/// A trust policy admitting one service principal.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Put `document` in force on `role` as its ONE inline policy, replacing whatever was there.
async fn put_policy(router: &MultiServiceRouter, case: &str, role: &str, document: &Value) {
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// Create one role trusting `principal` with `document` in force on it, and answer the ARN IAM
/// minted for it.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
    document: &Value,
) -> String {
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy(principal)),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    put_policy(router, case, name, document).await;
    arn
}

/// The stack declaring both functions, each running as its own role with the one handler source:
/// the inline source is archived into the deployment package by CloudFormation, which is how a
/// function whose handler is written here comes to exist at all.
fn function_stack(mutable_role_arn: &str, untouched_role_arn: &str) -> String {
    let function = |name: &str, role_arn: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": RUNTIME,
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": HANDLER_SOURCE },
            },
        })
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "MutableCaller": function(MUTABLE_FUNCTION, mutable_role_arn),
            "UntouchedCaller": function(UNTOUCHED_FUNCTION, untouched_role_arn),
        },
    })
    .to_string()
}

/// Look again until the stack `stack` first reports [`CREATE_COMPLETE`], and answer the last
/// status it reported with the answer that status was read from. Waits on the CONDITION, not on a
/// span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
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
        if reported == CREATE_COMPLETE || Instant::now() >= giving_up_at {
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

/// Invoke one function with `event` and answer what the invocation reported: the
/// `X-Amz-Function-Error` header and the payload document.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let (status, headers, payload) = call(
        router,
        "POST",
        "lambda",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        &[],
        event.to_string().into_bytes(),
    )
    .await;
    let document: Value = serde_json::from_slice(&payload)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()));
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({function}), got {status} with {document}"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (signal, document)
}

/// Drain the queue until the sentinel's message has been seen, and answer how many messages each
/// marker arrived in.
///
/// Every message read is DELETED, so a marker's count is how many times the rule forwarded it and
/// never a redelivery of one message. Waits on the CONDITION — the last publish of the suite
/// arriving — rather than on a span, so a step that expects NOTHING is judged against a queue that
/// has already taken everything that was ever sent.
async fn forwarded_counts(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
) -> BTreeMap<String, usize> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    while Instant::now() < giving_up_at && !counts.contains_key(SENTINEL) {
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
            *counts.entry(forwarded_marker(case, body)).or_default() += 1;
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": queue_url, "ReceiptHandle": handle }),
                )
                .await;
            }
        }
    }
    assert!(
        counts.contains_key(SENTINEL),
        "case {case:?}: the rule never forwarded the last publish of the suite, so what the queue \
         holds says nothing; it held {counts:?}"
    );
    counts
}

/// The marker the queue message `body` carries: the selected message's own document, which is the
/// form this suite's one `sqs` action asks for (`useBase64` false).
fn forwarded_marker(case: &str, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(|document| document.get("marker"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: a forwarded message carries no marker: {body}"))
}

/// RULE: the authorization of an in-function data-plane publish is decided per call from the
/// policies in force at that moment, and the message of a served publish — and of no refused one
/// — is what the matched rule's `sqs` action forwards, once each.
#[tokio::test]
async fn an_in_function_publish_is_judged_against_the_policies_in_force_at_each_call() {
    let router = emulator();

    // The queue the rule forwards to, and the ARN a statement about that forward names.
    let created = sqs(&router, "setup", "CreateQueue", &json!({ "QueueName": QUEUE })).await;
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateQueue reported no QueueUrl: {created}"))
        .to_string();
    let attributes = sqs(
        &router,
        "setup",
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetQueueAttributes reported no QueueArn: {attributes}"))
        .to_string();

    // The rules engine's own role, which may make exactly the send its action makes.
    let rule_role = given_role(
        &router,
        "setup",
        RULE_ROLE,
        IOT_PRINCIPAL,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "sqs:SendMessage",
                "Resource": queue_arn,
            }],
        }),
    )
    .await;

    // The one rule: every message published to the topic is forwarded to the queue as the
    // selected document.
    let (status, answered) = rest(
        &router,
        "iot",
        "POST",
        &format!("/rules/{RULE}"),
        Some(
            json!({
                "sql": format!("SELECT * FROM '{TOPIC}'"),
                "awsIotSqlVersion": "2016-03-23",
                "ruleDisabled": false,
                "actions": [{
                    "sqs": {
                        "roleArn": rule_role,
                        "queueUrl": queue_url,
                        "useBase64": false,
                    },
                }],
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTopicRule({RULE}) must succeed, got {status} with {answered}"
    );

    // The two execution roles: the mutable one starts with whatever the first step puts in force,
    // and the untouched one may publish to the topic for the whole run.
    let mutable_role = given_role(
        &router,
        "setup",
        MUTABLE_ROLE,
        LAMBDA_PRINCIPAL,
        &InForce::EndpointOnly.document(),
    )
    .await;
    let untouched_role = given_role(
        &router,
        "setup",
        UNTOUCHED_ROLE,
        LAMBDA_PRINCIPAL,
        &InForce::PublishOnTheTopic.document(),
    )
    .await;

    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&mutable_role, &untouched_role)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK}) must be accepted, got {status} with {answered}"
    );
    let (reached, reported) = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached} with {reported}"
    );
    for function in [MUTABLE_FUNCTION, UNTOUCHED_FUNCTION] {
        wait_for_active(&router, function).await;
    }

    // What the queue has to hold afterwards: one message per publish the plane had to serve.
    let mut expected: BTreeMap<String, usize> = BTreeMap::new();
    for step in steps() {
        let case = step.case;
        if let Some(put) = step.put {
            put_policy(&router, case, MUTABLE_ROLE, &put.document()).await;
        }
        let function = if step.mutable {
            MUTABLE_FUNCTION
        } else {
            UNTOUCHED_FUNCTION
        };
        let (signal, payload) = invoke(
            &router,
            case,
            function,
            &json!({ "topic": TOPIC, "qos": step.qos, "marker": step.marker }),
        )
        .await;
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
        );
        if step.served {
            assert_eq!(
                payload["ok"],
                json!(true),
                "case {case:?}: expected the handler's publish to be served, got {payload}"
            );
            *expected.entry(step.marker.to_string()).or_default() += 1;
        } else {
            assert_eq!(
                payload["ok"],
                json!(false),
                "case {case:?}: expected the publish to be refused, got {payload}"
            );
            assert_eq!(
                payload["code"],
                json!(FORBIDDEN),
                "case {case:?}: expected the error code {FORBIDDEN:?}, got {payload}"
            );
            assert_eq!(
                payload["status"],
                json!(u64::from(FORBIDDEN_STATUS)),
                "case {case:?}: expected HTTP {FORBIDDEN_STATUS} from the data plane, got {payload}"
            );
            let carried = payload.as_object().and_then(|report| report.get("message"));
            assert_eq!(
                carried,
                Some(&Value::Null),
                "case {case:?}: expected the refusal to carry a null message, got {payload}"
            );
        }
    }

    // The last publish of the suite, through the caller that may always make it: once its message
    // is in the queue, everything that was ever forwarded is there to compare against.
    let (signal, payload) = invoke(
        &router,
        "sentinel",
        UNTOUCHED_FUNCTION,
        &json!({ "topic": TOPIC, "qos": 0, "marker": SENTINEL }),
    )
    .await;
    assert_eq!(
        signal, None,
        "case \"sentinel\": expected no FunctionError, got {signal:?} with {payload}"
    );
    assert_eq!(
        payload["ok"],
        json!(true),
        "case \"sentinel\": expected the last publish of the suite to be served, got {payload}"
    );
    *expected.entry(SENTINEL.to_string()).or_default() += 1;

    let forwarded = forwarded_counts(&router, "the forwarded messages", &queue_url).await;
    assert_eq!(
        forwarded, expected,
        "case \"the forwarded messages\": expected the queue to hold exactly one message per \
         publish the data plane served, got {forwarded:?}"
    );
}
