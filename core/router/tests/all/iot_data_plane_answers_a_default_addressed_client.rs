// SPDX-License-Identifier: CC0-1.0

//! The IoT DATA PLANE as the AWS SDK inside a running function addresses it — over the real
//! `MultiServiceRouter`, the whole emulator on one engine, and a `python3.x` function whose
//! package bundles no SDK.
//!
//! RULE: a data-plane client a running handler builds reaches this emulator's IoT data plane
//! whichever address the SDK sends its calls to — the account's own data endpoint the handler
//! DISCOVERED (`iot:DescribeEndpoint`), or, for a client built with no endpoint of its own, the
//! service's regional endpoint the SDK resolves by default — so every such call TERMINATES with
//! that plane's own answer rather than hanging on an address nothing serves: HTTP 200 for a
//! `Publish` the execution role's policies allow on the topic the request addresses
//! (`topic/<topic>`), whose message is then observable on that topic, and `ForbiddenException`
//! (HTTP 403) for one they do not, which publishes nothing.
//!
//! The rows differ along what the rule generalises over: WHERE the handler's client is addressed
//! (no endpoint at all, the ATS data endpoint it discovered, the plain data endpoint it
//! discovered), how the published topic is shaped (one level, and a multi-level name whose
//! separators travel percent-encoded, so the resource the decision is taken on is the topic's
//! whole name), the FORM the handler hands its payload over in (the text of a document, and its
//! bytes), the `qos` the publish carries (`0` and `1`, the two values the modelled `Qos` range
//! admits), and whether the execution role's grant names the topic at all. Each row asserts both
//! halves — what the handler's own call answered, which is what it returns as the invocation's
//! payload, and what the DATA PLANE holds afterwards, read back through its own
//! `GetRetainedMessage` — so a call that answered without publishing, or published without
//! answering, fails here, and a call that never came back fails as the invocation's own
//! `FunctionError`.
//!
//! Four crates have to agree for that to hold — Lambda runs the handler and supplies it the SDK,
//! IAM holds the execution role and its inline document, STS mints the session the handler's
//! calls are signed with, and the IoT control plane announces the endpoint while the IoT data
//! plane takes the decision and the message — so the suite lives here: a crate-local suite can
//! only ever see one side of it.
//!
//! What this suite is about that its neighbours are not: `iot_data_plane_from_a_lambda_handler.rs`
//! varies the grant in force and the data-plane operation called, and
//! `iot_discovered_data_endpoint_routes_a_published_message.rs` varies which stored rule the topic
//! selects; both build their client on a DISCOVERED address. This one varies the ADDRESS the
//! client is built with, and reads the published message back off the topic itself.

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

/// The role the function runs as, the function itself, and the stack that deploys it.
const CALLER_ROLE: &str = "xsvcaddr-caller-role";
const CALLER_FUNCTION: &str = "xsvcaddr-caller";
const CALLER_LOGICAL_ID: &str = "Caller";
const STACK: &str = "xsvcaddr-caller-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// Where Lambda binds the collection the function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the function carries, and how long one invocation is given: well past what a run
/// needs on any host, because what is under test is the call path and not how fast an interpreter
/// gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` the function has to report before it is invoked.
const ACTIVE: &str = "Active";

/// The status an accepted publish is answered with (`Publish`, `code 200`).
const PUBLISH_OK: u64 = 200;

/// What a caller whose identity-based policies do not grant the action is answered with by this
/// data plane: the error code and the status that go with it.
const FORBIDDEN: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u64 = 403;

/// The status a topic with no retained message is read back with (`GetRetainedMessage` declares
/// `ResourceNotFoundException` with `httpError` 404).
const NOT_RETAINED_STATUS: u16 = 404;

/// How long an observation of work the CLOCK owes — the stack completing, the function going
/// `Active` — waits before calling it a hang, and how long between two looks. Generous: a row
/// fails only when the work did not happen at all, so a loaded host cannot decide the outcome.
/// Every wait is on its own CONDITION.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler the function runs: `import boto3` at MODULE scope, then — for each invocation —
/// the data-plane client the event asks for, and the one publish, reporting what the SDK saw.
/// The invocation's payload IS what the SDK gave the guest.
///
/// A client is built one of two ways: on the address the registry ANNOUNCED for the endpoint type
/// the event names, and — when the event names none — with no endpoint at all, which is the
/// client `boto3.client("iot-data")` builds and which the SDK addresses at the service's own
/// regional endpoint.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

REGISTRY = boto3.client("iot")


def handler(event, context):
    asked = event.get("endpointType")
    if asked is None:
        announced = ""
        data = boto3.client("iot-data")
    else:
        announced = REGISTRY.describe_endpoint(endpointType=asked)["endpointAddress"]
        data = boto3.client("iot-data", endpoint_url="https://" + announced)
    document = json.dumps({"marker": event["marker"]})
    carried = document.encode("utf-8") if event["bytes"] else document
    try:
        answered = data.publish(
            topic=event["topic"], qos=event["qos"], retain=True, payload=carried
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "announced": announced,
            "document": document,
            "code": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "announced": announced,
        "document": document,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }
"#;

/// One row: where the handler's data-plane client is addressed, the publish it then makes, and
/// whether the execution role's grant names the topic it addresses.
struct Row {
    case: &'static str,
    /// The `endpointType` the handler discovers its address with, or `None` for a client built
    /// with no endpoint at all.
    endpoint_type: Option<&'static str>,
    topic: &'static str,
    qos: u64,
    /// Whether the handler hands the payload over as BYTES rather than as the document's text.
    bytes: bool,
    marker: &'static str,
    /// Whether the role's inline document grants `iot:Publish` on this topic's ARN.
    granted: bool,
}

/// The rows, each differing from the others along the address the client is built with, the shape
/// of the topic, the form of the payload, the `qos` and the grant.
///
/// The endpoint types named are the data endpoints of the catalog the `endpointType` member
/// documents ("`iot:Data` - Returns a VeriSign signed data endpoint. `iot:Data-ATS` - Returns an
/// ATS signed data endpoint."), read with:
///
/// ```text
/// smithy_query(query="doc",shape="DescribeEndpointRequest",member="endpointType",model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")
/// ```
///
/// The `qos` values are the two the modelled `Qos` range admits (`range 0..1`), read with:
///
/// ```text
/// smithy_query(query="operation",operation="Publish",model_path="services/service-com-amazonaws-iotdataplane-iotmoonrakerservice/smithy-model/com-amazonaws-iotdataplane-iotmoonrakerservice.json")
/// ```
///
/// Every topic a row publishes to is its OWN, because a retained publish replaces the topic's
/// retained message: one topic per row is what lets each row's message be read back as its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client built with no endpoint, a one-level topic, the document's text, qos 0",
            endpoint_type: None,
            topic: "xsvcaddr-default-text",
            qos: 0,
            bytes: false,
            marker: "default-text",
            granted: true,
        },
        Row {
            case: "a client built with no endpoint, the payload handed over as bytes, qos 1",
            endpoint_type: None,
            topic: "xsvcaddr-default-bytes",
            qos: 1,
            bytes: true,
            marker: "default-bytes",
            granted: true,
        },
        Row {
            case: "a client built with no endpoint, a multi-level topic",
            endpoint_type: None,
            topic: "xsvcaddr/deep/default",
            qos: 0,
            bytes: false,
            marker: "default-deep",
            granted: true,
        },
        Row {
            case: "a client built on the discovered ATS data endpoint, qos 1",
            endpoint_type: Some("iot:Data-ATS"),
            topic: "xsvcaddr-ats",
            qos: 1,
            bytes: false,
            marker: "ats",
            granted: true,
        },
        Row {
            case: "a client built on the discovered plain data endpoint, bytes, qos 0",
            endpoint_type: Some("iot:Data"),
            topic: "xsvcaddr-data",
            qos: 0,
            bytes: true,
            marker: "data",
            granted: true,
        },
        Row {
            case: "a client built with no endpoint, a topic the role's grant does not name",
            endpoint_type: None,
            topic: "xsvcaddr-default-barred",
            qos: 0,
            bytes: false,
            marker: "default-barred",
            granted: false,
        },
        Row {
            case: "a client built on the discovered ATS endpoint, a topic the grant does not name",
            endpoint_type: Some("iot:Data-ATS"),
            topic: "xsvcaddr-ats-barred",
            qos: 1,
            bytes: true,
            marker: "ats-barred",
            granted: false,
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

/// One HTTP call into the emulator: the status and the body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (u16, http::HeaderMap, Vec<u8>) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
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

/// One restJson1 call to a service of this emulator, answered with its status and its document:
/// the IoT data plane and Lambda both bind their operations that way.
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

/// Look again until the function reports `Active`, which is the state "you can't invoke or modify
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
        "bytes": row.bytes,
        "marker": row.marker,
    });
    if let Some(endpoint_type) = row.endpoint_type {
        asked["endpointType"] = json!(endpoint_type);
    }
    asked
}

/// The topic's RETAINED MESSAGE as the data plane holds it, read back through its own
/// `GetRetainedMessage` (`GET /retainedMessage/{topic}`): the status, and the document for a topic
/// that has one.
///
/// The topic travels as one path SEGMENT — `topic` is a plain `@httpLabel`, so a separator inside
/// the name is percent-encoded rather than splitting the path. That is why the address is written
/// with `form_encode`, which escapes `/` as well, rather than with the path encoder beside it,
/// which leaves `/` literal and would address a topic of several segments.
async fn retained_message(router: &MultiServiceRouter, topic: &str) -> (u16, Value) {
    rest(
        router,
        "iotdata",
        "GET",
        &format!("/retainedMessage/{}", form_encode(topic)),
        None,
    )
    .await
}

/// The bytes a `payload` member of an answer carries: a blob, which a `restJson1` body renders as
/// base64.
fn payload_bytes(case: &str, held: &Value) -> Vec<u8> {
    let carried = held["payload"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the retained message carries no payload: {held}"));
    ::aws_smithy_types::base64::decode(carried).unwrap_or_else(|error| {
        panic!("case {case:?}: a payload is base64 ({error}): {carried:?}")
    })
}

/// RULE: a data-plane client a running handler builds reaches the IoT data plane whether it is
/// addressed at the account's discovered data endpoint or left at the regional endpoint the SDK
/// resolves by default — the call terminates with HTTP 200 for a publish the execution role allows
/// and `ForbiddenException` (403) for one it does not — and an allowed publish's message is
/// observable on the topic it was published to.
#[tokio::test]
async fn an_in_function_publish_terminates_at_either_address_the_data_plane_answers() {
    let router = emulator();

    // The execution role the handler's own calls are decided against: the endpoint discovery it
    // may make, and the publish on the ARN of each topic a GRANTED row addresses — and of no
    // other, which is what leaves the barred rows refused.
    let granted: Vec<String> = rows()
        .iter()
        .filter(|row| row.granted)
        .map(|row| topic_arn(row.topic))
        .collect();
    let caller_role = given_role(
        &router,
        "setup",
        CALLER_ROLE,
        LAMBDA_PRINCIPAL,
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": "iot:DescribeEndpoint", "Resource": "*" },
                { "Effect": "Allow", "Action": "iot:Publish", "Resource": granted },
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
    wait_for_active(&router, CALLER_FUNCTION).await;

    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(&router, case, CALLER_FUNCTION, &event(&row)).await;
        // A call that never came back is what this issue's symptom was: the invocation then ends
        // on its own timeout and reports a `FunctionError` instead of the plane's answer.
        assert_eq!(
            signal, None,
            "case {case:?}: expected the handler to return with the data plane's own answer and \
             no FunctionError, got {signal:?} with {payload}"
        );
        let document = payload["document"].as_str().unwrap_or_default().to_string();
        assert!(
            !document.is_empty(),
            "case {case:?}: the handler reported no document it published: {payload}"
        );
        // A discovered address is what the handler DIALLED, so it has to be an address and not a
        // URL: `endpointAddress` is documented as `identifier.iot.region.amazonaws.com`, which a
        // client prefixes with a scheme itself. A row whose client is built with no endpoint
        // discovers nothing, so there is no announced address of this emulator's to judge.
        if row.endpoint_type.is_some() {
            let announced = payload["announced"].as_str().unwrap_or_default();
            assert!(
                !announced.is_empty() && !announced.contains("://") && !announced.contains('/'),
                "case {case:?}: expected the announced endpoint to be a bare address, got \
                 {announced:?}"
            );
        }

        let (held_status, held) = retained_message(&router, row.topic).await;
        if row.granted {
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
            assert_eq!(
                held_status, 200,
                "case {case:?}: expected the published message to be observable on {}, got \
                 {held_status} with {held}",
                row.topic
            );
            assert_eq!(
                String::from_utf8_lossy(&payload_bytes(case, &held)),
                document,
                "case {case:?}: expected the topic to hold exactly the published document, got \
                 {held}"
            );
            assert_eq!(
                held["qos"],
                json!(row.qos),
                "case {case:?}: expected the topic to hold the published qos {}, got {held}",
                row.qos
            );
            assert_eq!(
                held["topic"],
                json!(row.topic),
                "case {case:?}: expected the retained message to name its own topic, got {held}"
            );
        } else {
            assert_eq!(
                payload["ok"],
                json!(false),
                "case {case:?}: expected the handler's publish to be refused, got {payload}"
            );
            assert_eq!(
                payload["code"],
                json!(FORBIDDEN),
                "case {case:?}: expected error code {FORBIDDEN}, got {payload}"
            );
            assert_eq!(
                payload["status"],
                json!(FORBIDDEN_STATUS),
                "case {case:?}: expected HTTP {FORBIDDEN_STATUS}, got {payload}"
            );
            assert_eq!(
                held_status, NOT_RETAINED_STATUS,
                "case {case:?}: a refused publish must leave {} with no retained message, got \
                 {held_status} with {held}",
                row.topic
            );
        }
    }
}
