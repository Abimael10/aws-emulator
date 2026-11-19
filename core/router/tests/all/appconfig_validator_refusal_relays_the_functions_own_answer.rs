// SPDX-License-Identifier: CC0-1.0

//! What a configuration profile's `LAMBDA` validator REPORTED is what AppConfig tells the
//! caller it refused for: the refusal of a gated call relays the invocation's own answer, so a
//! client reads the validator's own diagnostic and not a sentence AppConfig wrote about it.
//!
//! RULE: a profile's validators are the gate a configuration has to pass ("the configuration
//! deployment or update can only proceed when the configuration data is valid",
//! `smithy_query(query="doc", shape="Validator")`), and a `LAMBDA` validator gives its verdict
//! by failing: the function is invoked synchronously with the candidate configuration, and when
//! its run reports an error the gated call is refused `BadRequestException` / HTTP 400
//! (`smithy_query(query="operation", operation="StartDeployment,ValidateConfiguration")`)
//! carrying what the INVOCATION reported — the `FunctionError`, the run's `LogResult` and the
//! error document the function's own runtime wrote, relayed verbatim rather than summarised —
//! while a function that reports no error lets the call through. A refused deployment persists
//! nothing, so the environment's deployment history stays empty.
//!
//! Only a cross-service suite can state that: the validator's `Content` names a Lambda function,
//! the function's runtime is what writes the error document being relayed, CloudFormation is what
//! archives a handler written out here into a deployment package, IAM holds its execution role,
//! and AppConfig holds the profile, the version and the deployment. The expected sentence is never
//! written out here: it is assembled from the answer the SAME function gives when a client invokes
//! it directly through Lambda's own API, so a relay that re-words, re-serializes or drops part of
//! that answer fails these rows.
//!
//! What the neighbours pin and this one does not repeat:
//! `appconfig_gated_release_decides_what_is_in_service` follows a refused extension ACTION point
//! through to the deployment sequence (a different seam, with its own wording),
//! `appconfig_pre_create_action_decides_the_stored_version` has an action's answer decide what is
//! stored, and the AppConfig crate's own `lambda_validator_relayed_verdict` reads the members of the
//! relayed error document out of the refusal for a function the crate's own harness holds. What is
//! only reachable here is the comparison itself: the function is one CloudFormation archived and
//! Lambda runs under a role IAM holds, and the expected sentence is the answer THAT function gives a
//! client invoking it directly, so the relay is checked against an independent invocation of the
//! same code rather than against members a suite names.
//!
//! The rows differ along what the rule generalises over: the exception class the validator raises
//! (a built-in, another built-in, and one the handler declares itself), the sentence it raises with
//! (plain text, and text carrying the quotes, braces and non-ASCII characters a JSON document's
//! escaping has to survive), the document stored under the profile, and whether the validator finds
//! that document compliant at all. Every rejecting row is driven through BOTH operations the gate
//! refuses.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the hierarchy, the role and the
/// functions belong to.
const REGION: &str = "us-east-1";

/// The `LocationUri` that selects AppConfig's own hosted configuration store, the only store a
/// hosted configuration version is written to.
const HOSTED: &str = "hosted";

/// The media type a JSON control-plane body travels with, and the one every document here is
/// written with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The modelled refusal of a call a validator did not let through (`BadRequestException`, HTTP 400
/// — declared by both operations this suite drives).
const BAD_REQUEST: &str = "BadRequestException";
const BAD_REQUEST_STATUS: u16 = 400;

/// The `StackStatus` the fixture stack must reach before anything is invoked.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` Lambda reports for a function ready to be invoked.
const FUNCTION_ACTIVE: &str = "Active";

/// How long one invocation of a validator is given, as the function's own `Timeout`: well past what
/// a run needs on any host, because what is under test is what the run REPORTS and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long any wait for work another service's clock owes goes on — the stack's provisioning, a
/// function's activation — and how often it looks again.
///
/// Each deadline is a ceiling on the WAIT and nothing else: generous enough that a loaded host
/// cannot decide an outcome, because this suite runs beside every other test of the workspace on
/// the same machine. Nothing here sleeps a fixed span on another service's behalf: every wait is on
/// the CONDITION.
const SETTLED_BY: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The member of a Lambda error document whose value is MINTED per invocation: the id of the run
/// that answered it. Two invocations of one function differ in it and in nothing else, so it is
/// what a comparison of two answers replaces.
const REQUEST_ID_MEMBER: &str = "\"requestId\":";

/// What that value is replaced by while two answers are compared.
const ANY_REQUEST_ID: &str = "<the invocation's own id>";

/// What the relay states for a member of the invocation's answer that carried no value — the run's
/// `LogResult`, which an invocation that asked for no log tail does not carry.
const ABSENT_MEMBER: &str = "null";

/// The error codes Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries nothing the function decided: the concurrency this process can hold at once was
/// full (`TooManyRequestsException`), or the function was not `Active` yet
/// (`ResourceConflictException`). Neither is a verdict, so a call refused for one of them is made
/// again.
///
/// Whether one arrives is decided by what the rest of this machine is running at that instant and
/// by nothing a row varies; no handler can produce either, so recognising exactly these two cannot
/// swallow a defect of the relay — a function that ran is reported as `Unhandled` and is asserted
/// at once.
const UNSERVED_CODES: &[&str] = &["TooManyRequestsException", "ResourceConflictException"];

/// How many attempts one gated call is given while Lambda answers that way. A handful: the
/// condition is one the host's own load creates and clears, so it cannot persist across this many
/// attempts for a reason nothing here varies — and a bounded count is what makes a gate that
/// answers one of those codes for a reason of its own a reported failure rather than a wait.
const UNSERVED_ATTEMPTS: usize = 8;

/// The handler every validator of this suite runs: it reads the candidate configuration out of the
/// event AppConfig handed it, lets a document carrying the member its environment names through,
/// and otherwise RAISES — the class and the sentence coming from its own environment, so each row
/// produces a different error document while one handler is archived for all of them.
///
/// The sentence carries the document it refused, so the error document a row relays could not have
/// been written by anything that did not see the candidate configuration.
const VALIDATOR_HANDLER: &str = r#"
import base64
import json
import os


class ConfigurationRejected(Exception):
    """The class a validator raises when it names the refusal itself."""


CLASSES = {
    "ValueError": ValueError,
    "RuntimeError": RuntimeError,
    "ConfigurationRejected": ConfigurationRejected,
}


def _document(event):
    encoded = event.get("content")
    if encoded is None:
        return "<the event carried no content>"
    return base64.b64decode(encoded).decode("utf-8", "replace")


def lambda_handler(event, context):
    document = _document(event)
    required = os.environ["REQUIRED_MEMBER"]
    try:
        parsed = json.loads(document)
    except ValueError:
        parsed = {}
    if isinstance(parsed, dict) and required in parsed:
        return {"valid": True}
    raise CLASSES[os.environ["REJECTION_CLASS"]](
        os.environ["REJECTION_SENTENCE"] + " " + document
    )
"#;

/// The services this suite exercises, on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them: a validator's invocation is dispatched back into this same router, so the function the
/// validator names and the profile that names it are records of this one emulator.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
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
    router.register_service(
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(engine),
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

    /// The body as text, for a failure message and for an answer that is a function's own
    /// document rather than a modelled shape.
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

    /// The `Message` of a refusal, or the whole body when it carries none.
    fn message(&self) -> String {
        self.string("Message").unwrap_or_else(|| self.text())
    }

    /// Whether this is a refusal Lambda's own "not now" produced rather than a verdict a
    /// function gave ([`UNSERVED_CODES`]).
    fn unserved(&self) -> bool {
        let message = self.message();
        self.status == BAD_REQUEST_STATUS
            && UNSERVED_CODES.iter().any(|code| message.contains(code))
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

/// One AppConfig control-plane call carrying a JSON body.
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

/// One AppConfig control-plane call whose body is a configuration's own bytes, of its own media
/// type — the form `CreateHostedConfigurationVersion` binds its `@httpPayload` content and its
/// `@required` `Content-Type` header in.
async fn appconfig_content(
    router: &MultiServiceRouter,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    dispatch(
        router,
        "appconfig",
        "appconfig",
        "POST",
        uri,
        content_type,
        body,
    )
    .await
}

/// One Lambda `restJson1` call.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answered {
    dispatch(
        router,
        "lambda",
        "lambda",
        method,
        uri,
        JSON_CONTENT_TYPE,
        body,
    )
    .await
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

/// One row: the validator a profile carries, the document stored under that profile, and whether
/// the validator finds it compliant.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The exception class the validator raises, as its own environment names it.
    class: &'static str,
    /// The sentence it raises with, before the document it refused.
    sentence: &'static str,
    /// The member a compliant document has to carry.
    required: &'static str,
    /// The document stored under the profile and handed to the validator.
    document: &'static str,
    /// Whether [`Row::document`] carries [`Row::required`], and so passes the gate.
    compliant: bool,
}

/// The rows. Every rejecting row's expected sentence is read back from the function itself, so
/// nothing of the relayed answer is written out here.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a validator raising a built-in class",
            class: "ValueError",
            sentence: "the configuration must set replicas:",
            required: "replicas",
            document: r#"{"replica_count":3}"#,
            compliant: false,
        },
        Row {
            case: "a validator raising a class its own module declares",
            class: "ConfigurationRejected",
            sentence: "refused by the release gate:",
            required: "enabled",
            document: r#"{"flag":"off"}"#,
            compliant: false,
        },
        Row {
            case: "a rejection whose sentence carries quotes, braces and non-ASCII text",
            class: "RuntimeError",
            sentence: "cannot accept {\"mode\": \"beta\"} \u{2014} prüfung fehlgeschlagen:",
            required: "mode_v2",
            document: r#"{"mode":"beta","note":"ünïcode"}"#,
            compliant: false,
        },
        Row {
            case: "a validator that finds the document compliant",
            class: "ValueError",
            sentence: "never raised by this row:",
            required: "replicas",
            document: r#"{"replicas":2}"#,
            compliant: true,
        },
    ]
}

/// The name of the function one row's validator names.
fn function_name(index: usize) -> String {
    format!("xsvc-appconfig-validator-{index}")
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

/// The fixture stack: one execution role, and per row one function running [`VALIDATOR_HANDLER`]
/// under that row's environment.
///
/// The handler is declared as the stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to exist at
/// all.
fn stack_template(rows: &[Row]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": trust_policy(),
                "Policies": [{
                    "PolicyName": "write-the-validators-logs",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream",
                                       "logs:PutLogEvents"],
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }),
    );
    let mut outputs = serde_json::Map::new();
    for (index, row) in rows.iter().enumerate() {
        let logical = format!("Validator{index}");
        resources.insert(
            logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {
                        "REJECTION_CLASS": row.class,
                        "REJECTION_SENTENCE": row.sentence,
                        "REQUIRED_MEMBER": row.required,
                    }},
                    "Code": {"ZipFile": VALIDATOR_HANDLER},
                },
            }),
        );
        outputs.insert(
            format!("FunctionArn{index}"),
            json!({"Value": {"Fn::GetAtt": [logical, "Arn"]}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Deploy the fixture stack, wait for `CREATE_COMPLETE` and answer the function ARN of each row.
async fn deploy(router: &MultiServiceRouter, stack: &str, rows: &[Row]) -> Vec<String> {
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", stack.to_string()),
            ("TemplateBody", stack_template(rows)),
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
            published
                .get(&format!("FunctionArn{index}"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!("setup: the stack must publish row {index}'s function ARN: {published:?}")
                })
        })
        .collect()
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs: a function
/// still provisioning would refuse the validator's invocation, which is a different rule.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        let reported = read.json();
        if reported["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda must report {name:?} {FUNCTION_ACTIVE}, last reported {reported}"
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
    answered.string("Id").unwrap_or_else(|| {
        panic!(
            "case {case:?}: creating {what} answered no Id: {}",
            answered.text()
        )
    })
}

/// `text` with the value of every `requestId` member replaced by [`ANY_REQUEST_ID`].
///
/// A Lambda error document names the invocation that produced it, so two answers of one function
/// differ in that value and in nothing else. Replacing it is what lets the sentence AppConfig
/// relayed be compared with the answer the same function gave a client — everything else, down to
/// the member order and the separators the runtime wrote, has to match.
fn without_the_request_id(text: &str) -> String {
    let mut relayed = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(REQUEST_ID_MEMBER) {
        let (head, tail) = rest.split_at(at + REQUEST_ID_MEMBER.len());
        relayed.push_str(head);
        let Some(opens) = tail.find('"') else {
            break;
        };
        let Some(closes) = tail[opens + 1..].find('"') else {
            break;
        };
        relayed.push_str(&tail[..=opens]);
        relayed.push_str(ANY_REQUEST_ID);
        rest = &tail[opens + 1 + closes..];
    }
    relayed.push_str(rest);
    relayed
}

/// The event a `LAMBDA` validator is handed: the resources the configuration belongs to, the
/// version under validation, its content base64-encoded and the location the configuration is held
/// at, keyed as the AppConfig User Guide's validator page documents them.
fn validator_event(application: &str, profile: &str, version: &str, document: &str) -> Vec<u8> {
    json!({
        "applicationId": application,
        "configurationProfileId": profile,
        "configurationVersion": version,
        "content": ::aws_smithy_types::base64::encode(document),
        "uri": HOSTED,
    })
    .to_string()
    .into_bytes()
}

/// What the invocation of `function` with `event` reported, as the one sentence a service relaying a
/// function's own answer states: the `FunctionError`, the run's `LogResult` and the response payload
/// the function's runtime wrote.
///
/// This is the EXPECTATION every rejecting row is asserted against, taken from Lambda's own API
/// rather than written out here: the client asks for no log tail, exactly as a relaying service's
/// own invocation does, so the answer reports none. The only literal of the sentence written here is
/// its FIXED text, which is the service's own and is pinned beside the rendering it belongs to (the
/// AppConfig crate's `extension_actions` unit test); everything variable in it comes from an
/// invocation this suite made itself.
async fn reported_by_the_function(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &[u8],
) -> (String, Value) {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let invoked = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            event,
        )
        .await;
        if let Some(function_error) = invoked.header("X-Amz-Function-Error") {
            let log_result = invoked
                .header("X-Amz-Log-Result")
                .unwrap_or_else(|| ABSENT_MEMBER.to_string());
            return (
                format!(
                    "Function Error: {function_error}, Log Result: {log_result}, Payload: {}",
                    invoked.text()
                ),
                invoked.json(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the validator must report an error for the document it refuses, got \
             {} with {}",
            invoked.status,
            invoked.text()
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Make one gated call, making it again for as long as Lambda answers the validator's invocation
/// with one of its own "not now" codes ([`UNSERVED_CODES`]), which is no verdict.
///
/// The repeat is bounded twice over — at most [`UNSERVED_ATTEMPTS`] attempts, and never past the
/// deadline — so a gate that answers one of those codes for a reason of its OWN is reported rather
/// than waited out: the last answer is returned to the caller, which is what asserts on its status
/// and wording. Nothing here can hide the defect this suite pins either: a gate that never ran at
/// all answers success, which every rejecting row asserts against. A refused call persists nothing
/// (asserted per row against the environment's deployment history), so a repeat starts from the
/// state the first attempt did.
async fn gated_call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let deadline = Instant::now() + SETTLED_BY;
    for _ in 1..UNSERVED_ATTEMPTS {
        let answered = appconfig(router, method, uri, body).await;
        if !answered.unserved() || Instant::now() >= deadline {
            return answered;
        }
        tokio::time::sleep(LOOK).await;
    }
    appconfig(router, method, uri, body).await
}

/// RULE: a `LAMBDA` validator gives its verdict by failing, and the `BadRequestException` the gated
/// call is refused with relays what the invocation reported — the `FunctionError`, the `LogResult`
/// and the error document the function's runtime wrote — while a validator that reports no error
/// lets the call through. A refused deployment leaves the environment's history empty.
#[tokio::test]
async fn a_lambda_validators_refusal_relays_the_answer_the_function_gave() {
    let router = build_router();
    let rows = rows();
    let functions = deploy(&router, "xsvc-appconfig-validators", &rows).await;
    for index in 0..rows.len() {
        wait_until_active(&router, &function_name(index)).await;
    }

    // One strategy for every row: it reaches every target in one step and bakes for no time, so a
    // deployment the gate lets through is over as soon as the service's own clock says it is.
    let strategy = minted_id(
        "setup",
        "the deployment strategy",
        &appconfig(
            &router,
            "POST",
            "/deploymentstrategies",
            &json!({
                "Name": "xsvc-appconfig-validators-instant",
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
        let function = &functions[index];
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                "POST",
                "/applications",
                &json!({"Name": format!("xsvc-validator-app-{index}")}),
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
                &json!({"Name": format!("xsvc-validator-env-{index}")}),
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
                    "Name": format!("xsvc-validator-profile-{index}"),
                    "LocationUri": HOSTED,
                    "Validators": [{"Type": "LAMBDA", "Content": function}],
                }),
            )
            .await,
        );

        // The version the gate runs against. A validator gates what goes INTO SERVICE, not what is
        // stored, so this write is accepted whatever the validator would say about it.
        let written = appconfig_content(
            &router,
            &format!(
                "/applications/{application}/configurationprofiles/{profile}/\
                 hostedconfigurationversions"
            ),
            JSON_CONTENT_TYPE,
            row.document.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: CreateHostedConfigurationVersion must answer 201, got {} with {}",
            written.status,
            written.text()
        );
        let version = written
            .header("Version-Number")
            .unwrap_or_else(|| panic!("case {case:?}: the write must answer its version number"));

        let validate = format!(
            "/applications/{application}/configurationprofiles/{profile}/validators\
             ?configuration_version={}",
            form_encode(&version)
        );
        let deployments =
            format!("/applications/{application}/environments/{environment}/deployments");
        let start = json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
        });

        if row.compliant {
            let validated = gated_call(&router, "POST", &validate, &Value::Null).await;
            assert_eq!(
                validated.status,
                204,
                "case {case:?}: a document the validator accepts must answer 204, got {} with {}",
                validated.status,
                validated.text()
            );
            let started = gated_call(&router, "POST", &deployments, &start).await;
            assert_eq!(
                started.status,
                201,
                "case {case:?}: a deployment the validator accepts must answer 201, got {} with {}",
                started.status,
                started.text()
            );
            let listed = appconfig(&router, "GET", &deployments, &Value::Null).await;
            assert_eq!(
                listed.json()["Items"].as_array().map(Vec::len),
                Some(1),
                "case {case:?}: the accepted deployment must be the environment's one record, got \
                 {}",
                listed.text()
            );
            continue;
        }

        // THE EXPECTATION: what the same function answers a client that invokes it with the same
        // event, assembled as a relay of that answer states it.
        let (expected, answered) = reported_by_the_function(
            &router,
            case,
            function,
            &validator_event(&application, &profile, &version, row.document),
        )
        .await;
        // The function's answer really is its verdict on THIS document: its own sentence, and the
        // configuration it was handed. A validator that never saw the candidate bytes could not
        // report them.
        let raised = answered["errorMessage"].as_str().unwrap_or_default();
        assert!(
            raised.contains(row.sentence) && raised.contains(row.document),
            "case {case:?}: the validator's own error must carry its sentence and the document it \
             refused, got {answered}"
        );

        // THE GRADED CALLS: both operations the gate refuses, each relaying that same answer.
        for (what, method, uri, body) in [
            ("StartDeployment", "POST", deployments.clone(), start.clone()),
            ("ValidateConfiguration", "POST", validate.clone(), Value::Null),
        ] {
            let refused = gated_call(&router, method, &uri, &body).await;
            assert_eq!(
                refused.status,
                BAD_REQUEST_STATUS,
                "case {case:?}: {what} must be refused {BAD_REQUEST_STATUS}, got {} with {}",
                refused.status,
                refused.text()
            );
            let reported = refused.header("x-amzn-errortype").unwrap_or_default();
            assert!(
                reported.contains(BAD_REQUEST),
                "case {case:?}: {what} must be refused {BAD_REQUEST}, got {reported:?} with {}",
                refused.text()
            );
            assert_eq!(
                without_the_request_id(&refused.message()),
                without_the_request_id(&expected),
                "case {case:?}: {what} must relay what the invocation reported, expected {:?}, \
                 got {:?}",
                without_the_request_id(&expected),
                without_the_request_id(&refused.message())
            );
        }

        // A refused deployment persists nothing, so the environment has no deployment at all — the
        // refusal did not take a number of its sequence either.
        let listed = appconfig(&router, "GET", &deployments, &Value::Null).await;
        assert_eq!(
            listed.json()["Items"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a refused deployment must leave the environment's history empty, got {}",
            listed.text()
        );
    }
}
