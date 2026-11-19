// SPDX-License-Identifier: CC0-1.0

//! The gate AppConfig consults before it starts a deployment is a function that really RUNS:
//! it is handed the release's own event, and what that run does decides whether the release
//! happens at all.
//!
//! RULE: an extension action point AppConfig consults synchronously before performing a call
//! invokes the target the action's `Uri` names with the event of that call, and gates the call
//! on what the target answers — a target that returns lets the deployment be created, take the
//! next deployment number of its environment and reach the modelled terminal `COMPLETE`
//! (`smithy_query(query="shape", shape="DeploymentState")`) with the version it names in
//! service, while a target whose run RAISED refuses the call with `BadRequestException` /
//! HTTP 400 (`smithy_query(query="operation", operation="StartDeployment")`), in a message
//! naming the extension whose action failed and the id of the invocation that failed, and
//! persists nothing: no deployment record, no deployment number, and the version the last
//! completed deployment put in service is still the one served.
//!
//! Only a cross-service suite can state that end to end: the action's `Uri` names a Lambda
//! function — the one target kind whose run can ANSWER an action point — the function's handler
//! forwards the event it was handed, together with the `context.aws_request_id` of its own run,
//! into an Amazon SQS queue (no AppConfig API reports that event, so the queue is where a client
//! observes it) and runs as an execution role IAM holds, CloudFormation is what archives a
//! handler written out here into a deployment package, AppConfig holds the gate and the
//! deployment sequence, and what is in service is read where an application reads it, out of
//! AppConfig's own data plane.
//!
//! What that correlation buys, and what no neighbour has: the request id the refusal names is
//! the id the FAILING RUN reported to itself, so the refusal is pinned against the invocation
//! it is about rather than against the shape of a sentence.
//!
//! What the neighbours pin and this one does not repeat:
//! `appconfig_gated_release_decides_what_is_in_service` follows a gate whose target cannot be
//! invoked at all through to the deployment sequence, `appconfig_pre_create_action_decides_the_stored_version`
//! has an action's ANSWER decide the bytes a version is created with, and the AppConfig crate's
//! `extension_action_points` pins the event each binding level is handed against a queue target.
//! None of them has a RUN of the gate's own target see the release's event and then veto it.
//!
//! The rows differ along what the rule generalises over: the media type and bytes of the
//! configuration being released (a JSON document, a text document, and bytes that are not text
//! at all) and which of the three levels an extension association binds the gate at. Every row
//! drives the same three legs — a release the gate lets through, the SAME release refused once
//! the target raises, and that identical call again once the target is restored — so the rule is
//! asserted on an accepted release, a vetoed one, and the retry of a vetoed one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the hierarchy, the role, the
/// queue and the function belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the ARN an association names carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The media type a JSON control-plane body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `Type` a `PRE_START_DEPLOYMENT` event names its action point by: the action point in
/// PascalCase, not the SCREAMING_SNAKE key the `Actions` map is keyed by.
const PRE_START_TYPE: &str = "PreStartDeployment";

/// The `TriggeredBy` a point reached inside a caller's own request carries: "the entity that
/// triggered the deployment event" (`smithy_query(query="doc", shape="DeploymentEvent",
/// member="TriggeredBy")`), one of the values the model's `TriggeredBy` enum admits.
const TRIGGERED_BY_USER: &str = "USER";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`), and the one a finished rollout must
/// report.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];
const COMPLETE: &str = "COMPLETE";

/// The `StackStatus` the fixture stack must reach before anything is invoked.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The two halves of the sentence AppConfig refuses a gated call with when the gate's own run
/// failed, around the name of the extension whose action failed and the id of the invocation
/// that failed.
const REFUSAL_OPENING: &str = "Error invoking extension ";
const REFUSAL_MIDDLE: &str = ": Unhandled error, Lambda Request ID: ";

/// The value of the handler's own `REFUSE` variable that makes its run raise, and the value that
/// makes it return.
const RAISE: &str = "1";
const RETURN: &str = "0";

/// How long one invocation of the gate is given, as the function's own `Timeout`: well past what
/// a run needs on any host, because what is under test is what the run decides and not how fast
/// an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long any wait for work another service's clock owes goes on — the stack's provisioning, a
/// function's activation or update, a rollout, an event reaching the observation queue, a session
/// seeing what a deployment put in service — and how often it looks again.
///
/// Each deadline is a ceiling on the WAIT and nothing else: generous enough that a loaded host
/// cannot decide an outcome, because this suite runs beside every other test of the workspace on
/// the same machine. Nothing here sleeps a fixed span on another service's behalf: every wait is
/// on the CONDITION.
const SETTLED_BY: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The error codes Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries nothing the gate decided: the concurrency the process can hold at once was
/// full (`TooManyRequestsException`), or the function was not ready for an invocation yet
/// (`ResourceConflictException`). AppConfig relays the code its invocation failed with into the
/// sentence it refuses the gated call with, which is how one is recognised here.
///
/// Whether one arrives is decided by what the rest of this machine is running at that instant and
/// by nothing a row varies; no handler can produce either, so recognising exactly these two cannot
/// swallow a defect of the dispatch seam — a gate that never ran answers 201 with no event on the
/// queue, and a run that raised is reported as the failure of the FUNCTION (`Unhandled`), both of
/// which fail their leg at once.
const UNSERVED_CODES: &[&str] = &["TooManyRequestsException", "ResourceConflictException"];

/// The handler every gate runs: it forwards the WHOLE event AppConfig handed it, and the request
/// id of its own run, into the queue beside it — the only way a client observes either — and then
/// either returns or raises, as its own environment asks.
///
/// The forward happens BEFORE the raise, so the event of a release the gate goes on to veto is
/// observable too.
const GATE_HANDLER: &str = r#"
import json
import os

import boto3

QUEUES = boto3.client("sqs")


def lambda_handler(event, context):
    QUEUES.send_message(
        QueueUrl=os.environ["OBSERVATION_QUEUE_URL"],
        MessageBody=json.dumps({"Event": event, "RequestId": context.aws_request_id}),
    )
    if os.environ["REFUSE"] == "1":
        raise Exception("the extension action refuses this deployment")
    return {}
"#;

/// The stack whose role, queues and functions every row's gate is made of.
const STACK: &str = "xsvc-appconfig-prestart-gate";

/// The services this suite exercises, on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them: the gate's invocation is dispatched back into this same router, so the function its
/// `Uri` names, the queue its handler writes to and the configuration a session polls are all
/// records of this one emulator.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature. The handler's own call is
    // signed by the runtime, with the credential its execution role was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appconfigdata_appconfigdata::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The wire error code of an error response.
    fn error_code(&self) -> String {
        self.string("__type")
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_else(|| self.text())
    }
}

/// One request dispatched at the endpoint host of `service`, with `signing_name` in the credential
/// scope.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    signing_name: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .body(Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        "appconfig",
        "appconfig",
        method,
        uri,
        JSON_CONTENT_TYPE,
        &payload,
    )
    .await
}

/// One AppConfig CONTROL-plane call whose body is a configuration's own bytes, of its own media
/// type — the form `CreateHostedConfigurationVersion` binds its `@httpPayload` content and its
/// `@required` `Content-Type` header in.
async fn appconfig_content(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    dispatch(
        router,
        "appconfig",
        "appconfig",
        method,
        uri,
        content_type,
        body,
    )
    .await
}

/// One AppConfig DATA-plane call, as a polling application makes one. Both of AppConfig's APIs
/// sign as `appconfig`, whichever endpoint a client addresses.
async fn appconfigdata(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        "appconfigdata",
        "appconfig",
        method,
        uri,
        JSON_CONTENT_TYPE,
        &payload,
    )
    .await
}

/// One Lambda `restJson1` call.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        "lambda",
        "lambda",
        method,
        uri,
        JSON_CONTENT_TYPE,
        &payload,
    )
    .await
}

/// One Amazon SQS `awsJson1_0` RPC, which must be answered.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    assert_eq!(
        status,
        200,
        "setup: SQS {operation} must be answered, got {status} with {}",
        String::from_utf8_lossy(&payload)
    );
    if payload.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// One awsQuery call of `signing_name`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(&response.into_body()).to_string(),
    )
}

/// Which of the three levels an extension association binds the gate at
/// (`smithy_query(query="doc", shape="CreateExtensionAssociationRequest",
/// member="ResourceIdentifier")`: "an AppConfig application, environment, or configuration
/// profile"). Every one of them binds the gate of a deployment into that environment.
#[derive(Clone, Copy, Debug)]
enum Level {
    Application,
    Environment,
    Profile,
}

impl Level {
    /// The ARN of the resource at this level, which is what the association names.
    fn resource_arn(self, application: &str, environment: &str, profile: &str) -> String {
        let resource = match self {
            Self::Application => format!("application/{application}"),
            Self::Environment => format!("application/{application}/environment/{environment}"),
            Self::Profile => {
                format!("application/{application}/configurationprofile/{profile}")
            }
        };
        format!("arn:aws:appconfig:{REGION}:{ACCOUNT}:{resource}")
    }
}

/// One row: the configuration a release puts in service, and the level its gate is bound at.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// Which of the three levels the association binds the gate at.
    level: Level,
    /// The media type both of the row's versions are written with.
    content_type: &'static str,
    /// The bytes of version 1, which the first release puts in service.
    first: Vec<u8>,
    /// The bytes of version 2, which the vetoed release would have put in service and the
    /// restored one does.
    second: Vec<u8>,
}

/// The rows, each releasing configurations of its own through a gate bound at a level of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a JSON document, gated at the environment",
            level: Level::Environment,
            content_type: "application/json",
            first: br#"{"release":"one"}"#.to_vec(),
            second: br#"{"release":"two"}"#.to_vec(),
        },
        Row {
            case: "a text document, gated at the application",
            level: Level::Application,
            content_type: "text/plain",
            first: b"release = one\n".to_vec(),
            second: b"release = two\n".to_vec(),
        },
        Row {
            case: "bytes that are not text, gated at the configuration profile",
            level: Level::Profile,
            content_type: "application/octet-stream",
            first: vec![0x00, 0x01, 0xfe, 0xff],
            second: vec![0x7f, 0x00, 0xff, 0x01, 0x02],
        },
    ]
}

/// The name of the function one row's gate points at.
fn function_name(index: usize) -> String {
    format!("xsvc-appconfig-prestart-gate-{index}")
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The fixture stack: one execution role granting the forward every handler makes, and per row one
/// observation queue and one function running [`GATE_HANDLER`] with its `REFUSE` variable set to
/// return.
///
/// The handler is declared as the stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to exist
/// at all.
fn stack_template(rows: &[Row]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": trust_policy(),
                // A function calls another service with temporary credentials for its execution
                // role, so the role GRANTS the forward its handler makes: without it the
                // handler's own `sqs:SendMessage` is refused and no event is observable.
                "Policies": [{
                    "PolicyName": "forward-the-action-event",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["sqs:SendMessage", "logs:CreateLogGroup",
                                       "logs:CreateLogStream", "logs:PutLogEvents"],
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "RoleArn".to_string(),
        json!({"Value": {"Fn::GetAtt": ["Role", "Arn"]}}),
    );
    for (index, _) in rows.iter().enumerate() {
        let queue_logical = format!("Queue{index}");
        let function_logical = format!("Gate{index}");
        resources.insert(
            queue_logical.clone(),
            json!({ "Type": "AWS::SQS::Queue", "Properties": {} }),
        );
        resources.insert(
            function_logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {
                        "OBSERVATION_QUEUE_URL": {"Ref": queue_logical},
                        "REFUSE": RETURN,
                    }},
                    "Code": {"ZipFile": GATE_HANDLER},
                },
            }),
        );
        outputs.insert(
            format!("QueueUrl{index}"),
            json!({"Value": {"Ref": queue_logical}}),
        );
        outputs.insert(
            format!("FunctionArn{index}"),
            json!({"Value": {"Fn::GetAtt": [function_logical, "Arn"]}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// What the fixture stack published: the execution role's ARN, and per row the observation queue's
/// url and the gate function's ARN.
struct Fixture {
    role_arn: String,
    targets: Vec<(String, String)>,
}

/// Deploy the fixture stack, wait for `CREATE_COMPLETE` and answer the values it publishes.
async fn deploy(router: &MultiServiceRouter, rows: &[Row]) -> Fixture {
    let template = stack_template(rows);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK.to_string()),
            ("TemplateBody", template),
            // The template declares an IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_IAM".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: this looks until it reports it.
    let deadline = Instant::now() + SETTLED_BY;
    let described = loop {
        let (status, described) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE || Instant::now() >= deadline {
            assert_eq!(
                reported, CREATE_COMPLETE,
                "setup: the fixture stack must reach {CREATE_COMPLETE}, got {described}"
            );
            break described;
        }
        tokio::time::sleep(LOOK).await;
    };
    let published = outputs_of(&described);
    let named = |name: &str| {
        published
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must publish {name}: {published:?}"))
    };
    Fixture {
        role_arn: named("RoleArn"),
        targets: (0..rows.len())
            .map(|index| {
                (
                    named(&format!("QueueUrl{index}")),
                    named(&format!("FunctionArn{index}")),
                )
            })
            .collect(),
    }
}

/// Wait until Lambda reports `name` ready for an invocation with `REFUSE` set to `refuse`: the
/// state an invocation needs (`Active`, with no update still in flight) and the environment the
/// run under test reads. Answers the configuration it last read.
async fn wait_until_ready(router: &MultiServiceRouter, name: &str, refuse: &str) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &Value::Null,
        )
        .await;
        let reported = read.json();
        let active = reported["State"].as_str() == Some("Active");
        let settled = reported["LastUpdateStatus"]
            .as_str()
            .is_none_or(|status| status == "Successful");
        let carried = reported["Environment"]["Variables"]["REFUSE"].as_str() == Some(refuse);
        if active && settled && carried {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda must report {name:?} Active with REFUSE {refuse:?}, last reported \
             {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Set the gate's `REFUSE` variable to `refuse` and wait until the function is ready to be invoked
/// with it.
///
/// `Environment` replaces the whole variable map, so the queue the handler forwards to is restated
/// with it.
async fn set_refusing(router: &MultiServiceRouter, index: usize, queue_url: &str, refuse: &str) {
    let name = function_name(index);
    let updated = lambda(
        router,
        "PUT",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &json!({"Environment": {"Variables": {
            "OBSERVATION_QUEUE_URL": queue_url,
            "REFUSE": refuse,
        }}}),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "setup: UpdateFunctionConfiguration({name:?}) must succeed, got {} with {}",
        updated.status,
        updated.text()
    );
    wait_until_ready(router, &name, refuse).await;
}

/// One minted id out of a 201 answer.
fn minted_id(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: creating {what} must answer 201, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.string("Id").unwrap_or_else(|| {
        panic!(
            "case {case:?}: creating {what} answered no Id: {}",
            answered.text()
        )
    })
}

/// Every message waiting on the queue, parsed, each DELETED by its receipt handle as it is read —
/// so what a later read answers is what arrived since, and no message can be counted twice.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    messages
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str(body).unwrap_or_else(|error| {
                panic!("what a gate forwarded must be a JSON document ({error}): {body:?}")
            })
        })
        .collect()
}

/// The ONE thing the gate forwarded for the release of `version`: the event it was handed and the
/// request id of the run that was handed it.
///
/// A release consults the point once, so a second forward of the same version is a defect of its
/// own. The wait is on the CONDITION: the invocation is made inside the gated call, so the forward
/// is normally there by the time that call answers, and a leg that never sees one fails with the
/// deadline rather than with a sleep.
async fn forwarded(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    url: &str,
    version: &str,
) -> (Value, String) {
    let deadline = Instant::now() + SETTLED_BY;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, url).await);
        let handed: Vec<&Value> = collected
            .iter()
            .filter(|message| {
                message["Event"]["Type"] == json!(PRE_START_TYPE)
                    && message["Event"]["ContentVersion"] == json!(version)
            })
            .collect();
        if !handed.is_empty() {
            assert_eq!(
                handed.len(),
                1,
                "case {case:?} ({leg}): expected the gate to be handed the release of version \
                 {version:?} exactly once, got {collected:?}"
            );
            let request_id = handed[0]["RequestId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?} ({leg}): the gate's run must report its own request id, \
                         got {}",
                        handed[0]
                    )
                })
                .to_string();
            return (handed[0]["Event"].clone(), request_id);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?} ({leg}): the gate bound to the release must be invoked with its \
             event, got {collected:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The bytes a base64 member of an event carries, which is the encoding the event's `Content` is
/// documented to use.
fn decoded(case: &str, what: &str, carried: &Value) -> Vec<u8> {
    let text = carried.as_str().unwrap_or_default();
    ::aws_smithy_types::base64::decode(text)
        .unwrap_or_else(|error| panic!("case {case:?}: {what} must be base64 ({error}): {carried}"))
}

/// `StartDeployment`, looking again for as long as AppConfig refuses it because Lambda did not
/// serve the invocation the action point gated it on ([`UNSERVED_CODES`]) — the same call the
/// recorded scenario makes again for the same reason.
///
/// The tolerance is as narrow as the sentence allows: only a refusal of THIS gate's extension
/// whose relayed detail OPENS with one of those codes is looked at again, which is the form
/// AppConfig words a failed invocation in (`Error invoking extension <name>: <code>: <what the
/// invoking service reported>`), so a refusal that merely mentions one of them elsewhere — in a
/// message the gate's own run produced, say — is returned at once.
///
/// Every other answer is returned to the caller, which is what asserts on it. Nothing here can
/// hide what this suite pins: a release whose gate never ran at all is answered 201 with nothing
/// on the observation queue, and a refusal still standing at the deadline is returned carrying
/// what AppConfig answered. A refused release persists nothing, so the queue is drained between
/// attempts — an unserved invocation ran no handler and forwarded nothing, so that drain normally
/// reads nothing and only keeps the "one forward per release" assertion honest.
async fn released(
    router: &MultiServiceRouter,
    queue_url: &str,
    application: &str,
    environment: &str,
    extension_name: &str,
    body: &Value,
) -> Answered {
    let uri = format!("/applications/{application}/environments/{environment}/deployments");
    let unserved_openings: Vec<String> = UNSERVED_CODES
        .iter()
        .map(|code| format!("{REFUSAL_OPENING}{extension_name}: {code}"))
        .collect();
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let answered = appconfig(router, "POST", &uri, body).await;
        let refusal = answered.string("Message").unwrap_or_default();
        let unserved = answered.status == 400
            && unserved_openings
                .iter()
                .any(|opening| refusal.starts_with(opening));
        if !unserved || Instant::now() >= deadline {
            return answered;
        }
        drain(router, queue_url).await;
        tokio::time::sleep(LOOK).await;
    }
}

/// The deployment numbers the environment holds, as `ListDeployments` reports them, smallest
/// first.
async fn deployment_numbers(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> Vec<i64> {
    let listed = appconfig(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListDeployments must answer 200, got {} with {}",
        listed.status,
        listed.text()
    );
    let mut numbers: Vec<i64> = listed.json()["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["DeploymentNumber"].as_i64())
        .collect();
    numbers.sort_unstable();
    numbers
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the document it
/// was last reported with. The wait is on the CONDITION.
async fn settled_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = appconfig(
            router,
            "GET",
            &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment must answer 200, got {} with {}",
            read.status,
            read.text()
        );
        let document = read.json();
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} must reach one of {TERMINAL_STATES:?}, last \
             reported {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The configuration AppConfig's data plane serves ONE fresh client right now, as its bytes: a
/// session of its own, then the configuration that session is handed.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> Vec<u8> {
    let session = appconfigdata(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": application,
            "EnvironmentIdentifier": environment,
            "ConfigurationProfileIdentifier": profile,
        }),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must answer 201, got {} with {}",
        session.status,
        session.text()
    );
    let token = session
        .string("InitialConfigurationToken")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartConfigurationSession answered no token: {}",
                session.text()
            )
        });
    let read = appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    read.body.to_vec()
}

/// What a fresh session is served once the deployment that reported terminal has reached it: read
/// again until the bytes are `expected` or the deadline passes, then answer the last read, which is
/// what the caller asserts on.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    expected: &[u8],
) -> Vec<u8> {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let bytes = served(router, case, application, environment, profile).await;
        if bytes == expected || Instant::now() >= deadline {
            return bytes;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Bytes as a failure message reports them: the text when they are text, so a row whose document is
/// readable fails readably, and the bytes themselves otherwise.
fn shown(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => format!("{text:?}"),
        Err(_) => format!("{bytes:?}"),
    }
}

/// Write one hosted configuration version and assert the number it took.
async fn given_version(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    profile: &str,
    content_type: &str,
    body: &[u8],
    expected_number: &str,
) {
    let written = appconfig_content(
        router,
        "POST",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}\
             /hostedconfigurationversions"
        ),
        content_type,
        body,
    )
    .await;
    assert_eq!(
        written.status,
        201,
        "case {case:?}: seeding a version must answer 201, got {} with {}",
        written.status,
        written.text()
    );
    assert_eq!(
        written.header("Version-Number").as_deref(),
        Some(expected_number),
        "case {case:?}: the seeded version must take number {expected_number}, got {:?}",
        written.header("Version-Number")
    );
}

/// The release one leg drives, as the event the gate is handed must describe it.
struct Release<'a> {
    application: &'a str,
    environment: &'a str,
    profile: &'a str,
    /// The `ConfigurationVersion` the request named, which the event carries as a string.
    version: &'a str,
    /// The candidate configuration's exact bytes, which the event carries base64-encoded.
    content: &'a [u8],
}

/// Assert the event the gate was handed for one release: the action point in PascalCase, the three
/// resources the deployment runs on, the version as a STRING, the candidate configuration
/// base64-encoded, and the caller as the entity that triggered it.
fn assert_event(case: &str, leg: &str, event: &Value, release: &Release<'_>) {
    let Release {
        application,
        environment,
        profile,
        version,
        content,
    } = *release;
    assert_eq!(
        event["Type"],
        json!(PRE_START_TYPE),
        "case {case:?} ({leg}): the event must name the action point in PascalCase, got {event}"
    );
    assert_eq!(
        event["Application"]["Id"],
        json!(application),
        "case {case:?} ({leg}): the event must name the application the release runs on, got \
         {event}"
    );
    assert_eq!(
        event["Environment"]["Id"],
        json!(environment),
        "case {case:?} ({leg}): the event must name the environment the release runs on, got \
         {event}"
    );
    assert_eq!(
        event["ConfigurationProfile"]["Id"],
        json!(profile),
        "case {case:?} ({leg}): the event must name the profile the release runs on, got {event}"
    );
    assert_eq!(
        event["ContentVersion"],
        json!(version),
        "case {case:?} ({leg}): the event must carry ContentVersion as a string, got {event}"
    );
    assert_eq!(
        event["TriggeredBy"],
        json!(TRIGGERED_BY_USER),
        "case {case:?} ({leg}): a point reached inside the caller's own request is the caller's, \
         got {event}"
    );
    let carried = decoded(case, "the event's Content", &event["Content"]);
    assert_eq!(
        carried,
        content,
        "case {case:?} ({leg}): the event must carry the candidate configuration's exact bytes, \
         expected {}, got {}",
        shown(content),
        shown(&carried)
    );
}

/// RULE: a `PRE_START_DEPLOYMENT` action of an extension associated to a resource a deployment
/// runs on is invoked synchronously with that release's event before any deployment record
/// exists, and the call is gated on what the run answers: a run that returns lets the deployment
/// be created and reach `COMPLETE` with its version in service, while a run that raised refuses
/// `StartDeployment` with `BadRequestException` / HTTP 400 naming the extension and the failing
/// invocation, adds no deployment record, and leaves the version already in service untouched —
/// after which the identical call, with the target restored, is accepted.
#[tokio::test]
async fn a_pre_start_deployment_gate_runs_its_function_and_its_veto_leaves_no_record() {
    let router = build_router();
    let rows = rows();
    let fixture = deploy(&router, &rows).await;
    for index in 0..rows.len() {
        wait_until_ready(&router, &function_name(index), RETURN).await;
    }

    // The strategy every release here is paced by is deliberately NON-INSTANT: it declares a
    // rollout window and reaches every target in one growth step, so a release of it is work the
    // service's own clock owes rather than something finished before its start answers.
    let strategy = minted_id(
        "setup",
        "the deployment strategy",
        &appconfig(
            &router,
            "POST",
            "/deploymentstrategies",
            &json!({
                "Name": "xsvc-appconfig-prestart-paced",
                "DeploymentDurationInMinutes": 1,
                "FinalBakeTimeInMinutes": 0,
                "GrowthFactor": 100.0,
                "GrowthType": "LINEAR",
                "ReplicateTo": "NONE",
            }),
        )
        .await,
    );

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (queue_url, function_arn) = &fixture.targets[index];
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                "POST",
                "/applications",
                &json!({"Name": format!("xsvc-prestart-app-{index}")}),
            )
            .await,
        );
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("xsvc-prestart-env-{index}")}),
            )
            .await,
        );
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &json!({
                    "Name": format!("xsvc-prestart-profile-{index}"),
                    "LocationUri": HOSTED,
                }),
            )
            .await,
        );
        given_version(
            &router,
            case,
            &application,
            &profile,
            row.content_type,
            &row.first,
            "1",
        )
        .await;
        given_version(
            &router,
            case,
            &application,
            &profile,
            row.content_type,
            &row.second,
            "2",
        )
        .await;

        // The gate: one extension binding one action at the point AppConfig consults before it
        // starts a deployment, pointing at this row's function and naming the role the action is
        // invoked through.
        let extension_name = format!("xsvc-prestart-gate-extension-{index}");
        let created = appconfig(
            &router,
            "POST",
            "/extensions",
            &json!({
                "Name": extension_name,
                "Actions": {
                    "PRE_START_DEPLOYMENT": [{
                        "Name": "gate",
                        "Uri": function_arn,
                        "RoleArn": fixture.role_arn,
                    }],
                },
            }),
        )
        .await;
        let extension = minted_id(case, "the extension", &created);
        let document = created.json();
        assert_eq!(
            document["VersionNumber"],
            json!(1),
            "case {case:?}: the first version of an extension is 1, got {document}"
        );
        assert_eq!(
            document["Actions"]["PRE_START_DEPLOYMENT"][0]["Uri"],
            json!(function_arn),
            "case {case:?}: the action must round-trip the target it names, got {document}"
        );
        let resource_arn = row
            .level
            .resource_arn(&application, &environment, &profile);
        let associated = appconfig(
            &router,
            "POST",
            "/extensionassociations",
            &json!({
                "ExtensionIdentifier": extension,
                "ResourceIdentifier": resource_arn,
            }),
        )
        .await;
        assert_eq!(
            associated.status,
            201,
            "case {case:?}: CreateExtensionAssociation must answer 201, got {} with {}",
            associated.status,
            associated.text()
        );
        assert_eq!(
            associated.string("ResourceArn").as_deref(),
            Some(resource_arn.as_str()),
            "case {case:?}: the association must report the resource it was bound to, got {}",
            associated.text()
        );

        // LEG 1: a gate whose run returns lets the release through.
        let first_release = json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": "1",
        });
        let started = released(
            &router,
            queue_url,
            &application,
            &environment,
            &extension_name,
            &first_release,
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} with {}",
            started.status,
            started.text()
        );
        let (event, _) = forwarded(&router, case, "the first release", queue_url, "1").await;
        assert_event(
            case,
            "the first release",
            &event,
            &Release {
                application: &application,
                environment: &environment,
                profile: &profile,
                version: "1",
                content: &row.first,
            },
        );
        let settled = settled_deployment(&router, case, &application, &environment, 1).await;
        assert_eq!(
            settled["State"],
            json!(COMPLETE),
            "case {case:?}: a release the gate let through reports the modelled {COMPLETE}, got \
             {settled}"
        );
        let in_service = serving(
            &router,
            case,
            &application,
            &environment,
            &profile,
            &row.first,
        )
        .await;
        assert_eq!(
            in_service,
            row.first,
            "case {case:?}: the data plane must serve the released version's bytes, expected {}, \
             got {}",
            shown(&row.first),
            shown(&in_service)
        );

        // LEG 2: the SAME gate, with its run raising, vetoes the next release.
        set_refusing(&router, index, queue_url, RAISE).await;
        let second_release = json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": "2",
        });
        let refused = released(
            &router,
            queue_url,
            &application,
            &environment,
            &extension_name,
            &second_release,
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: a release the gate vetoed must be refused with the modelled 400, got \
             {} with {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code().split('#').next_back(),
            Some("BadRequestException"),
            "case {case:?}: the veto must be refused as BadRequestException, got {}",
            refused.error_code()
        );
        // The gate still SAW the release it vetoed, and the refusal names the invocation that
        // failed: the request id the run reported to itself.
        let (vetoed_event, request_id) =
            forwarded(&router, case, "the vetoed release", queue_url, "2").await;
        assert_event(
            case,
            "the vetoed release",
            &vetoed_event,
            &Release {
                application: &application,
                environment: &environment,
                profile: &profile,
                version: "2",
                content: &row.second,
            },
        );
        let expected_message =
            format!("{REFUSAL_OPENING}{extension_name}{REFUSAL_MIDDLE}{request_id}");
        assert_eq!(
            refused.string("Message").as_deref(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            refused.string("Message")
        );
        // And nothing a client can see changed: the deployment sequence is the one the accepted
        // release left, and the configuration in service is still version 1's.
        let numbers = deployment_numbers(&router, case, &application, &environment).await;
        assert_eq!(
            numbers,
            vec![1],
            "case {case:?}: a vetoed release must leave no deployment record, got {numbers:?}"
        );
        let still = served(&router, case, &application, &environment, &profile).await;
        assert_eq!(
            still,
            row.first,
            "case {case:?}: a vetoed release leaves the version in service untouched, expected \
             {}, got {}",
            shown(&row.first),
            shown(&still)
        );

        // LEG 3: the target restored, the identical call is accepted and its version reaches the
        // data plane.
        set_refusing(&router, index, queue_url, RETURN).await;
        let retried = released(
            &router,
            queue_url,
            &application,
            &environment,
            &extension_name,
            &second_release,
        )
        .await;
        assert_eq!(
            retried.status,
            201,
            "case {case:?}: the identical release must be accepted once the gate returns, got {} \
             with {}",
            retried.status,
            retried.text()
        );
        assert_eq!(
            retried.json()["DeploymentNumber"],
            json!(2),
            "case {case:?}: the accepted release takes the NEXT number of the sequence, got {}",
            retried.text()
        );
        let (retried_event, _) =
            forwarded(&router, case, "the restored release", queue_url, "2").await;
        assert_event(
            case,
            "the restored release",
            &retried_event,
            &Release {
                application: &application,
                environment: &environment,
                profile: &profile,
                version: "2",
                content: &row.second,
            },
        );
        let settled = settled_deployment(&router, case, &application, &environment, 2).await;
        assert_eq!(
            settled["State"],
            json!(COMPLETE),
            "case {case:?}: the accepted release reports the modelled {COMPLETE}, got {settled}"
        );
        let numbers = deployment_numbers(&router, case, &application, &environment).await;
        assert_eq!(
            numbers,
            vec![1, 2],
            "case {case:?}: the accepted release is the only record the veto's retry added, got \
             {numbers:?}"
        );
        let in_service = serving(
            &router,
            case,
            &application,
            &environment,
            &profile,
            &row.second,
        )
        .await;
        assert_eq!(
            in_service,
            row.second,
            "case {case:?}: the data plane must serve the newly released bytes, expected {}, got \
             {}",
            shown(&row.second),
            shown(&in_service)
        );
    }
}
