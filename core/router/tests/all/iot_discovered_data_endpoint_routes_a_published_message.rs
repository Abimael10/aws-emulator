// SPDX-License-Identifier: CC0-1.0

//! The IoT ingestion shape a client actually writes: compute DISCOVERS where the device data
//! plane answers, publishes telemetry there, and a topic rule fans the message out to a queue —
//! driven over the real `MultiServiceRouter`, with the whole emulator on one engine and a
//! running `python3.x` function making the two calls.
//!
//! RULE: the address `iot:DescribeEndpoint` announces for an account is the address that
//! account's IoT planes are SERVED at, so a client built on it from inside the emulator reaches
//! the data plane and its `Publish` is answered HTTP 200; and a message the broker took is
//! evaluated by the rules engine, which performs the actions of every ENABLED topic rule whose
//! `FROM` topic filter COVERS the published topic — and of no other rule, so a topic whose only
//! rule is disabled and a topic no filter covers forward nothing.
//!
//! The rows differ along what the rule generalises over: WHICH endpoint type the handler
//! discovers the data plane with (`iot:Data-ATS`, `iot:Data`, and naming none at all, which asks
//! for the default), the `qos` the publish carries (`0` and `1`, the two values the modelled
//! `Qos` range admits), the FORM of the covering rule's topic filter (the topic's own name, a
//! single-level `+` wildcard, a multi-level `#` wildcard), and whether any enabled rule covers
//! the topic at all (one whose only rule is disabled, and one no rule's filter covers). Each row
//! asserts both halves — what the handler's own call answered, which is what it returns as the
//! invocation's payload, and what the TARGET holds afterwards — so a publish that answered
//! without being routed, or was routed without being answered, fails here.
//!
//! Five crates have to agree for that to hold — Lambda runs the handler and supplies it the SDK,
//! IAM holds the execution role and the rule action's role, the IoT control plane answers the
//! endpoint discovery and holds the rules, the IoT data plane takes the publish, and Amazon SQS
//! holds what the rule forwarded — so the suite lives here: a crate-local suite can only ever
//! see one side of it.
//!
//! What this suite is about that its neighbour `iot_data_plane_from_a_lambda_handler.rs` is not:
//! that one varies the grant in force on the calling role and the action the rule performs; this
//! one varies the DISCOVERY the handler starts from and which stored rule the published topic
//! selects.

use std::collections::BTreeSet;
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

/// The topics the rows publish to: one a rule names outright, one a single-level wildcard
/// covers, one a multi-level wildcard covers, one whose only rule is disabled, and one no
/// rule's filter covers.
const EXACT_TOPIC: &str = "xsvcdisc/exact";
const SINGLE_TOPIC: &str = "xsvcdisc/sensor-a/single";
const DEEP_TOPIC: &str = "xsvcdisc/deep/room/1";
const DISABLED_TOPIC: &str = "xsvcdisc/disabled";
const UNWATCHED_TOPIC: &str = "xsvcdisc/unwatched";

/// Every topic a row publishes to, which is what the execution role is granted the publish on.
const PUBLISHED_TOPICS: &[&str] = &[
    EXACT_TOPIC,
    SINGLE_TOPIC,
    DEEP_TOPIC,
    DISABLED_TOPIC,
    UNWATCHED_TOPIC,
];

/// The rule over each covered topic, written as its topic FILTER and the name it is stored
/// under. A rule name carries letters, digits and underscores alone (`RuleName`, `pattern
/// ^[a-zA-Z0-9_]+$`).
const EXACT_RULE: (&str, &str) = ("xsvcdisc_exact", EXACT_TOPIC);
const SINGLE_RULE: (&str, &str) = ("xsvcdisc_single", "xsvcdisc/+/single");
const DEEP_RULE: (&str, &str) = ("xsvcdisc_deep", "xsvcdisc/deep/#");
const DISABLED_RULE: (&str, &str) = ("xsvcdisc_disabled", DISABLED_TOPIC);

/// The queue every rule's `sqs` action forwards to, and the role it runs under.
const QUEUE: &str = "xsvcdisc-forwarded";
const RULE_ROLE: &str = "xsvcdisc-rule-role";

/// The role the function runs as, the function itself, and the stack that deploys it.
const CALLER_ROLE: &str = "xsvcdisc-caller-role";
const CALLER_FUNCTION: &str = "xsvcdisc-caller";
const CALLER_LOGICAL_ID: &str = "Caller";
const STACK: &str = "xsvcdisc-caller-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principals the two roles trust: the rules engine acts as AWS IoT, and the
/// function's execution role is assumed by Lambda.
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// Where Lambda binds the collection the function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the function carries, and how long one invocation is given: well past what a run
/// needs on any host, because what is under test is the call path and not how fast an
/// interpreter gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` the function has to report before it is invoked.
const ACTIVE: &str = "Active";

/// The status an accepted publish is answered with (`Publish`, `code 200`).
const PUBLISH_OK: u64 = 200;

/// How long an observation of work the CLOCK owes — the stack completing, the function going
/// `Active`, a forwarded message reaching the queue — waits before calling it a hang, and how
/// long between two looks. Generous: a row fails only when the work did not happen at all, so a
/// loaded host cannot decide the outcome. Every wait is on its own CONDITION.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of the queue makes, in seconds.
const WAIT_TIME_SECONDS: u64 = 1;

/// The marker the LAST publish of the suite carries, over the topic a rule names outright: once
/// it is in the queue every publish made before it has been forwarded or was never forwarded at
/// all, so what the queue holds can be compared against what the rows expected without waiting
/// out a fixed span for the rows that expect nothing.
const SENTINEL: &str = "sentinel";

/// The handler the function runs: `import boto3` at MODULE scope, then — for each invocation —
/// the endpoint discovery the event asks for, a data-plane client built on the address that
/// discovery announced, and the one publish, reporting the status the SDK saw. The invocation's
/// payload IS what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

REGISTRY = boto3.client("iot")


def handler(event, context):
    requested = event.get("endpointType")
    if requested is None:
        endpoint = REGISTRY.describe_endpoint()["endpointAddress"]
    else:
        endpoint = REGISTRY.describe_endpoint(endpointType=requested)["endpointAddress"]
    data = boto3.client("iot-data", endpoint_url="https://" + endpoint)
    try:
        answered = data.publish(
            topic=event["topic"],
            qos=event["qos"],
            payload=json.dumps({"marker": event["marker"]}),
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "endpoint": endpoint,
            "code": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "endpoint": endpoint,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }
"#;

/// One row: the discovery the handler starts from, the publish it then makes, and whether the
/// rules engine must forward that message to the queue.
struct Row {
    case: &'static str,
    /// The `endpointType` the handler asks for, or `None` to ask for no particular one.
    endpoint_type: Option<&'static str>,
    topic: &'static str,
    qos: u8,
    marker: &'static str,
    /// Whether the message must REACH the queue, which is what the stored rules decide.
    forwarded: bool,
}

/// The rows, each differing from the others along the discovery, the `qos`, the form of the
/// filter that covers the topic, and whether an enabled rule covers it at all.
///
/// The endpoint types named are the data endpoints of the catalog the `endpointType` member
/// documents ("`iot:Data` - Returns a VeriSign signed data endpoint. `iot:Data-ATS` - Returns an
/// ATS signed data endpoint."), read with:
///
/// ```text
/// smithy_query(query="doc",shape="DescribeEndpointRequest",member="endpointType",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
/// ```
///
/// The `qos` values are the two the modelled `Qos` range admits (`range 0..1`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the ATS data endpoint, a rule naming the topic outright, qos 0",
            endpoint_type: Some("iot:Data-ATS"),
            topic: EXACT_TOPIC,
            qos: 0,
            marker: "ats-exact",
            forwarded: true,
        },
        Row {
            case: "the plain data endpoint, a rule whose filter is a single-level wildcard, qos 1",
            endpoint_type: Some("iot:Data"),
            topic: SINGLE_TOPIC,
            qos: 1,
            marker: "data-single-level",
            forwarded: true,
        },
        Row {
            case: "no endpoint type asked for, a rule whose filter is a multi-level wildcard",
            endpoint_type: None,
            topic: DEEP_TOPIC,
            qos: 0,
            marker: "default-multi-level",
            forwarded: true,
        },
        Row {
            case: "a topic whose only rule is disabled",
            endpoint_type: Some("iot:Data-ATS"),
            topic: DISABLED_TOPIC,
            qos: 1,
            marker: "disabled-rule",
            forwarded: false,
        },
        Row {
            case: "a topic no rule's filter covers",
            endpoint_type: Some("iot:Data-ATS"),
            topic: UNWATCHED_TOPIC,
            qos: 0,
            marker: "no-rule",
            forwarded: false,
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
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, &str)]) -> String {
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
    iam(
        router,
        case,
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

/// The ARN of one topic, which is the resource a statement about a publish to it names.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}")
}

/// The stack declaring the one function, running as `role_arn` with the one handler source: the
/// inline source is archived into the deployment package by CloudFormation, which is how a
/// function whose handler is written here comes to exist at all.
fn function_stack(role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            CALLER_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": CALLER_FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// Look again until the stack first reports [`CREATE_COMPLETE`], and answer the last status it
/// reported with the answer that status was read from. Waits on the CONDITION, not on a span.
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

/// Look again until the function reports `Active`, which is the state "you can't invoke or
/// modify the function" before. Waits on the CONDITION, not on a span.
async fn wait_for_active(router: &MultiServiceRouter, case: &str, function: &str) {
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
            "case {case:?}: GetFunctionConfiguration({function}) must succeed, got {status} with \
             {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: {function} never reported {ACTIVE}, last {last}");
}

/// Invoke the function with `event` and answer what the invocation reported: the
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

/// The event one row invokes the function with.
fn event(row: &Row) -> Value {
    let mut asked = json!({
        "topic": row.topic,
        "qos": row.qos,
        "marker": row.marker,
    });
    if let Some(endpoint_type) = row.endpoint_type {
        asked["endpointType"] = json!(endpoint_type);
    }
    asked
}

/// Drain the queue until the sentinel's message has been seen, and answer every marker the queue
/// held. Waits on the CONDITION — the last publish of the suite arriving — rather than on a
/// span, so a row that expects NOTHING is judged against a queue that has already taken
/// everything that was ever forwarded.
async fn drained_markers(router: &MultiServiceRouter, case: &str, queue_url: &str) -> BTreeSet<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < giving_up_at && !seen.contains(SENTINEL) {
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
            seen.insert(forwarded_marker(case, body));
        }
    }
    assert!(
        seen.contains(SENTINEL),
        "case {case:?}: the rule never forwarded the last publish of the suite, so what the queue \
         holds says nothing; it held {seen:?}"
    );
    seen
}

/// The marker a forwarded body carries. The actions below all leave `useBase64` false, so the
/// body is required to be the published document itself — "exactly the published payload" is
/// what the action forwards.
fn forwarded_marker(case: &str, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|document| {
            document
                .get("marker")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: a forwarded message is not the published document: {body:?}")
        })
}

/// RULE: the address `iot:DescribeEndpoint` announces is one a client inside the emulator reaches
/// the IoT data plane at, a `Publish` made there answers HTTP 200, and the message is forwarded
/// by every enabled topic rule whose filter covers the topic and by no other rule.
#[tokio::test]
async fn a_discovered_data_endpoints_publish_is_routed_by_the_covering_topic_rules() {
    let router = emulator();

    // The queue every rule forwards to, and the ARN a statement about that forward names.
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

    // The role the rules engine performs each `sqs` action under.
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

    // One rule per covered topic filter, each forwarding the selected message to the queue as
    // the document it was published as — and the one that is stored DISABLED, which is a rule
    // the engine must skip.
    let rules: [((&str, &str), bool); 4] = [
        (EXACT_RULE, false),
        (SINGLE_RULE, false),
        (DEEP_RULE, false),
        (DISABLED_RULE, true),
    ];
    for ((rule, filter), disabled) in &rules {
        let payload = json!({
            "sql": format!("SELECT * FROM '{filter}'"),
            "awsIotSqlVersion": "2016-03-23",
            "ruleDisabled": disabled,
            "actions": [{
                "sqs": {
                    "roleArn": rule_role,
                    "queueUrl": queue_url,
                    "useBase64": false,
                },
            }],
        });
        let (status, answered) = rest(
            &router,
            "iot",
            "POST",
            &format!("/rules/{rule}"),
            Some(payload.to_string().into_bytes()),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateTopicRule({rule}) must succeed, got {status} with {answered}"
        );
    }

    // The execution role the handler's own calls are decided against: the endpoint lookup it
    // starts from, and the publish on each topic ARN a row addresses.
    let caller_role = given_role(
        &router,
        "setup",
        CALLER_ROLE,
        LAMBDA_PRINCIPAL,
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": "iot:DescribeEndpoint", "Resource": "*" },
                {
                    "Effect": "Allow",
                    "Action": "iot:Publish",
                    "Resource": PUBLISHED_TOPICS
                        .iter()
                        .map(|topic| topic_arn(topic))
                        .collect::<Vec<_>>(),
                },
            ],
        }),
    )
    .await;

    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&caller_role)),
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
    wait_for_active(&router, "setup", CALLER_FUNCTION).await;

    let mut expected_markers: BTreeSet<String> = BTreeSet::new();
    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(&router, case, CALLER_FUNCTION, &event(&row)).await;
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
        );
        assert_eq!(
            payload["ok"],
            json!(true),
            "case {case:?}: expected the handler's publish to be served, got {payload}"
        );
        assert_eq!(
            payload["status"],
            json!(PUBLISH_OK),
            "case {case:?}: expected HTTP {PUBLISH_OK} from the data plane, got {payload}"
        );
        // The discovered address is what the handler DIALLED, so it has to be an address and not
        // a URL: `endpointAddress` is documented as `identifier.iot.region.amazonaws.com`, which
        // a client prefixes with a scheme itself.
        let announced = payload["endpoint"].as_str().unwrap_or_default();
        assert!(
            !announced.is_empty()
                && !announced.contains("://")
                && !announced.contains('/'),
            "case {case:?}: expected the announced endpoint to be a bare address, got \
             {announced:?}"
        );
        if row.forwarded {
            expected_markers.insert(row.marker.to_string());
        }
    }

    // The last publish of the suite, over the topic a rule names outright: once its message is in
    // the queue, everything that was ever forwarded is there to compare against.
    let (signal, payload) = invoke(
        &router,
        "sentinel",
        CALLER_FUNCTION,
        &json!({
            "endpointType": "iot:Data-ATS",
            "topic": EXACT_TOPIC,
            "qos": 0,
            "marker": SENTINEL,
        }),
    )
    .await;
    assert_eq!(
        signal, None,
        "case \"sentinel\": expected no FunctionError, got {signal:?} with {payload}"
    );
    expected_markers.insert(SENTINEL.to_string());

    let forwarded = drained_markers(&router, "the forwarded messages", &queue_url).await;
    assert_eq!(
        forwarded, expected_markers,
        "case \"the forwarded messages\": expected the queue to hold exactly the markers of the \
         publishes an enabled rule covers, got {forwarded:?}"
    );
}
