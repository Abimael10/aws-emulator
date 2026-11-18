// SPDX-License-Identifier: CC0-1.0

//! A SYNCHRONOUS invocation always comes back, whatever its handler was doing — the property a
//! client branching on an invocation's answer depends on, exercised over the real
//! `MultiServiceRouter` with the whole emulator on one engine.
//!
//! RULE under test: **a `RequestResponse` `Invoke` is bounded by the invoked function's
//! configured `Timeout` — "the amount of time (in seconds) that Lambda allows a function to run
//! before stopping it" (`smithy_query(query="shape", shape="Timeout")`) — and ALWAYS answers
//! inside it: HTTP 200 carrying the handler's own value as the payload and no
//! `X-Amz-Function-Error` when the handler returned, including when a call the handler made
//! into another service was refused and the handler caught that refusal; and HTTP 200 with
//! `X-Amz-Function-Error` and the runtime's timeout error document — the sandbox timeout class,
//! and a message stating the function's CONFIGURED `Timeout` — when the handler did not return
//! inside that span. The request is never held open: a call a handler makes to another plane of
//! this emulator is spent out of what the run has left, whatever endpoint the client that made
//! it was built with, so an unanswered in-function call surfaces as the handler's own outcome
//! or as the run's own timeout rather than as an invocation that never settles.**
//!
//! Why the assembled emulator: the trajectory needs five crates to agree — AWS Lambda runs the
//! handler and bounds the run, IAM holds the execution role and the document in force on it,
//! the token service mints the session the handler's own calls are signed with, the IoT data
//! plane decides those calls, and the IoT control plane's rules engine forwards what it took to
//! Amazon SQS — and the rule is about what the LAMBDA answer is while an in-function call to
//! another service is in flight, which a single-crate suite cannot stage at all.
//!
//! The rows differ along what the rule generalises over rather than restating one case: what the
//! handler is doing when the span runs out (returning straight after an in-function call that
//! was served, returning after one the policies in force refused, returning after one made
//! through a client built at an address off this emulator, waiting far past its own `Timeout`,
//! stalling AFTER an in-function call it already made, and spinning without ever yielding), and
//! the `Timeout` the function is configured with (a generous one for the rows that must return,
//! and two short ones for the rows that must be stopped, so the span the timeout document states
//! is each function's own rather than one value every row shares).
//!
//! Each row asserts the OTHER half too, where that half is the rule's: the topic rule over the
//! topic forwards what the data plane took to a queue, so a publish the plane SERVED has to reach
//! it and one the plane REFUSED — and one no handler ever made — must never appear there. A row
//! whose publish raced its own deadline asserts the answer alone and nothing about the queue:
//! whether work the handler had already done was forwarded before the run was stopped is this
//! host's timing and no part of the rule.
//!
//! Nothing here asserts on elapsed time: each invocation is bounded by one generous deadline
//! that stands for "the request was never answered at all", which is the only way a held-open
//! request differs from a slow host.
//!
//! What this suite does NOT decide is which answer a client built at an unreachable address
//! earns. The row about one asserts that the call came BACK as the handler's own outcome, which
//! is the rule, and reads the address off the payload to show the client really was built at it —
//! so the row stands whether such a call is dispatched or refused.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where AWS Lambda binds the function operations this suite calls.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function has to report before it takes invocations.
const ACTIVE: &str = "Active";

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The topic every publish below addresses, the rule over it, and the queue that rule forwards
/// to. A rule name carries letters, digits and underscores alone (`RuleName`,
/// `pattern ^[a-zA-Z0-9_]+$`).
const TOPIC: &str = "xsvc-invoke-deadline-telemetry";
const RULE: &str = "xsvc_invoke_deadline_forward";
const QUEUE: &str = "xsvc-invoke-deadline-forwarded";

/// The role the rule's `sqs` action runs under, and the two execution roles the functions run
/// as: one holding the publish the handler makes, one holding only the endpoint lookup.
const RULE_ROLE: &str = "xsvc-invoke-deadline-rule-role";
const GRANTED_ROLE: &str = "xsvc-invoke-deadline-granted-role";
const UNGRANTED_ROLE: &str = "xsvc-invoke-deadline-ungranted-role";

/// The service principals those roles trust.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";

/// The stack that declares the functions: inline source is archived into the deployment package
/// by CloudFormation, which is how a function whose handler is written out here comes to exist.
const STACK: &str = "xsvc-invoke-deadline-functions";

/// The runtime and handler every function below is configured with.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The functions the stack declares: one per `Timeout` a row needs, under the role that row's
/// call is decided against.
const GRANTED_FUNCTION: &str = "xsvc-invoke-deadline-granted";
const UNGRANTED_FUNCTION: &str = "xsvc-invoke-deadline-ungranted";
const SHORT_FUNCTION: &str = "xsvc-invoke-deadline-short";
const LONGER_FUNCTION: &str = "xsvc-invoke-deadline-longer";

/// The `Timeout` the rows that must RETURN are given: far more than a run needs on any host,
/// because what those rows prove is the ANSWER and not how fast an interpreter gets through a
/// handler. Well inside the range the model allows the member.
const GENEROUS_TIMEOUT: u64 = 120;

/// The two `Timeout` values the rows that must be STOPPED are given: the smallest the model
/// allows (`smithy_query(query="shape", shape="Timeout")`: `@range(min: 1, max: 5400)`), and a
/// second, longer one — so the span stated in the timeout document is each function's own rather
/// than one value every row shares. The longer one belongs to the row whose handler does REAL
/// work before it stalls, which is why it is not the smallest: these are the limits those rows
/// TEST, and every other wait in this suite is generous.
const SHORT_TIMEOUT: u64 = 1;
const LONGER_TIMEOUT: u64 = 3;

/// The wait a stalling handler asks for: far past any `Timeout` a row configures, so what ends
/// such a run is the run's own span and never the span the handler named.
const STALL_SECONDS: u64 = 600;

/// The code and status the IoT data plane refuses a call the policies in force do not permit
/// with, and the signal and class an answer whose handler did not return carries.
const FORBIDDEN: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u64 = 403;
const UNHANDLED: &str = "Unhandled";
const TIMED_OUT: &str = "Sandbox.Timedout";

/// An address off this emulator, in the TLD reserved for names that resolve to nothing (RFC
/// 2606: "`.invalid` is intended for use in online construction of domain names that are sure to
/// be invalid"). A client built at it is the row about an in-function call whose endpoint names
/// no host this emulator — or anything else — answers on.
const OFF_EMULATOR_HOST: &str = "nothing-serves-this.example.invalid";

/// How long ONE invocation is waited for before the row calls it a request that was never
/// answered. Generous by orders of magnitude over every `Timeout` configured here: a loaded host
/// makes a row slower without making it fail, and only a request held open past this fails it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(180);

/// How long an observation of work the CLOCK owes — a function going `Active`, a stack
/// completing, a forwarded message reaching the queue — waits before calling it a hang, and how
/// long between two looks. Each wait is on its own CONDITION.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of the queue makes, in seconds.
const WAIT_TIME_SECONDS: u64 = 1;

/// The marker the last publish of the suite carries: once it is in the queue, every publish made
/// before it has been forwarded or was never sent, so what the queue holds can be compared
/// without waiting out a fixed span for the row that expects nothing.
const SENTINEL: &str = "sentinel";

/// How many times a row re-drives its invocation while this PROCESS's own bounds answer instead
/// of a run of the handler, and how long it lets the suites holding those bounds get through a
/// stretch of their own work between attempts. This binary runs its suites in parallel, so a
/// trivial run can be refused for what another suite holds.
const HOST_STOP_ATTEMPTS: usize = 12;
const HOST_STOP_BACKOFF: Duration = Duration::from_millis(250);

/// The handler every function runs: it resolves the data plane's address at module scope with the
/// AWS SDK its runtime supplies, and then does what the event asks — publish one message and
/// report what the plane answered, wait, stall after publishing, or spin — catching a refusal of
/// its own call so a decision the plane took is the invocation's payload rather than a failure.
///
/// The address the data-plane client is built at is the event's where it names one, so a row can
/// address a client at a host nothing answers on without the handler itself being a second one.
const HANDLER_SOURCE: &str = r#"
import json
import time

import boto3
import botocore.exceptions

REGISTRY = boto3.client("iot")
ENDPOINT = REGISTRY.describe_endpoint(endpointType="iot:Data-ATS")["endpointAddress"]


def _published(event):
    address = event.get("endpoint") or ENDPOINT
    data = boto3.client("iot-data", endpoint_url="https://" + address)
    try:
        data.publish(
            topic=event["topic"],
            qos=0,
            payload=json.dumps({"marker": event["marker"]}),
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "endpoint": address,
            "error": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except Exception as error:
        return {"ok": False, "endpoint": address, "error": type(error).__name__}
    return {"ok": True, "endpoint": address}


def handler(event, context):
    mode = event["mode"]
    if mode == "spin":
        while True:
            pass
    if mode == "wait":
        time.sleep(event["seconds"])
        return {"ok": True, "waited": event["seconds"]}
    answered = _published(event)
    if mode == "publish_then_stall":
        time.sleep(event["seconds"])
    return answered
"#;

/// What an invocation of a row has to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settles {
    /// The handler returned, and its call was SERVED: the payload reports the publish the plane
    /// took.
    Served,
    /// The handler returned, and its call was REFUSED: the payload reports the plane's own
    /// decision, which the handler caught.
    Refused,
    /// The handler returned a DECISION either way, inside the span: the call came back as the
    /// handler's own outcome, which is what the rule is about, and the payload names the address
    /// the client that made it was built at.
    Decided,
    /// The handler did not return inside the function's `Timeout`, so the answer is the
    /// runtime's timeout document stating that configured span.
    TimedOut(u64),
}

/// What the queue the topic rule forwards to has to hold once a row has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forwarded {
    /// The message the row published REACHES the queue: the plane took it, so the rule forwards
    /// it.
    Reaches,
    /// It NEVER appears there: the plane refused the publish, or the handler never made one.
    Never,
    /// Not asserted: the publish raced the row's own deadline, or the answer the row asserts
    /// leaves it open whether the call was dispatched at all, so whether the message was
    /// forwarded is this host's timing rather than the rule.
    Unasserted,
}

/// One row: what the handler is doing while the span runs out, and what the invocation must
/// answer.
struct Row {
    case: &'static str,
    /// The function invoked, which carries the `Timeout` this row is about.
    function: &'static str,
    /// The event it is invoked with.
    event: Value,
    /// What the invocation has to answer.
    settles: Settles,
    /// The marker this row's publish carries, which is what the queue is read for, and what the
    /// queue has to hold for it.
    marker: &'static str,
    forwarded: Forwarded,
}

/// Every row, in the order they run.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handler that publishes through a role holding that publish and returns",
            function: GRANTED_FUNCTION,
            event: json!({ "mode": "publish", "topic": TOPIC, "marker": "granted" }),
            settles: Settles::Served,
            marker: "granted",
            forwarded: Forwarded::Reaches,
        },
        Row {
            case: "a handler whose role holds no publish, catching the plane's refusal",
            function: UNGRANTED_FUNCTION,
            event: json!({ "mode": "publish", "topic": TOPIC, "marker": "ungranted" }),
            settles: Settles::Refused,
            marker: "ungranted",
            forwarded: Forwarded::Never,
        },
        Row {
            case: "a handler whose data-plane client is built at an address off this emulator",
            function: GRANTED_FUNCTION,
            event: json!({
                "mode": "publish",
                "topic": TOPIC,
                "marker": "off-emulator-endpoint",
                "endpoint": OFF_EMULATOR_HOST,
            }),
            settles: Settles::Decided,
            marker: "off-emulator-endpoint",
            forwarded: Forwarded::Unasserted,
        },
        Row {
            case: "a handler that waits far past the smallest Timeout the model allows",
            function: SHORT_FUNCTION,
            event: json!({ "mode": "wait", "seconds": STALL_SECONDS }),
            settles: Settles::TimedOut(SHORT_TIMEOUT),
            marker: "never-published-while-waiting",
            forwarded: Forwarded::Never,
        },
        Row {
            case: "a handler that stalls after the in-function publish it already made",
            function: LONGER_FUNCTION,
            event: json!({
                "mode": "publish_then_stall",
                "topic": TOPIC,
                "marker": "published-then-stalled",
                "seconds": STALL_SECONDS,
            }),
            settles: Settles::TimedOut(LONGER_TIMEOUT),
            marker: "published-then-stalled",
            forwarded: Forwarded::Unasserted,
        },
        Row {
            case: "a handler that spins without ever yielding",
            function: SHORT_FUNCTION,
            event: json!({ "mode": "spin" }),
            settles: Settles::TimedOut(SHORT_TIMEOUT),
            marker: "never-published-while-spinning",
            forwarded: Forwarded::Never,
        },
    ]
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
/// request. The signature itself is never checked here.
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
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, params).await;
    assert_eq!(
        status, 200,
        "case \"setup\": IAM {action} must succeed, got {status} with {body}"
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
/// both IoT planes and AWS Lambda bind their operations that way.
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

/// Create one role trusting `principal` with `document` in force on it, and answer the ARN IAM
/// minted for it.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    principal: &str,
    document: &Value,
) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy(principal)),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "grants"),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
    arn
}

/// The ARN of the topic the publishes address, which is the resource a statement about one names.
fn topic_arn() -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{TOPIC}")
}

/// What an execution role is allowed: the endpoint lookup every handler makes at module scope,
/// and — for the granted role alone — the publish to the topic's own ARN.
fn execution_policy(publishes: bool) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": "iot:DescribeEndpoint",
        "Resource": "*",
    })];
    if publishes {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "iot:Publish",
            "Resource": topic_arn(),
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The stack declaring one function per `Timeout` a row needs, each running as the role whose
/// grants that row's call is decided against.
fn function_stack(granted_role: &str, ungranted_role: &str) -> String {
    // (the logical id, the function, the role it runs as, its `Timeout`)
    let declared: [(&str, &str, &str, u64); 4] = [
        ("Granted", GRANTED_FUNCTION, granted_role, GENEROUS_TIMEOUT),
        (
            "Ungranted",
            UNGRANTED_FUNCTION,
            ungranted_role,
            GENEROUS_TIMEOUT,
        ),
        ("Short", SHORT_FUNCTION, granted_role, SHORT_TIMEOUT),
        ("Longer", LONGER_FUNCTION, granted_role, LONGER_TIMEOUT),
    ];
    let mut resources = serde_json::Map::new();
    for (logical_id, function, role, timeout) in declared {
        resources.insert(
            logical_id.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role,
                    "Timeout": timeout,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Look again until the stack reports a terminal status, and answer it with the description it
/// was read from. The completion is owed from the instant the services report their resources
/// made, which no fixed sleep names: this waits on that CONDITION.
async fn wait_for_stack(router: &MultiServiceRouter) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case \"setup\": DescribeStacks({STACK}) must be answered, got {status} with {body}"
        );
        let reported = element(&body, "StackStatus");
        if reported.ends_with("_COMPLETE") || reported.ends_with("_FAILED") {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case \"setup\": {STACK} never reached a terminal status, last {reported}"
        );
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
            "case \"setup\": GetFunctionConfiguration({function}) must be answered, got \
             {status} with {configuration}"
        );
        if configuration.get("State").and_then(Value::as_str) == Some(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case \"setup\": {function} never reported {ACTIVE}, last {last}");
}

/// What one invocation answered: the status, the `X-Amz-Function-Error` signal and the payload.
struct Answered {
    status: u16,
    signal: Option<String>,
    payload: Value,
}

impl Answered {
    /// The payload as the text it arrived as, for a message naming what a row really got.
    fn text(&self) -> String {
        self.payload.to_string()
    }
}

/// Whether an answer of `status` carrying `body` reports one of this PROCESS's own bounds instead
/// of what the run came to: the run slots an invocation waits for, whose refusal says to retry
/// the request, and the memory bound read from the whole test binary, which stops whichever run
/// is in flight once this process as a whole is over it. Neither says anything about the rule
/// under test, so such an answer is re-driven rather than read.
fn is_host_bound(status: u16, body: &str) -> bool {
    if status != 200 {
        return body.contains("invocations are running in this emulator")
            || body.contains("reading their deployment packages");
    }
    // A run the host killed for the memory this whole test binary holds, and never the run the
    // rows about the clock are waiting for: a stopped run's own document is the behaviour under
    // test, so it is read rather than re-driven.
    body.contains("signal: killed") && !body.contains(TIMED_OUT)
}

/// Invoke one function with `event` and answer what the invocation reported, re-driven while this
/// process's own bounds answer instead of a run of the handler.
///
/// The call is bounded by [`ANSWER_DEADLINE`] — the whole point of the rule is that a synchronous
/// invocation comes back, so a request still open then is this suite's failure rather than
/// something to wait out.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> Answered {
    let uri = format!("{FUNCTIONS_URI}/{function}/invocations");
    for attempt in 1..=HOST_STOP_ATTEMPTS {
        let answered = tokio::time::timeout(
            ANSWER_DEADLINE,
            call(
                router,
                "POST",
                "lambda",
                &uri,
                "application/json",
                &[],
                event.to_string().into_bytes(),
            ),
        )
        .await;
        let Ok((status, headers, payload)) = answered else {
            panic!(
                "case {case:?}: Invoke({function}) never answered at all — a RequestResponse \
                 invocation is bounded by the function's own Timeout, so the request must not \
                 be held open"
            )
        };
        let text = String::from_utf8_lossy(&payload).to_string();
        if is_host_bound(status, &text) && attempt < HOST_STOP_ATTEMPTS {
            tokio::time::sleep(HOST_STOP_BACKOFF).await;
            continue;
        }
        return Answered {
            status,
            signal: headers
                .get("x-amz-function-error")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            payload: serde_json::from_slice(&payload).unwrap_or(Value::String(text)),
        };
    }
    unreachable!("the loop above either answers or panics")
}

/// Drain the queue until the sentinel's message has been seen, and answer every marker the queue
/// held. Waits on the CONDITION — the last publish of the suite arriving — rather than on a span,
/// so a row that expects NOTHING is judged against a queue that has already taken everything that
/// was ever forwarded.
async fn drained_markers(router: &MultiServiceRouter, queue_url: &str) -> BTreeSet<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < giving_up_at {
        let received = sqs(
            router,
            "the forwarded messages",
            "ReceiveMessage",
            &json!({
                "QueueUrl": queue_url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        )
        .await;
        for message in received["Messages"].as_array().unwrap_or(&Vec::new()) {
            let body = message["Body"].as_str().unwrap_or_default();
            let document: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let marker = document["marker"].as_str().unwrap_or_else(|| {
                panic!("case \"the forwarded messages\": a forwarded message carries no marker: {body}")
            });
            seen.insert(marker.to_string());
        }
        if seen.contains(SENTINEL) {
            return seen;
        }
    }
    panic!(
        "case \"the forwarded messages\": the sentinel publish never reached the queue, saw \
         {seen:?}"
    );
}

/// Assert one invocation settled the way its row says: the handler's own payload inside the span,
/// or the runtime's timeout document stating the function's configured `Timeout`.
fn assert_settled(row: &Row, answered: &Answered) {
    let case = row.case;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: a RequestResponse invocation answers HTTP 200 whether the handler \
         returned or was stopped on its Timeout, got {} with {}",
        answered.status,
        answered.text()
    );
    match row.settles {
        Settles::Served => {
            assert_eq!(
                answered.signal, None,
                "case {case:?}: the handler returned, so no function error is reported, got {:?} \
                 with {}",
                answered.signal,
                answered.text()
            );
            assert_eq!(
                answered.payload["ok"],
                json!(true),
                "case {case:?}: expected the handler's own call to be served, got {}",
                answered.text()
            );
        }
        Settles::Refused => {
            assert_eq!(
                answered.signal, None,
                "case {case:?}: the handler caught the refusal and returned, so no function \
                 error is reported, got {:?} with {}",
                answered.signal,
                answered.text()
            );
            assert_eq!(
                answered.payload["ok"],
                json!(false),
                "case {case:?}: expected the handler to report the plane's refusal, got {}",
                answered.text()
            );
            assert_eq!(
                answered.payload["error"],
                json!(FORBIDDEN),
                "case {case:?}: expected the error code {FORBIDDEN:?}, got {}",
                answered.text()
            );
            assert_eq!(
                answered.payload["status"],
                json!(FORBIDDEN_STATUS),
                "case {case:?}: expected HTTP {FORBIDDEN_STATUS} from the data plane, got {}",
                answered.text()
            );
        }
        Settles::Decided => {
            assert_eq!(
                answered.signal, None,
                "case {case:?}: the handler returned a decision, so no function error is \
                 reported, got {:?} with {}",
                answered.signal,
                answered.text()
            );
            assert!(
                answered.payload["ok"].is_boolean(),
                "case {case:?}: expected the handler's own decision on the call it made, got {}",
                answered.text()
            );
            // The client really was built at the address the row named, so what came back is the
            // outcome of a call addressed THERE rather than of one addressed anywhere else.
            assert_eq!(
                answered.payload["endpoint"],
                json!(OFF_EMULATOR_HOST),
                "case {case:?}: expected the payload to name the address the client was built \
                 at, got {}",
                answered.text()
            );
        }
        Settles::TimedOut(timeout) => {
            assert_eq!(
                answered.signal.as_deref(),
                Some(UNHANDLED),
                "case {case:?}: a stopped run reports the {UNHANDLED:?} signal, got {:?} with {}",
                answered.signal,
                answered.text()
            );
            assert_eq!(
                answered.payload["errorType"],
                json!(TIMED_OUT),
                "case {case:?}: expected the sandbox timeout class, got {}",
                answered.text()
            );
            let message = answered.payload["errorMessage"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            // The span the message states is the function's CONFIGURED `Timeout`, and never the
            // span the handler itself asked to wait for.
            let stated = format!("Task timed out after {:.2} seconds", timeout as f64);
            assert!(
                message.ends_with(&stated),
                "case {case:?}: expected the message to state {stated:?}, got {message:?}"
            );
            // The frame AWS states a sandbox failure in: the label, the invocation's own id and
            // the separator before what happened to it.
            let leading = message[..message.len() - stated.len()].trim_end();
            let named = leading
                .strip_prefix("RequestId: ")
                .and_then(|rest| rest.strip_suffix(" Error:"))
                .unwrap_or_default();
            let widths: Vec<usize> = named.split('-').map(str::len).collect();
            assert!(
                widths == vec![8, 4, 4, 4, 12]
                    && named
                        .chars()
                        .all(|c| c == '-' || (c.is_ascii_hexdigit() && !c.is_ascii_uppercase())),
                "case {case:?}: expected the invocation's own id framed as AWS states it, got \
                 {leading:?}"
            );
        }
    }
}

/// RULE: a `RequestResponse` invocation is bounded by the invoked function's configured `Timeout`
/// and always answers inside it — the handler's own payload when it returned, including a
/// refusal of an in-function call it caught, and the runtime's timeout document stating that
/// configured span when it did not.
#[tokio::test]
async fn an_invocation_settles_inside_the_functions_configured_timeout() {
    let router = emulator();

    // The queue the rule forwards to, and the ARN a statement about that forward names.
    let created = sqs(&router, "setup", "CreateQueue", &json!({ "QueueName": QUEUE })).await;
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"setup\": CreateQueue reported no QueueUrl: {created}"))
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
        .unwrap_or_else(|| {
            panic!("case \"setup\": GetQueueAttributes reported no QueueArn: {attributes}")
        })
        .to_string();

    // The rules engine's own role, and the rule that forwards what the plane took to the queue:
    // this is what makes a SERVED publish observable where a client reads it.
    let rule_role = given_role(
        &router,
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
        "case \"setup\": CreateTopicRule({RULE}) must succeed, got {status} with {answered}"
    );

    // The two execution roles, and the functions that run as them.
    let granted_role = given_role(
        &router,
        GRANTED_ROLE,
        LAMBDA_PRINCIPAL,
        &execution_policy(true),
    )
    .await;
    let ungranted_role = given_role(
        &router,
        UNGRANTED_ROLE,
        LAMBDA_PRINCIPAL,
        &execution_policy(false),
    )
    .await;
    let (status, body) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            (
                "TemplateBody",
                &function_stack(&granted_role, &ungranted_role),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"setup\": CreateStack({STACK}) must be accepted, got {status} with {body}"
    );
    let (reached, described) = wait_for_stack(&router).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case \"setup\": the stack must reach {CREATE_COMPLETE}, got {reached} with {described}"
    );
    for function in [
        GRANTED_FUNCTION,
        UNGRANTED_FUNCTION,
        SHORT_FUNCTION,
        LONGER_FUNCTION,
    ] {
        wait_for_active(&router, function).await;
    }

    // What the queue has to hold, and what it must never hold, once every row has run.
    let mut must_reach: BTreeSet<String> = BTreeSet::new();
    let mut must_not_reach: BTreeSet<String> = BTreeSet::new();
    for row in rows() {
        let answered = invoke(&router, row.case, row.function, &row.event).await;
        assert_settled(&row, &answered);
        match row.forwarded {
            Forwarded::Reaches => {
                must_reach.insert(row.marker.to_string());
            }
            Forwarded::Never => {
                must_not_reach.insert(row.marker.to_string());
            }
            Forwarded::Unasserted => {}
        }
    }

    // The last publish of the suite, through the role that may make it: once its message is in
    // the queue, everything that was ever forwarded is there to be read, so a row that expects
    // NOTHING is judged against a queue that has already taken it all. It is itself a served
    // publish, so it is one of the messages the queue has to hold.
    let sentinel = invoke(
        &router,
        "the sentinel publish",
        GRANTED_FUNCTION,
        &json!({ "mode": "publish", "topic": TOPIC, "marker": SENTINEL }),
    )
    .await;
    assert_eq!(
        sentinel.signal, None,
        "case \"the sentinel publish\": expected no function error, got {:?} with {}",
        sentinel.signal,
        sentinel.text()
    );
    must_reach.insert(SENTINEL.to_string());

    let forwarded = drained_markers(&router, &queue_url).await;
    for marker in &must_reach {
        assert!(
            forwarded.contains(marker),
            "case \"the forwarded messages\": expected the message the data plane served for \
             {marker:?} to reach the queue, got {forwarded:?}"
        );
    }
    for marker in &must_not_reach {
        assert!(
            !forwarded.contains(marker),
            "case \"the forwarded messages\": expected nothing to be forwarded for {marker:?}, \
             got {forwarded:?}"
        );
    }
}

