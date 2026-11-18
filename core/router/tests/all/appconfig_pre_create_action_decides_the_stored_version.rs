// SPDX-License-Identifier: CC0-1.0

//! An extension action AppConfig consults BEFORE it creates a hosted configuration version
//! decides the bytes that version holds: the action is invoked synchronously with the submitted
//! document, and what it answers is what AppConfig stores, reads back and later serves to a
//! running application.
//!
//! RULE: when a configuration profile has an extension associated to it whose action is bound at
//! `PRE_CREATE_HOSTED_CONFIGURATION_VERSION`, `CreateHostedConfigurationVersion` invokes that
//! action's target with an event naming the action point in PascalCase, the resources the write
//! runs on, the submitted `Content` base64-encoded and the `ContentVersion` as a string; a target
//! that answers `{"Content": "<base64>"}` replaces the bytes the version is created with — so the
//! version read back, and the configuration the data plane serves once a deployment of it
//! completes, are the action's bytes and not the caller's — while a target that answers no
//! `Content` leaves the submitted bytes exactly as they were. A deployment that finishes reports
//! `Deployment$State` as the modelled enum value `COMPLETE`
//! (`smithy_query(query="shape", shape="DeploymentState")`), which is what a client polling for a
//! terminal state matches on.
//!
//! Only a cross-service suite can state that end to end: the action's `Uri` names a Lambda
//! function — the one target kind that can ANSWER an action point — the function's handler
//! forwards the event it was handed into an Amazon SQS queue (no AppConfig API reports that
//! event, so the queue is where a client observes it) and runs as an execution role IAM holds,
//! CloudFormation is what archives a handler written out here into a deployment package, AppConfig
//! holds the profile and the deployment, and what is in service is read where an application reads
//! it, out of AppConfig's own data plane.
//!
//! What the neighbours pin and this one does not repeat: `appconfig_gated_release_decides_what_is_in_service`
//! follows a REFUSED gate through to the deployment sequence, the AppConfig crate's
//! `extension_action_points` pins the event each binding level is handed against a queue target,
//! and `lambda_appconfig_reader_on_every_python_runtime` drives a handler READING the data plane.
//! None of them has an action's answer decide what AppConfig stores.
//!
//! The rows differ along what the rule generalises over: how the action derives its replacement
//! (a stamp added to the submitted JSON document, the submitted bytes rewritten, bytes that do not
//! derive from the submitted document at all), the media type and size of the configuration,
//! whether the bytes are text at all, and whether the action answers a replacement at all. One row
//! also writes a SECOND version, where what the action is told about the version already there is
//! the bytes IT replaced them with: the answer of one invocation is the state the next one reads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the hierarchy, the role, the
/// queue and the function belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the profile ARN an association names carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `LocationUri` that selects AppConfig's own hosted configuration store, the only store a
/// hosted configuration version is written to.
const HOSTED: &str = "hosted";

/// The media type a JSON control-plane body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `Type` a `PRE_CREATE_HOSTED_CONFIGURATION_VERSION` event names its action point by: the
/// action point in PascalCase, not the SCREAMING_SNAKE key the `Actions` map is keyed by.
const PRE_CREATE_TYPE: &str = "PreCreateHostedConfigurationVersion";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`), one of which every deployment here
/// ends in — and `COMPLETE` is the one a finished rollout must report.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];
const COMPLETE: &str = "COMPLETE";

/// The `StackStatus` the fixture stack must reach before anything is invoked.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation of the action's target is given, as the function's own `Timeout`: well
/// past what a run needs on any host, because what is under test is what the action answers and
/// not how fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long any wait for work another service's clock owes goes on — the stack's provisioning, a
/// function's activation, a deployment's rollout, a session seeing what a deployment put in
/// service — and how often it looks again.
///
/// Each deadline is a ceiling on the WAIT and nothing else: generous enough that a loaded host
/// cannot decide an outcome, because this suite runs beside every other test of the workspace on
/// the same machine. Nothing here sleeps a fixed span on another service's behalf: every wait is
/// on the CONDITION.
const SETTLED_BY: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The handler every action target runs: it forwards the WHOLE event AppConfig handed it into the
/// queue beside it — the only way a client observes that event — and then answers the replacement
/// its own environment asks for, derived from the `Content` the event carried, so an answer here
/// is evidence the action really saw the submitted document.
const TRANSFORM_HANDLER: &str = r#"
import base64
import json
import os

import boto3

QUEUES = boto3.client("sqs")


def lambda_handler(event, context):
    QUEUES.send_message(
        QueueUrl=os.environ["OBSERVATION_QUEUE_URL"],
        MessageBody=json.dumps(event),
    )
    submitted = base64.b64decode(event["Content"])
    transform = os.environ["TRANSFORM"]
    if transform == "none":
        return {}
    if transform == "stamp":
        document = json.loads(submitted.decode("utf-8"))
        document["stamped"] = os.environ["STAMP"]
        replacement = json.dumps(
            document, separators=(",", ":"), sort_keys=True
        ).encode("utf-8")
    elif transform == "rewrite":
        replacement = submitted[::-1]
    else:
        replacement = base64.b64decode(os.environ["FIXED"])
    return {"Content": base64.b64encode(replacement).decode("ascii")}
"#;

/// The services this suite exercises, on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them: an action's invocation is dispatched back into this same router, so the function its
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

/// How the action a row binds derives the replacement it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transform {
    /// It parses the submitted JSON document and answers it with one member added.
    Stamp,
    /// It answers the submitted bytes rewritten, whatever they are.
    Rewrite,
    /// It answers bytes of its own, which the submitted document decides nothing about.
    Fixed,
    /// It answers no `Content` at all, so the submitted bytes stand.
    None,
}

impl Transform {
    /// The token the handler reads out of its own environment.
    fn token(self) -> &'static str {
        match self {
            Self::Stamp => "stamp",
            Self::Rewrite => "rewrite",
            Self::Fixed => "fixed",
            Self::None => "none",
        }
    }
}

/// One row: the document a caller submits, what the action bound at the profile answers for it,
/// and the bytes AppConfig must therefore store and serve.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The media type the document is written with, which the event also carries.
    content_type: &'static str,
    /// The bytes the caller submits.
    submitted: Vec<u8>,
    /// How the action derives its answer.
    transform: Transform,
    /// The value a `Stamp` action adds, as its environment carries it.
    stamp: &'static str,
    /// The bytes a `Fixed` action answers.
    fixed: Vec<u8>,
    /// The bytes the version must hold: the action's answer, or the submitted bytes when it
    /// answers none.
    expected: Vec<u8>,
    /// A SECOND write to the same profile, where the row also reads what the action is told
    /// about the version it already replaced: the document submitted, and the bytes the
    /// action's answer makes of it.
    again: Option<(Vec<u8>, Vec<u8>)>,
}

/// The padding of the row whose document is larger than one base64 line, written once so the
/// submitted document and the expected replacement cannot disagree about it.
fn padding() -> String {
    "p".repeat(300)
}

/// The rows, each with the bytes its action's answer makes of its document. Every expected
/// document is written out here rather than computed through the same code the handler runs, so a
/// transform that did not happen cannot satisfy a row.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a JSON document the action stamps",
            content_type: "application/json",
            submitted: br#"{"colour":"blue"}"#.to_vec(),
            transform: Transform::Stamp,
            stamp: "release-7",
            fixed: Vec::new(),
            expected: br#"{"colour":"blue","stamped":"release-7"}"#.to_vec(),
            again: Some((
                br#"{"colour":"green"}"#.to_vec(),
                br#"{"colour":"green","stamped":"release-7"}"#.to_vec(),
            )),
        },
        Row {
            case: "a text document the action rewrites",
            content_type: "text/plain",
            submitted: b"a=1\nb=2\n".to_vec(),
            transform: Transform::Rewrite,
            stamp: "",
            fixed: Vec::new(),
            expected: b"\n2=b\n1=a".to_vec(),
            again: None,
        },
        Row {
            case: "bytes that are not text, replaced by bytes of the action's own",
            content_type: "application/octet-stream",
            submitted: vec![0x00, 0x01, 0xfe, 0xff, 0x7f],
            transform: Transform::Fixed,
            stamp: "",
            fixed: vec![0x7f, 0x00, 0xff, 0x01, 0x02, 0x03],
            expected: vec![0x7f, 0x00, 0xff, 0x01, 0x02, 0x03],
            again: None,
        },
        Row {
            case: "a document larger than one base64 line, stamped",
            content_type: "application/json",
            submitted: format!(r#"{{"padding":"{}"}}"#, padding()).into_bytes(),
            transform: Transform::Stamp,
            stamp: "release-9",
            fixed: Vec::new(),
            expected: format!(
                r#"{{"padding":"{}","stamped":"release-9"}}"#,
                padding()
            )
            .into_bytes(),
            again: None,
        },
        Row {
            case: "an action that answers no Content leaves the submitted bytes",
            content_type: "application/json",
            submitted: br#"{"kept":true}"#.to_vec(),
            transform: Transform::None,
            stamp: "",
            fixed: Vec::new(),
            expected: br#"{"kept":true}"#.to_vec(),
            again: None,
        },
    ]
}

/// The name of the function one row's action points at.
fn function_name(index: usize) -> String {
    format!("xsvc-appconfig-transform-{index}")
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
/// observation queue and one function running [`TRANSFORM_HANDLER`] under that row's environment.
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
    for (index, row) in rows.iter().enumerate() {
        let queue_logical = format!("Queue{index}");
        let function_logical = format!("Transform{index}");
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
                        "TRANSFORM": row.transform.token(),
                        "STAMP": row.stamp,
                        "FIXED": ::aws_smithy_types::base64::encode(&row.fixed),
                    }},
                    "Code": {"ZipFile": TRANSFORM_HANDLER},
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

/// Deploy the fixture stack, wait for `CREATE_COMPLETE` and answer the values it publishes.
async fn deploy(router: &MultiServiceRouter, stack: &str, rows: &[Row]) -> Vec<(String, String)> {
    let template = stack_template(rows);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack.to_string()),
            ("TemplateBody", template),
            // The template declares an IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_IAM".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
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
            &[("StackName", stack.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({stack:?}) must succeed, got {status} with {described}"
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
    (0..rows.len())
        .map(|index| {
            let queue = published
                .get(&format!("QueueUrl{index}"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!("setup: the stack must publish row {index}'s queue url: {published:?}")
                });
            let function = published
                .get(&format!("FunctionArn{index}"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!("setup: the stack must publish row {index}'s function ARN: {published:?}")
                });
            (queue, function)
        })
        .collect()
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs: a function
/// still provisioning would refuse the action's invocation, which is a different rule.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
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
        if reported["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda must report {name:?} Active, last reported {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
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
    answered
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: creating {what} answered no Id: {}", answered.text()))
}

/// Every event waiting on the queue, parsed, each DELETED by its receipt handle as it is read — so
/// what a later read answers is what arrived since, and no event can be counted twice.
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
                panic!("an action-point event must be a JSON document ({error}): {body:?}")
            })
        })
        .collect()
}

/// Read the queue until the event of the action point has arrived, and answer every event read.
///
/// The wait is on the CONDITION: the invocation is made inside the write, so the event is normally
/// there by the time the write answers, and a row that never sees one fails with the deadline
/// rather than with a sleep.
async fn handed_events(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + SETTLED_BY;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, url).await);
        if collected
            .iter()
            .any(|event| event["Type"] == json!(PRE_CREATE_TYPE))
        {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the action bound at the profile must be handed the write's event, got \
             {collected:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The error codes Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries nothing the target decided: the concurrency the process can hold at once was
/// full (`TooManyRequestsException`), or the function was not `Active` yet
/// (`ResourceConflictException`). AppConfig relays the code its invocation failed with into the
/// sentence it refuses the gated call with, which is how one is recognised here.
///
/// Whether one arrives is decided by what the rest of this machine is running at that instant and
/// by nothing a row varies; no handler can produce either, so recognising exactly these two cannot
/// swallow a defect of the invocation seam — an event the target rejected, or an answer AppConfig
/// mis-read, is reported as the failure of the FUNCTION (`Unhandled`) and fails the row at once.
const UNSERVED_CODES: &[&str] = &["TooManyRequestsException", "ResourceConflictException"];

/// Write one hosted configuration version, looking again for as long as AppConfig refuses the write
/// because Lambda did not serve the invocation the action point gated it on ([`UNSERVED_CODES`]) —
/// the same call the recorded scenario makes again for the same reason.
///
/// Every other answer is returned to the caller, which is what asserts on it. Nothing here can hide
/// the defect this suite pins: a write whose action never ran at all is answered 201 with the
/// CALLER's own bytes, which every row asserts against, and a refusal still standing at the
/// deadline fails the row carrying what AppConfig answered. A refused write persists nothing, so
/// the queue is drained between attempts — an unserved invocation delivered no event, so that drain
/// normally reads nothing and only keeps the "one event per write" assertion honest.
async fn written_version(
    router: &MultiServiceRouter,
    queue_url: &str,
    application: &str,
    profile: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    let uri = format!(
        "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions"
    );
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let answered = appconfig_content(router, "POST", &uri, content_type, body).await;
        let refusal = answered.string("Message").unwrap_or_default();
        let unserved = answered.status == 400
            && UNSERVED_CODES.iter().any(|code| refusal.contains(code));
        if !unserved || Instant::now() >= deadline {
            return answered;
        }
        drain(router, queue_url).await;
        tokio::time::sleep(LOOK).await;
    }
}

/// The ONE event of the action point a write handed its target, read off the observation queue: a
/// write consults the point once, so a second event is a defect of its own.
async fn only_event(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let events = handed_events(router, case, url).await;
    let handed: Vec<&Value> = events
        .iter()
        .filter(|event| event["Type"] == json!(PRE_CREATE_TYPE))
        .collect();
    assert_eq!(
        handed.len(),
        1,
        "case {case:?}: expected exactly one event of the action point, got {events:?}"
    );
    handed[0].clone()
}

/// The bytes a base64 member of an event carries, which is the encoding the event's `Content` and
/// its `PreviousContent$Content` are documented to use.
fn decoded(case: &str, what: &str, carried: &Value) -> Vec<u8> {
    let text = carried.as_str().unwrap_or_default();
    ::aws_smithy_types::base64::decode(text)
        .unwrap_or_else(|error| panic!("case {case:?}: {what} must be base64 ({error}): {carried}"))
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

/// RULE: a `PRE_CREATE_HOSTED_CONFIGURATION_VERSION` action associated to a configuration profile
/// is invoked synchronously with the submitted document, and the `Content` it answers is what
/// AppConfig stores — read back through `GetHostedConfigurationVersion` and served to every fresh
/// session once a deployment of that version reports the modelled `COMPLETE` — while an action
/// answering no `Content` leaves the submitted bytes untouched.
#[tokio::test]
async fn a_pre_create_action_answer_is_the_version_that_is_stored_and_served() {
    let router = build_router();
    let rows = rows();
    let targets = deploy(&router, "xsvc-appconfig-transform", &rows).await;
    for index in 0..rows.len() {
        wait_until_active(&router, &function_name(index)).await;
    }

    // One strategy for every row: it reaches every target in one step and bakes for no time, so a
    // deployment of it is over as soon as the service's own clock says it is.
    let strategy = minted_id(
        "setup",
        "the deployment strategy",
        &appconfig(
            &router,
            "POST",
            "/deploymentstrategies",
            &json!({
                "Name": "xsvc-appconfig-transform-instant",
                "DeploymentDurationInMinutes": 0,
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
        let (queue_url, function_arn) = &targets[index];
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                "POST",
                "/applications",
                &json!({"Name": format!("xsvc-transform-app-{index}")}),
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
                &json!({"Name": format!("xsvc-transform-env-{index}")}),
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
                    "Name": format!("xsvc-transform-profile-{index}"),
                    "LocationUri": HOSTED,
                }),
            )
            .await,
        );
        let extension = minted_id(
            case,
            "the extension",
            &appconfig(
                &router,
                "POST",
                "/extensions",
                &json!({
                    "Name": format!("xsvc-transform-extension-{index}"),
                    "Actions": {
                        "PRE_CREATE_HOSTED_CONFIGURATION_VERSION": [{
                            "Name": "transform",
                            "Uri": function_arn,
                        }],
                    },
                }),
            )
            .await,
        );
        let associated = appconfig(
            &router,
            "POST",
            "/extensionassociations",
            &json!({
                "ExtensionIdentifier": extension,
                "ResourceIdentifier": format!(
                    "arn:aws:appconfig:{REGION}:{ACCOUNT}:application/{application}/\
                     configurationprofile/{profile}"
                ),
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

        // THE GRADED CALL: the write the action point gates.
        let written = written_version(
            &router,
            queue_url,
            &application,
            &profile,
            row.content_type,
            &row.submitted,
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: CreateHostedConfigurationVersion must answer 201, got {} with {}",
            written.status,
            written.text()
        );
        assert_eq!(
            written.header("Version-Number").as_deref(),
            Some("1"),
            "case {case:?}: the write must answer the version it created, got {:?}",
            written.header("Version-Number")
        );
        assert_eq!(
            written.body.to_vec(),
            row.expected,
            "case {case:?}: the write must answer the bytes it stored, expected {}, got {}",
            shown(&row.expected),
            shown(&written.body)
        );

        // The action really was consulted, and was handed the SUBMITTED document: one event of
        // this write's action point, naming the point in PascalCase, the resources the write runs
        // on, the version as a string and the caller's own bytes.
        let event = only_event(&router, case, queue_url).await;
        assert_eq!(
            event["ContentType"],
            json!(row.content_type),
            "case {case:?}: the event must carry the write's ContentType, got {event}"
        );
        assert_eq!(
            event["ContentVersion"],
            json!("1"),
            "case {case:?}: the event must carry ContentVersion as a string, got {event}"
        );
        assert_eq!(
            event["Application"]["Id"],
            json!(application),
            "case {case:?}: the event must name the application the write runs on, got {event}"
        );
        assert_eq!(
            event["ConfigurationProfile"]["Id"],
            json!(profile),
            "case {case:?}: the event must name the profile the write runs on, got {event}"
        );
        assert!(
            event.get("PreviousContent").is_none(),
            "case {case:?}: the first version of a profile has no previously stored version, got \
             {event}"
        );
        let carried = decoded(case, "the event's Content", &event["Content"]);
        assert_eq!(
            carried,
            row.submitted,
            "case {case:?}: the event must carry the submitted bytes, expected {}, got {}",
            shown(&row.submitted),
            shown(&carried)
        );

        // The STORED version is the action's answer, read back where a client reads it.
        let held = appconfig_content(
            &router,
            "GET",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions/1"
            ),
            JSON_CONTENT_TYPE,
            &[],
        )
        .await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: GetHostedConfigurationVersion must answer 200, got {} with {}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.body.to_vec(),
            row.expected,
            "case {case:?}: the stored version must hold the action's bytes, expected {}, got {}",
            shown(&row.expected),
            shown(&held.body)
        );
        assert_eq!(
            held.header("Content-Type").as_deref(),
            Some(row.content_type),
            "case {case:?}: the stored version keeps the write's ContentType, got {:?}",
            held.header("Content-Type")
        );

        // And the deployment of that version settles on the modelled terminal state, which is what
        // a client polling for one matches on.
        let started = appconfig(
            &router,
            "POST",
            &format!("/applications/{application}/environments/{environment}/deployments"),
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment must answer 201, got {} with {}",
            started.status,
            started.text()
        );
        let settled = settled_deployment(&router, case, &application, &environment, 1).await;
        assert_eq!(
            settled["State"],
            json!(COMPLETE),
            "case {case:?}: a finished deployment reports the modelled {COMPLETE}, got {settled}"
        );

        // What a running application is served is the action's bytes, byte for byte. `serving`
        // only decides WHEN to stop looking — it answers the last read either way, and the
        // assertion below is what passes or fails the row.
        let in_service = serving(
            &router,
            case,
            &application,
            &environment,
            &profile,
            &row.expected,
        )
        .await;
        assert_eq!(
            in_service,
            row.expected,
            "case {case:?}: the data plane must serve the action's bytes, expected {}, got {}",
            shown(&row.expected),
            shown(&in_service)
        );

        // A SECOND write to the same profile: what the action is told about the version already
        // there is the bytes IT replaced them with, so the answer of one invocation is the state
        // the next one reads — and the new version is the new answer.
        let Some((submitted_again, expected_again)) = &row.again else {
            continue;
        };
        let rewritten = written_version(
            &router,
            queue_url,
            &application,
            &profile,
            row.content_type,
            submitted_again,
        )
        .await;
        assert_eq!(
            rewritten.status,
            201,
            "case {case:?}: the second CreateHostedConfigurationVersion must answer 201, got {} \
             with {}",
            rewritten.status,
            rewritten.text()
        );
        assert_eq!(
            rewritten.header("Version-Number").as_deref(),
            Some("2"),
            "case {case:?}: the second write takes the next version number, got {:?}",
            rewritten.header("Version-Number")
        );
        assert_eq!(
            rewritten.body.to_vec(),
            *expected_again,
            "case {case:?}: the second write must answer the bytes it stored, expected {}, got {}",
            shown(expected_again),
            shown(&rewritten.body)
        );
        let event = only_event(&router, case, queue_url).await;
        assert_eq!(
            event["ContentVersion"],
            json!("2"),
            "case {case:?}: the second event must carry the version it is about, got {event}"
        );
        let carried = decoded(case, "the second event's Content", &event["Content"]);
        assert_eq!(
            carried,
            *submitted_again,
            "case {case:?}: the second event must carry the second submitted document, expected \
             {}, got {}",
            shown(submitted_again),
            shown(&carried)
        );
        let previous = &event["PreviousContent"];
        assert_eq!(
            previous["ContentVersion"],
            json!("1"),
            "case {case:?}: PreviousContent must name the version that was latest, got {event}"
        );
        assert_eq!(
            previous["ContentType"],
            json!(row.content_type),
            "case {case:?}: PreviousContent must carry that version's ContentType, got {event}"
        );
        let carried = decoded(case, "PreviousContent's Content", &previous["Content"]);
        assert_eq!(
            carried,
            row.expected,
            "case {case:?}: PreviousContent must carry the bytes the first action STORED, \
             expected {}, got {}",
            shown(&row.expected),
            shown(&carried)
        );
        let held = appconfig_content(
            &router,
            "GET",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions/2"
            ),
            JSON_CONTENT_TYPE,
            &[],
        )
        .await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: GetHostedConfigurationVersion(2) must answer 200, got {} with {}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.body.to_vec(),
            *expected_again,
            "case {case:?}: version 2 must hold the second action's bytes, expected {}, got {}",
            shown(expected_again),
            shown(&held.body)
        );
    }
}
