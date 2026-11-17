// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT DATA PLANE as a running Lambda handler reaches it, end to end over the real
//! `MultiServiceRouter`: the whole emulator on one engine, a `python3.x` function whose
//! package bundles no SDK, and the message it publishes followed all the way to the queue an
//! IoT topic rule forwards it to.
//!
//! RULE: a managed runtime ships the AWS SDK for the handler's language on its import path
//! with EVERY plane this emulator serves reachable through it — the device registry and the
//! device DATA plane alike, which is the pair a function uses (`iot:DescribeEndpoint` to learn
//! where the data plane answers, then `Publish` / `UpdateThingShadow` / `GetThingShadow`
//! against it) — and a call the SDK makes is dispatched into that plane as a session of the
//! function's EXECUTION ROLE, so the plane decides it against the policies in force on that
//! role: a call the role's identity-based policies allow on the resource the request addresses
//! (`topic/<topic>` for a message, `thing/<thingName>` for a shadow) is served and its effect
//! is where a client sees it, and one they do not allow is refused `ForbiddenException`
//! (HTTP 403) with the plane's own envelope — a NULL message — and performs nothing.
//!
//! The second half of the rule is what makes a served publish observable at all: a message the
//! broker took is evaluated by the rules engine, and an enabled rule whose statement selects it
//! has its actions performed — for an `sqs` action, one `sqs:SendMessage` of the selected
//! message to the queue the action's `queueUrl` addresses, under the action's own role, in the
//! form the action asks for (`useBase64`) — and NOTHING is sent for an action that cannot be
//! performed: one whose role does not grant that send, and one whose `queueUrl` carries a
//! substitution template this emulator's rules engine does not render.
//!
//! Five crates have to agree for that to hold — Lambda runs the handler and supplies it the
//! SDK, IAM holds the execution role and its inline document, STS mints the session the
//! handler's calls are signed with, the IoT data plane takes the authorization decision and the
//! IoT control plane's rules engine forwards the message to Amazon SQS — so the suite lives
//! here: a crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the data plane the
//! handler calls (a message publish, a shadow write, a shadow read), what the execution role
//! allows while it calls (every action of the service, exactly the action called scoped to the
//! resource it addresses, another action of the service, nothing of the data plane at all),
//! how the topic the publish addresses is shaped (one level, and a multi-level name whose
//! separators travel percent-encoded, so the resource the decision is taken on is the topic's
//! whole name), the form the handler hands its payload over in (the text of a document, and
//! its bytes), and what the RULE over the topic published to does with the message (forward it
//! as the document, forward it base64-encoded, or forward nothing because the action's role
//! bars the send or its `queueUrl` is a template). Each row asserts both halves — what the
//! handler read back, which is what it returns as the invocation's payload, and what the TARGET
//! holds afterwards: the queue the rule forwards to, and the shadow read back through the data
//! plane's own `GetThingShadow` — so a call that answered without doing the work, or did the
//! work without answering, fails here.

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

/// The topic the one-level rows publish to, and the multi-level one the deep row publishes to.
const TOPIC: &str = "xsvc-iot-data-telemetry";
const DEEP_TOPIC: &str = "xsvc/iot/data/deep";

/// The three topics whose rules are about what the `sqs` action does rather than about who may
/// publish: one whose action asks for base64 encoding, one whose action's role does not grant
/// the queue send, and one whose action's `queueUrl` carries a substitution template this
/// emulator's rules engine does not render.
const BASE64_TOPIC: &str = "xsvc-iot-data-base64";
const BARRED_TOPIC: &str = "xsvc-iot-data-barred";
const TEMPLATED_TOPIC: &str = "xsvc-iot-data-templated";

/// The rule over each of those topics, and the queue both forward to. A rule name carries
/// letters, digits and underscores alone (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const RULE: &str = "xsvc_iot_data_telemetry";
const DEEP_RULE: &str = "xsvc_iot_data_deep";
const BASE64_RULE: &str = "xsvc_iot_data_base64";
const BARRED_RULE: &str = "xsvc_iot_data_barred";
const TEMPLATED_RULE: &str = "xsvc_iot_data_templated";
const QUEUE: &str = "xsvc-iot-data-forwarded";

/// The role one rule's `sqs` action runs under that grants NOTHING, which is the only
/// observable the action's permission boundary has.
const BARRED_ROLE: &str = "xsvc-iot-data-barred-role";

/// The thing the shadow rows address.
const THING: &str = "xsvc-iot-data-sensor";

/// The role the rules engine performs its `sqs` action under, and the service principal that
/// role has to trust.
const RULE_ROLE: &str = "xsvc-iot-data-rule-role";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The stack the functions are deployed by, and the `StackStatus` it has to reach.
const STACK: &str = "xsvc-iot-data-callers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// Where Lambda binds the collection the functions are read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below carries, and how long one invocation is given: well past
/// what a run needs on any host, because what is under test is the call path and not how fast
/// an interpreter gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `State` a function has to report before it is invoked.
const ACTIVE: &str = "Active";

/// The code and status the IoT data plane refuses a call the policies in force do not permit
/// with.
const FORBIDDEN: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u64 = 403;

/// How long an observation of work the CLOCK owes — a function going `Active`, a stack
/// completing, a message reaching the queue — waits before calling it a hang, and how long
/// between two looks. Generous: a row fails only when the work did not happen at all, so a
/// loaded host cannot decide the outcome. Each wait is on its own CONDITION, and the whole
/// suite needs seconds of it when nothing is wrong.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The long poll one drain of the queue makes, in seconds.
const WAIT_TIME_SECONDS: u64 = 1;

/// The marker the last publish of the suite carries: once it is in the queue every publish
/// made before it has been forwarded or was never sent, so what the queue holds can be
/// compared against what the rows expected without waiting out a fixed time for the rows that
/// expect nothing.
const SENTINEL: &str = "sentinel";

/// The handler every function runs: `import boto3` at MODULE scope, the registry client and
/// the data-plane endpoint resolved there too — so a runtime shipping no SDK, or one whose SDK
/// cannot build a client for either plane, fails the initialization phase rather than this call
/// — and then the ONE data-plane call the event names, reporting what that call answered. The
/// invocation's payload IS what the SDK gave the guest.
///
/// A publish hands its payload over as the TEXT of a document and a shadow write as its BYTES,
/// which are the two forms an SDK takes for a member the model types as a blob.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

REGISTRY = boto3.client("iot")
ENDPOINT = REGISTRY.describe_endpoint(endpointType="iot:Data-ATS")["endpointAddress"]
DATA = boto3.client("iot-data", endpoint_url="https://" + ENDPOINT)


def handler(event, context):
    call = event["call"]
    try:
        if call == "publish":
            DATA.publish(
                topic=event["topic"],
                qos=1,
                payload=json.dumps({"marker": event["marker"]}),
            )
            answered = {}
        elif call == "update_shadow":
            document = json.dumps({"state": {"reported": event["state"]}})
            got = DATA.update_thing_shadow(
                thingName=event["thing"], payload=document.encode("utf-8")
            )
            answered = json.loads(got["payload"].read())
        else:
            got = DATA.get_thing_shadow(thingName=event["thing"])
            answered = json.loads(got["payload"].read())
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"ok": True, "endpoint": ENDPOINT, "answered": answered}
"#;

/// What one row's execution role allows while its handler calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Every action of AWS IoT, on every resource.
    EveryAction,
    /// Exactly the publish the rows make, scoped to the ARN of each topic they address.
    PublishOnTheseTopics,
    /// Another action of the service: the shadow READ, scoped to the thing's ARN.
    ShadowReadOnly,
    /// Nothing of the data plane: the endpoint lookup alone, which every role needs for the
    /// handler to reach its module scope at all.
    EndpointOnly,
}

impl Grant {
    /// Every grant, each with the name its role and function carry.
    const ALL: &'static [(Self, &'static str)] = &[
        (Self::EveryAction, "every"),
        (Self::PublishOnTheseTopics, "publish"),
        (Self::ShadowReadOnly, "read"),
        (Self::EndpointOnly, "endpoint"),
    ];

    /// The name of the role this grant is put on.
    fn role(self) -> String {
        format!("xsvc-iot-data-role-{}", self.leaf())
    }

    /// The name of the function that runs as that role.
    fn function(self) -> String {
        format!("xsvc-iot-data-fn-{}", self.leaf())
    }

    /// The logical id the stack declares that function under.
    fn logical_id(self) -> String {
        format!("Caller{}", self.leaf())
    }

    fn leaf(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(grant, _leaf)| *grant == self)
            .map(|(_grant, leaf)| *leaf)
            .unwrap_or_default()
    }

    /// The identity policy in force on that role: every statement always carries the endpoint
    /// lookup, because the handler resolves the data plane's address at module scope.
    fn document(self) -> Value {
        let mut statements = vec![json!({
            "Effect": "Allow",
            "Action": "iot:DescribeEndpoint",
            "Resource": "*",
        })];
        match self {
            Self::EveryAction => {
                statements = vec![json!({
                    "Effect": "Allow",
                    "Action": "iot:*",
                    "Resource": "*",
                })];
            }
            Self::PublishOnTheseTopics => statements.push(json!({
                "Effect": "Allow",
                "Action": "iot:Publish",
                "Resource": [topic_arn(TOPIC), topic_arn(DEEP_TOPIC)],
            })),
            Self::ShadowReadOnly => statements.push(json!({
                "Effect": "Allow",
                "Action": "iot:GetThingShadow",
                "Resource": thing_arn(THING),
            })),
            Self::EndpointOnly => {}
        }
        json!({ "Version": "2012-10-17", "Statement": statements })
    }
}

/// The ARN of one topic, which is the resource a statement about a publish to it names.
fn topic_arn(topic: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:topic/{topic}")
}

/// The ARN of one thing, which is the resource a statement about its shadow names.
fn thing_arn(thing: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{thing}")
}

/// One row: the call the handler makes, the role it runs as, and what the call must answer.
struct Row {
    case: &'static str,
    grant: Grant,
    /// The event the function is invoked with.
    event: Value,
    /// The marker a publishing row's message carries, which is what the queue is read for.
    /// `None` for a row that publishes nothing.
    marker: Option<&'static str>,
    /// `true` for a call that must be served, `false` for one the data plane must refuse.
    served: bool,
    /// Whether the message of a served publish must REACH the queue: the rule over the topic
    /// decides that, so a publish the data plane takes still forwards nothing when the rule's
    /// action cannot be performed.
    forwarded: bool,
    /// What the shadow must report once this row has run, for a row that is about the shadow.
    reported: Option<Value>,
}

/// The battery level the granted shadow write reports, and the one the refused write tries to.
const WRITTEN: i64 = 87;
const NEVER_WRITTEN: i64 = 11;

/// The rows, in the order they run: each later row stands on what the earlier ones wrote, which
/// is what makes the shadow read and the refused write about stored state.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a publish the role allows through the whole service namespace",
            grant: Grant::EveryAction,
            event: json!({ "call": "publish", "topic": TOPIC, "marker": "every-action" }),
            marker: Some("every-action"),
            served: true,
            forwarded: true,
            reported: None,
        },
        Row {
            case: "a publish the role allows on exactly that topic's ARN",
            grant: Grant::PublishOnTheseTopics,
            event: json!({ "call": "publish", "topic": TOPIC, "marker": "scoped-topic" }),
            marker: Some("scoped-topic"),
            served: true,
            forwarded: true,
            reported: None,
        },
        Row {
            case: "a publish to a multi-level topic the role allows on that topic's ARN",
            grant: Grant::PublishOnTheseTopics,
            event: json!({ "call": "publish", "topic": DEEP_TOPIC, "marker": "deep-topic" }),
            marker: Some("deep-topic"),
            served: true,
            forwarded: true,
            reported: None,
        },
        Row {
            case: "a publish whose rule's action asks for base64 encoding",
            grant: Grant::EveryAction,
            event: json!({ "call": "publish", "topic": BASE64_TOPIC, "marker": "base64" }),
            marker: Some("base64"),
            served: true,
            forwarded: true,
            reported: None,
        },
        Row {
            case: "a publish whose rule's action runs under a role granting no queue send",
            grant: Grant::EveryAction,
            event: json!({ "call": "publish", "topic": BARRED_TOPIC, "marker": "barred" }),
            marker: Some("barred"),
            served: true,
            forwarded: false,
            reported: None,
        },
        Row {
            case: "a publish whose rule's action names a queue by a substitution template",
            grant: Grant::EveryAction,
            event: json!({ "call": "publish", "topic": TEMPLATED_TOPIC, "marker": "templated" }),
            marker: Some("templated"),
            served: true,
            forwarded: false,
            reported: None,
        },
        Row {
            case: "a publish the role allows another action of the service instead",
            grant: Grant::ShadowReadOnly,
            event: json!({ "call": "publish", "topic": TOPIC, "marker": "other-action" }),
            marker: Some("other-action"),
            served: false,
            forwarded: false,
            reported: None,
        },
        Row {
            case: "a publish the role allows nothing of the data plane for",
            grant: Grant::EndpointOnly,
            event: json!({ "call": "publish", "topic": TOPIC, "marker": "no-grant" }),
            marker: Some("no-grant"),
            served: false,
            forwarded: false,
            reported: None,
        },
        Row {
            case: "a shadow write the role allows through the whole service namespace",
            grant: Grant::EveryAction,
            event: json!({
                "call": "update_shadow",
                "thing": THING,
                "state": { "battery": WRITTEN },
            }),
            marker: None,
            served: true,
            forwarded: false,
            reported: Some(json!({ "battery": WRITTEN })),
        },
        Row {
            case: "a shadow write the role allows only the read of",
            grant: Grant::ShadowReadOnly,
            event: json!({
                "call": "update_shadow",
                "thing": THING,
                "state": { "battery": NEVER_WRITTEN },
            }),
            marker: None,
            served: false,
            forwarded: false,
            reported: Some(json!({ "battery": WRITTEN })),
        },
        Row {
            case: "a shadow read the role allows on the thing's ARN",
            grant: Grant::ShadowReadOnly,
            event: json!({ "call": "get_shadow", "thing": THING }),
            marker: None,
            served: true,
            forwarded: false,
            reported: Some(json!({ "battery": WRITTEN })),
        },
        Row {
            case: "a shadow read the role allows nothing of the data plane for",
            grant: Grant::EndpointOnly,
            event: json!({ "call": "get_shadow", "thing": THING }),
            marker: None,
            served: false,
            forwarded: false,
            reported: Some(json!({ "battery": WRITTEN })),
        },
    ]
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request, and the handler's OWN calls are signed by the runtime
    // with the credential it was given.
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

/// One restJson1 call to a service of this emulator, answered with its status and its
/// document: the two IoT planes and Lambda all bind their operations that way.
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

/// The stack declaring one function per grant, each running as that grant's role with the one
/// handler source: the inline source is archived into the deployment package by CloudFormation,
/// which is how a function whose handler is written here comes to exist at all.
fn function_stack(role_arns: &[(Grant, String)]) -> String {
    let mut resources = serde_json::Map::new();
    for (grant, role_arn) in role_arns {
        resources.insert(
            grant.logical_id(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": grant.function(),
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
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

/// Look again until the stack `stack` first reports [`CREATE_COMPLETE`], and answer the last
/// status it reported with the answer that status was read from.
///
/// The stack's completion is owed from the instant Lambda reports its functions created, which
/// no fixed sleep can name: this waits on that CONDITION, with this suite's own generous
/// [`DEADLINE`] rather than a neighbour's, so a loaded host delays the setup instead of
/// deciding the outcome.
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

/// Look again until `function` reports `Active`, which is the state "you can't invoke or
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
            "case {case:?}: GetFunctionConfiguration({function}) must succeed, got {status} \
             with {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: {function} never reported {ACTIVE}, last {last}");
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

/// Drain the queue until the sentinel's message has been seen, and answer every marker the
/// queue held. Waits on the CONDITION — the last publish of the suite arriving — rather than on
/// a span, so a row that expects NOTHING is judged against a queue that has already taken
/// everything that was ever sent.
async fn drained_markers(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
) -> BTreeSet<String> {
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
        "case {case:?}: the rule never forwarded the last publish of the suite, so what the \
         queue holds says nothing; it held {seen:?}"
    );
    seen
}

/// The marker the queue message `body` carries, whichever form its rule's action sent it in:
/// the selected message's own document, or — for an action that asked for base64 encoding —
/// the document those bytes decode to.
///
/// Reading BOTH is what makes `useBase64` observable rather than assumed: a body that is not
/// the document is required to be the base64 OF the document, decoded with the one decoder
/// this tree uses for such a body.
fn forwarded_marker(case: &str, body: &str) -> String {
    let read = |text: &str| -> Option<String> {
        serde_json::from_str::<Value>(text)
            .ok()?
            .get("marker")?
            .as_str()
            .map(str::to_string)
    };
    if let Some(marker) = read(body) {
        return marker;
    }
    let decoded = aws_smithy_types::base64::decode(body).unwrap_or_else(|error| {
        panic!("case {case:?}: a forwarded message is neither the document nor its base64 ({error}): {body}")
    });
    let text = String::from_utf8_lossy(&decoded).to_string();
    read(&text).unwrap_or_else(|| {
        panic!("case {case:?}: a forwarded message carries no marker: {body} ({text})")
    })
}

/// What the thing's shadow reports, read where a client reads it: the data plane's own
/// `GetThingShadow`, as the account's own credential.
async fn reported_state(router: &MultiServiceRouter, case: &str, thing: &str) -> Value {
    let (status, document) = rest(
        router,
        "iotdata",
        "GET",
        &format!("/things/{thing}/shadow"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetThingShadow({thing}) must be served to the account, got {status} \
         with {document}"
    );
    document["state"]["reported"].clone()
}

/// RULE: a handler reaches the IoT data plane through the SDK its runtime supplies, its call is
/// dispatched into that plane as the function's execution role and decided against the policies
/// in force on that role, and a message the plane took is forwarded by an enabled topic rule's
/// `sqs` action to the queue it names.
#[tokio::test]
async fn a_running_handler_reaches_the_iot_data_plane_through_its_runtimes_sdk() {
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

    // The thing whose shadow the shadow rows address.
    let (status, created) = rest(
        &router,
        "iot",
        "POST",
        &format!("/things/{THING}"),
        Some(b"{}".to_vec()),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateThing({THING}) must succeed, got {status} with {created}"
    );

    // The rules engine's own role, and the role one rule's action runs under that grants
    // nothing at all.
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
    let barred_role = given_role(
        &router,
        "setup",
        BARRED_ROLE,
        IOT_PRINCIPAL,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "sqs:GetQueueUrl",
                "Resource": queue_arn,
            }],
        }),
    )
    .await;
    // One rule per topic, each forwarding the selected message to the queue — and the three
    // whose action is what the row is about: the base64 encoding, a role that does not grant
    // the send, and a `queueUrl` carrying a substitution template this emulator's rules engine
    // does not render.
    let rules: [(&str, &str, Value); 5] = [
        (
            RULE,
            TOPIC,
            json!({ "roleArn": rule_role, "queueUrl": queue_url, "useBase64": false }),
        ),
        (
            DEEP_RULE,
            DEEP_TOPIC,
            json!({ "roleArn": rule_role, "queueUrl": queue_url }),
        ),
        (
            BASE64_RULE,
            BASE64_TOPIC,
            json!({ "roleArn": rule_role, "queueUrl": queue_url, "useBase64": true }),
        ),
        (
            BARRED_RULE,
            BARRED_TOPIC,
            json!({ "roleArn": barred_role, "queueUrl": queue_url, "useBase64": false }),
        ),
        (
            TEMPLATED_RULE,
            TEMPLATED_TOPIC,
            json!({
                "roleArn": rule_role,
                "queueUrl": format!("{queue_url}-${{topic()}}"),
                "useBase64": false,
            }),
        ),
    ];
    for (rule, topic, action) in &rules {
        let payload = json!({
            "sql": format!("SELECT * FROM '{topic}'"),
            "actions": [{ "sqs": action }],
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

    // One execution role per grant, and one function per role.
    let mut role_arns: Vec<(Grant, String)> = Vec::new();
    for (grant, _leaf) in Grant::ALL {
        let arn = given_role(
            &router,
            "setup",
            &grant.role(),
            LAMBDA_PRINCIPAL,
            &grant.document(),
        )
        .await;
        role_arns.push((*grant, arn));
    }
    let (status, answered) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&role_arns)),
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
    for (grant, _arn) in &role_arns {
        wait_for_active(&router, "setup", &grant.function()).await;
    }

    let mut expected_markers: BTreeSet<String> = BTreeSet::new();
    for row in rows() {
        let case = row.case;
        let function = row.grant.function();
        let (signal, payload) = invoke(&router, case, &function, &row.event).await;
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
        );
        if row.served {
            assert_eq!(
                payload["ok"],
                json!(true),
                "case {case:?}: expected the handler's call to be served, got {payload}"
            );
            // A message the plane TOOK is forwarded only where the rule's own action can be
            // performed, so what the queue must hold is the row's own expectation and not
            // every served publish.
            if let (true, Some(marker)) = (row.forwarded, row.marker) {
                expected_markers.insert(marker.to_string());
            }
        } else {
            assert_eq!(
                payload["ok"],
                json!(false),
                "case {case:?}: expected the call to be refused, got {payload}"
            );
            assert_eq!(
                payload["code"],
                json!(FORBIDDEN),
                "case {case:?}: expected the error code {FORBIDDEN:?}, got {payload}"
            );
            assert_eq!(
                payload["status"],
                json!(FORBIDDEN_STATUS),
                "case {case:?}: expected HTTP {FORBIDDEN_STATUS} from the data plane, got \
                 {payload}"
            );
            let carried = payload.as_object().and_then(|report| report.get("message"));
            assert_eq!(
                carried,
                Some(&Value::Null),
                "case {case:?}: expected the refusal to carry a null message, got {payload}"
            );
        }
        // The other half of a shadow row: what the data plane itself holds for the thing,
        // whether the handler's call was served or refused.
        if let Some(expected) = &row.reported {
            let held = reported_state(&router, case, THING).await;
            assert_eq!(
                &held, expected,
                "case {case:?}: expected the shadow to report {expected}, got {held}"
            );
        }
        // A served shadow read answers the document the plane holds, not an empty one.
        if row.served && row.event["call"] == json!("get_shadow") {
            assert_eq!(
                payload["answered"]["state"]["reported"],
                json!({ "battery": WRITTEN }),
                "case {case:?}: expected the handler to read back the stored shadow, got \
                 {payload}"
            );
        }
    }

    // The last publish of the suite, through the role that may make it: once its message is in
    // the queue, everything that was ever forwarded is there to compare against.
    let (signal, payload) = invoke(
        &router,
        "sentinel",
        &Grant::EveryAction.function(),
        &json!({ "call": "publish", "topic": TOPIC, "marker": SENTINEL }),
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
        "case \"the forwarded messages\": expected the queue to hold exactly the markers of \
         the publishes the data plane served, got {forwarded:?}"
    );
}
