// SPDX-License-Identifier: CC0-1.0

//! A running Lambda function — including one wired as an API Gateway `TOKEN` authorizer —
//! deciding on a TAG of the CloudFormation stack it was handed, through the whole assembled
//! router.
//!
//! > **RULE: a tag CloudFormation recorded on a stack (`CreateStack$Tags`, replaced by
//! > `UpdateStack$Tags`) is part of that stack's own record, so every reader of the stack
//! > reports exactly the set in force — a client's `DescribeStacks` over the wire, and the same
//! > call made from inside an invoked function by the AWS SDK the managed runtime ships,
//! > whichever way that function was invoked. A function's own call is decided against the
//! > documents in force on its execution role, as `cloudformation:DescribeStacks`, so a handler
//! > whose role carries the grant reads the tag set and one whose role does not is refused
//! > `AccessDenied` — two outcomes a handler can tell apart.**
//!
//! `Stack$Tags` is "a list of `Tag`s that specify information about the stack"
//! (`smithy_query(query="shape", shape="Stack",
//! model_path="services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/\
//! com-amazonaws-cloudformation-cloudformation.json")`), and `CreateStack$Tags` is the
//! "key-value pairs to associate with this stack" — so the pairs a create supplied are what a
//! describe answers, and an update that states a different list is what a later describe
//! answers.
//!
//! What the rule generalises over is the TAG SET and the READER. The sets below differ in how
//! many pairs they carry, in whether the pair a decision gates on is among them, in the text a
//! value carries and in the case a key is spelled — and each is read three ways: over the wire,
//! by a handler invoked directly, and by that same handler invoked as the `TOKEN` authorizer of
//! a REST api, whose returned policy is what `TestInvokeAuthorizer` reports. An authorizer that
//! gates on a stack tag is the shape this matters most for: a handler that cannot read
//! CloudFormation state cannot decide on it, and its failure branch answers every caller the
//! same way.
//!
//! Five crates have to agree for this to hold, which is why this is a router suite: IAM holds
//! the execution role and the documents in force on it, the token service mints the session the
//! handler's call is signed as, CloudFormation records the tags and serves the read, AWS Lambda
//! runs the handler and makes the call on its behalf, and API Gateway invokes that handler as an
//! authorizer and judges the policy it returned. Everything is driven the way a client reaches
//! it — `iam:CreateRole`, `cloudformation:CreateStack`, `apigateway:CreateAuthorizer`,
//! `lambda:AddPermission`, `apigateway:TestInvokeAuthorizer` — so what the authorizer decides is
//! what a client's own deployment would get.
//!
//! The request writers and answer readers below are this suite's own, for the reason
//! [`crate::query_wire`] records: `core/router/Cargo.toml` is a pinned path, so this crate takes
//! no dev-dependency, and each suite drives the wire with the shared encoders and element
//! readers plus the few calls it makes itself.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element, pairs_of};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which is what the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the services below are called at.
const IAM_VERSION: &str = "2010-05-08";
const CFN_VERSION: &str = "2010-05-15";

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on CloudFormation.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The principal API Gateway invokes an authorizer's function as.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// Where Lambda binds the collection a function is read, invoked and granted under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State` a
/// function reports before it may be invoked. The timeout is well past what a run needs on any
/// host: what is under test is what the call answered, not how fast an interpreter gets through
/// the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;
const ACTIVE: &str = "Active";

/// The stack statuses a create and an update of these stacks reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// How long an observation of work the CLOCK owes — a stack completing, a function going
/// `Active` — waits before calling it a hang, and how long between two looks. Generous, so a
/// loaded host delays the setup instead of deciding the outcome; each wait is on its own
/// CONDITION rather than on a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The status Lambda answers an invocation that found no run capacity with: "Lambda returns
/// `TooManyRequestsException` if running the function would cause you to exceed a concurrency
/// limit" (`smithy_query(query="operation", operation="Invoke")`).
const THROTTLED: u16 = 429;

/// The wire error code a call no document allows is refused with, and the status it carries. The
/// decision is IAM's, taken ahead of the service, so the code is in no operation's modelled
/// error set.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The tag the authorizer below gates on, and the value it insists on: this suite's own pair,
/// since the rule is about the pair a stack carries rather than about any particular one.
const GATE_KEY: &str = "stack-access";
const GATE_VALUE: &str = "granted";

/// The `principalId` the probe authorizer names, which `TestInvokeAuthorizer` reports back.
const PRINCIPAL_ID: &str = "stack-gated-user";

/// One answered request.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

/// A SigV4 `Authorization` whose credential names the account's own published key id and whose
/// scope names `signing_name` and [`REGION`], which is what routes the request and what Region
/// the decision is taken in. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", &host)
                .header("authorization", authorization(signing_name))
                .header("content-type", content_type)
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One awsQuery call, signed with the account's own published credential, answered with its
/// status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signing_name,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        form.into_bytes(),
    )
    .await;
    (answered.status, answered.text)
}

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, signing_name, version, action, parameters).await;
    assert_eq!(
        status, 200,
        "setup: {signing_name} {action} must answer 200, got {status} with {body}"
    );
    body
}

/// One API Gateway call, answered as it came back.
async fn apigateway(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    dispatch(
        router,
        "apigateway",
        method,
        path,
        "application/json",
        body.map(|body| body.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await
}

/// The trust policy of a role one AWS service assumes.
fn service_trust_policy(principal: &str) -> String {
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

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// The one name an inline document is put on a role under.
const INLINE_POLICY: &str = "grants";

/// Hold one role named `role_name` trusting Lambda, with the AWS-managed execution-role policy
/// attached to it, through IAM's own calls. Answers the role's ARN.
async fn execution_role(router: &MultiServiceRouter, role_name: &str) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            (
                "AssumeRolePolicyDocument",
                &service_trust_policy(LAMBDA_PRINCIPAL),
            ),
        ],
    )
    .await;
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "AttachRolePolicy",
        &[("RoleName", role_name), ("PolicyArn", BASIC_EXECUTION)],
    )
    .await;
    element(&created, "Arn")
}

/// Put `document` on `role_name` as its one inline policy — what a client grants an existing
/// role a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// The handler the probe function runs: `import boto3` and the CloudFormation client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than the call — and then one
/// read of the stack the event names.
///
/// It serves both readings of the same code, as a real authorizer does: an event carrying an
/// `authorizationToken` is API Gateway's `TOKEN` event, answered with an authorizer output whose
/// policy allows the method only when the named stack carries the gate pair; any other event is
/// a direct invocation, answered with what the read itself reported — the tag set, or the
/// refusal CloudFormation answered the call with.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

CFN = boto3.client("cloudformation")

GATE_KEY = "stack-access"
GATE_VALUE = "granted"


def _read(stack):
    """What CloudFormation reports about `stack`, or the refusal it answered the call with."""
    try:
        held = CFN.describe_stacks(StackName=stack)["Stacks"][0]
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "stack_name": held["StackName"],
        "status": held["StackStatus"],
        "tags": {tag["Key"]: tag["Value"] for tag in held.get("Tags", [])},
    }


def _policy(effect, method_arn):
    return {
        "Version": "2012-10-17",
        "Statement": [
            {"Action": "execute-api:Invoke", "Effect": effect, "Resource": method_arn},
        ],
    }


def handler(event, context):
    token = event.get("authorizationToken")
    if token is None:
        return _read(event["stack"])
    read = _read(token)
    allowed = read["ok"] and read["tags"].get(GATE_KEY) == GATE_VALUE
    return {
        "principalId": "stack-gated-user",
        "policyDocument": _policy("Allow" if allowed else "Deny", event["methodArn"]),
        "context": {"read": json.dumps(read, sort_keys=True)},
    }
"#;

/// The template the probe function is deployed by. The inline source is archived into the
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all.
fn function_template(role_arn: &str, function: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "The probe function that reads a stack's tags",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
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

/// The template each SUBJECT stack is created from: one topic, so the stack holds a resource of
/// its own and reaches a terminal status without the tags under test being any resource's.
fn subject_template(topic: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Notices": {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": topic },
            },
        },
    })
    .to_string()
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is the tag set each reader reports and the decision taken on a handler's own
    // call. A handler's call is signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// The `Tags` member of one `CreateStack`/`UpdateStack` request, as the flattened query
/// parameters a client's SDK writes it as (`Tags.member.<n>.Key`, `Tags.member.<n>.Value`).
fn tag_parameters(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    tags.iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let at = index + 1;
            [
                (format!("Tags.member.{at}.Key"), (*key).to_string()),
                (format!("Tags.member.{at}.Value"), (*value).to_string()),
            ]
        })
        .collect()
}

/// Borrow owned query parameters as the pairs [`query`] takes.
fn borrowed(parameters: &[(String, String)]) -> Vec<(&str, &str)> {
    parameters
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Open a stack named `stack` from `template`, carrying `tags`.
async fn create_stack(
    router: &MultiServiceRouter,
    stack: &str,
    template: &str,
    tags: &[(&str, &str)],
) {
    let mut parameters = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), template.to_string()),
    ];
    parameters.extend(tag_parameters(tags));
    served_query(
        router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &borrowed(&parameters),
    )
    .await;
}

/// The whole `DescribeStacks` answer for `stack`.
async fn described(router: &MultiServiceRouter, stack: &str) -> String {
    served_query(
        router,
        "cloudformation",
        CFN_VERSION,
        "DescribeStacks",
        &[("StackName", stack)],
    )
    .await
}

/// The `Tags` of a `DescribeStacks` answer, as the key/value map a client reads it as.
fn tags_of(xml: &str) -> BTreeMap<String, String> {
    pairs_of(xml, "Key", "Value")
}

/// Look again until the stack `stack` reports `wanted`, and answer the last status it reported.
/// Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str, wanted: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = described(router, stack).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == wanted || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait for `stack` to report `wanted`, failing the setup when it never does.
async fn settled(router: &MultiServiceRouter, stack: &str, wanted: &str) {
    let reached = wait_for_stack(router, stack, wanted).await;
    assert_eq!(
        reached, wanted,
        "setup: the stack {stack:?} must reach {wanted}, got {reached}"
    );
}

/// Look again until `function` reports [`ACTIVE`], which is the state it cannot be invoked
/// before, and answer its ARN. Waits on that CONDITION rather than on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let answered = dispatch(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            "application/json",
            Vec::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "setup: GetFunctionConfiguration({function}) must be served, got {} {}",
            answered.status, answered.text
        );
        let configuration = answered.json();
        if configuration["State"] == json!(ACTIVE) {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("setup: {function} reports no FunctionArn: {configuration}")
                })
                .to_string();
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// Deploy the probe function `function` on a role named `role`, with `inline` in force on that
/// role, and answer the function's ARN.
async fn probe_function(
    router: &MultiServiceRouter,
    role: &str,
    function: &str,
    stack: &str,
    inline: Option<Value>,
) -> String {
    let role_arn = execution_role(router, role).await;
    if let Some(document) = inline {
        put_role_policy(router, role, &document).await;
    }
    create_stack(router, stack, &function_template(&role_arn, function), &[]).await;
    settled(router, stack, CREATE_COMPLETE).await;
    wait_for_active(router, function).await
}

/// Invoke the probe function with `event` and answer what the handler reported.
///
/// A host running many functions at once has only so many run slots, and an invocation that
/// finds none is answered the service's own capacity refusal ([`THROTTLED`]) — which is not what
/// is under test here, so this looks again until one is not throttled, as a client retries a
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`], so a loaded
/// host delays this rather than deciding it; every other answer is asserted at once.
async fn invoke(router: &MultiServiceRouter, case: &str, function: &str, event: &Value) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut answered = invoke_once(router, function, event).await;
    while answered.status == THROTTLED && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        answered = invoke_once(router, function, event).await;
    }
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({function}), got {} {}",
        answered.status, answered.text
    );
    let signal = answered
        .headers
        .get("x-amz-function-error")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    assert_eq!(
        signal, None,
        "case {case:?}: expected no function error, got {signal:?} with {}",
        answered.text
    );
    answered.json()
}

/// One invocation of the probe function with `event`, as it came back.
async fn invoke_once(router: &MultiServiceRouter, function: &str, event: &Value) -> Answered {
    dispatch(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        event.to_string().into_bytes(),
    )
    .await
}

/// A reported map member, as the map a client compares against the service's own answer.
fn reported_map(reported: &Value, member: &str) -> BTreeMap<String, String> {
    reported[member]
        .as_object()
        .map(|held| {
            held.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A REST api with one `TOKEN` authorizer backed by `function_arn`, granted to API Gateway: the
/// api id and the authorizer id a simulated execution is addressed by.
async fn token_authorizer(
    router: &MultiServiceRouter,
    api_name: &str,
    function: &str,
    function_arn: &str,
) -> (String, String) {
    let created = apigateway(router, "POST", "/restapis", Some(json!({ "name": api_name }))).await;
    assert_eq!(
        created.status, 201,
        "setup: CreateRestApi({api_name:?}) must answer 201, got {} {}",
        created.status, created.text
    );
    let api = created.json()["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateRestApi answered no id: {}", created.text))
        .to_string();
    let authorizer = apigateway(
        router,
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": format!("{api_name}-gate"),
            "type": "TOKEN",
            "identitySource": "method.request.header.Authorization",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
            // Nothing is held between two simulated executions, which is also what the
            // simulation itself does; stated so the api cannot answer one row out of a result
            // held for another.
            "authorizerResultTtlInSeconds": 0,
        })),
    )
    .await;
    assert_eq!(
        authorizer.status, 201,
        "setup: CreateAuthorizer for {api_name:?} must answer 201, got {} {}",
        authorizer.status, authorizer.text
    );
    let authorizer_id = authorizer.json()["id"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateAuthorizer answered no id: {}", authorizer.text))
        .to_string();
    let granted = dispatch(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        "application/json",
        json!({
            "StatementId": "apigateway-authorizer",
            "Action": "lambda:InvokeFunction",
            "Principal": APIGATEWAY_PRINCIPAL,
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{}:{api}/*/*",
                ::engine::emulator_account_id()
            ),
        })
        .to_string()
        .into_bytes(),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: AddPermission({function:?}) must answer 201, got {} {}",
        granted.status, granted.text
    );
    (api, authorizer_id)
}

/// What one simulated authorizer execution answered: the `Effect` of the policy the authorizer
/// returned, the principal it named, and the whole response for an assertion to quote.
struct Decision {
    effect: Option<String>,
    principal_id: Option<String>,
    reported: Value,
}

/// Simulate the authorizer `authorizer_id` of `api` for a request presenting `token`, which is
/// what a client calls `TestInvokeAuthorizer` for.
async fn simulated(
    router: &MultiServiceRouter,
    case: &str,
    api: &str,
    authorizer_id: &str,
    token: &str,
) -> Decision {
    let answered = apigateway(
        router,
        "POST",
        &format!("/restapis/{api}/authorizers/{authorizer_id}"),
        Some(json!({ "headers": { "Authorization": token } })),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from TestInvokeAuthorizer, got {} {}",
        answered.status, answered.text
    );
    let reported = answered.json();
    let policy: Value = reported["policy"]
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    Decision {
        effect: policy["Statement"][0]["Effect"]
            .as_str()
            .map(str::to_string),
        principal_id: reported["principalId"].as_str().map(str::to_string),
        reported,
    }
}

/// What one row of the tag table states: the pairs the stack is created with, and whether a
/// decision gating on [`GATE_KEY`]/[`GATE_VALUE`] then allows the call.
struct Row {
    case: &'static str,
    /// The stack and the topic this row's own stack is made of, so rows never share state.
    stack: &'static str,
    topic: &'static str,
    /// The `Tags` the create supplies.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the gate pair is among them, which is what the authorizer allows on.
    allowed: bool,
}

/// The rows: what the rule generalises over is the TAG SET — how many pairs it carries, whether
/// the gated pair is among them, what text a value carries, and the case a key is spelled in
/// (two keys differing only in case are two different tags).
const ROWS: &[Row] = &[
    Row {
        case: "one tag, the gated pair",
        stack: "tagged-one-pair",
        topic: "tagged-one-pair-notices",
        tags: &[(GATE_KEY, GATE_VALUE)],
        allowed: true,
    },
    Row {
        case: "several tags, the gated pair among them",
        stack: "tagged-several-pairs",
        topic: "tagged-several-pairs-notices",
        tags: &[
            ("team", "payments"),
            (GATE_KEY, GATE_VALUE),
            ("tier", "gold"),
        ],
        allowed: true,
    },
    Row {
        case: "the gated key carrying another value",
        stack: "tagged-other-value",
        topic: "tagged-other-value-notices",
        tags: &[(GATE_KEY, "revoked")],
        allowed: false,
    },
    Row {
        case: "a value carrying spaces and punctuation",
        stack: "tagged-spelled-out-value",
        topic: "tagged-spelled-out-value-notices",
        tags: &[(GATE_KEY, "granted by ops: 2026-01-01, review=annual")],
        allowed: false,
    },
    Row {
        case: "the gated key spelled in another case",
        stack: "tagged-other-case",
        topic: "tagged-other-case-notices",
        tags: &[("Stack-Access", GATE_VALUE)],
        allowed: false,
    },
    Row {
        case: "no tags at all",
        stack: "tagged-with-nothing",
        topic: "tagged-with-nothing-notices",
        tags: &[],
        allowed: false,
    },
];

/// The map a row's tags compare as.
fn expected_tags(row: &Row) -> BTreeMap<String, String> {
    row.tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// RULE: the tag set a stack was created with is what every reader of the stack reports — the
/// wire, a handler invoked directly, and that same handler invoked as a `TOKEN` authorizer whose
/// policy gates on one of those pairs.
#[tokio::test]
async fn a_stacks_recorded_tags_are_what_every_reader_of_it_reports() {
    const ROLE: &str = "stack-tag-reader-role";
    const FUNCTION: &str = "stack-tag-reader";
    const FUNCTION_STACK: &str = "stack-tag-reader-stack";

    let router = emulator();
    let function_arn = probe_function(
        &router,
        ROLE,
        FUNCTION,
        FUNCTION_STACK,
        // The grant is not what this test is about: the role may read every stack, so what each
        // row asserts is the TAG SET and the decision taken on it.
        Some(allow("cloudformation:DescribeStacks", "*")),
    )
    .await;
    let (api, authorizer_id) =
        token_authorizer(&router, "stack-tag-gated-api", FUNCTION, &function_arn).await;

    // Every row's stack is opened before any of them is waited for, so the stacks settle
    // alongside each other rather than one after another.
    for row in ROWS {
        create_stack(&router, row.stack, &subject_template(row.topic), row.tags).await;
    }
    for row in ROWS {
        settled(&router, row.stack, CREATE_COMPLETE).await;
    }

    for row in ROWS {
        let wanted = expected_tags(row);

        // 1. The WIRE: what a client reading the stack is answered.
        let answer = described(&router, row.stack).await;
        assert_eq!(
            tags_of(&answer),
            wanted,
            "case {:?}: expected DescribeStacks to report {wanted:?}, got {:?} in {answer}",
            row.case,
            tags_of(&answer)
        );

        // 2. The HANDLER, invoked directly: the same read, made by the SDK the runtime ships.
        let reported = invoke(
            &router,
            row.case,
            FUNCTION,
            &json!({ "stack": row.stack }),
        )
        .await;
        assert_eq!(
            reported["ok"],
            json!(true),
            "case {:?}: expected the handler's own read to be served, got {reported}",
            row.case
        );
        assert_eq!(
            reported_map(&reported, "tags"),
            wanted,
            "case {:?}: expected the handler to read {wanted:?}, got {reported}",
            row.case
        );

        // 3. The AUTHORIZER: the same handler, invoked by API Gateway, deciding on that set.
        let decision = simulated(&router, row.case, &api, &authorizer_id, row.stack).await;
        let effect = if row.allowed { "Allow" } else { "Deny" };
        assert_eq!(
            (
                decision.effect.as_deref(),
                decision.principal_id.as_deref()
            ),
            (Some(effect), Some(PRINCIPAL_ID)),
            "case {:?}: expected the authorizer to answer {effect} as {PRINCIPAL_ID}, got {}",
            row.case,
            decision.reported
        );
    }
}

/// RULE: a handler's own read is decided against the documents in force on its execution role,
/// so a function whose role carries `cloudformation:DescribeStacks` reads the stack's tags and
/// an otherwise identical one whose role does not is refused `AccessDenied` at HTTP 403 —
/// outcomes an authorizer gating on a tag answers differently.
#[tokio::test]
async fn only_a_handler_whose_role_grants_the_read_decides_on_the_tag() {
    const GRANTED_ROLE: &str = "granted-tag-reader-role";
    const GRANTED_FUNCTION: &str = "granted-tag-reader";
    const GRANTED_STACK: &str = "granted-tag-reader-stack";
    const UNGRANTED_ROLE: &str = "ungranted-tag-reader-role";
    const UNGRANTED_FUNCTION: &str = "ungranted-tag-reader";
    const UNGRANTED_STACK: &str = "ungranted-tag-reader-stack";
    const SUBJECT: &str = "grant-split-subject";
    const TOPIC: &str = "grant-split-notices";

    let router = emulator();
    let granted_arn = probe_function(
        &router,
        GRANTED_ROLE,
        GRANTED_FUNCTION,
        GRANTED_STACK,
        Some(allow("cloudformation:DescribeStacks", "*")),
    )
    .await;
    // The sibling role carries only the AWS-managed execution-role policy, which grants no
    // CloudFormation action at all.
    let ungranted_arn = probe_function(
        &router,
        UNGRANTED_ROLE,
        UNGRANTED_FUNCTION,
        UNGRANTED_STACK,
        None,
    )
    .await;
    create_stack(
        &router,
        SUBJECT,
        &subject_template(TOPIC),
        &[(GATE_KEY, GATE_VALUE)],
    )
    .await;
    settled(&router, SUBJECT, CREATE_COMPLETE).await;

    let (granted_api, granted_authorizer) = token_authorizer(
        &router,
        "granted-gated-api",
        GRANTED_FUNCTION,
        &granted_arn,
    )
    .await;
    let (ungranted_api, ungranted_authorizer) = token_authorizer(
        &router,
        "ungranted-gated-api",
        UNGRANTED_FUNCTION,
        &ungranted_arn,
    )
    .await;

    // The GRANTED leg: the read is served, so the authorizer decides on the tag it read.
    let case = "the role carrying the grant";
    let reported = invoke(
        &router,
        case,
        GRANTED_FUNCTION,
        &json!({ "stack": SUBJECT }),
    )
    .await;
    let gated: BTreeMap<String, String> = [(GATE_KEY.to_string(), GATE_VALUE.to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        (reported["ok"].clone(), reported_map(&reported, "tags")),
        (json!(true), gated.clone()),
        "case {case:?}: expected the granted handler to read {gated:?}, got {reported}"
    );
    let decision = simulated(&router, case, &granted_api, &granted_authorizer, SUBJECT).await;
    assert_eq!(
        decision.effect.as_deref(),
        Some("Allow"),
        "case {case:?}: expected the authorizer to allow on the tag it read, got {}",
        decision.reported
    );

    // The UNGRANTED leg: the very same call is refused, which the handler reports as the refusal
    // it is — and the authorizer, having read nothing, denies.
    let case = "the sibling role carrying no grant";
    let reported = invoke(
        &router,
        case,
        UNGRANTED_FUNCTION,
        &json!({ "stack": SUBJECT }),
    )
    .await;
    assert_eq!(
        (
            reported["ok"].clone(),
            reported["error_code"].clone(),
            reported["status"].clone(),
        ),
        (
            json!(false),
            json!(ACCESS_DENIED),
            json!(ACCESS_DENIED_STATUS),
        ),
        "case {case:?}: expected the ungranted read to be refused {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED}, got {reported}"
    );
    let refused = simulated(
        &router,
        case,
        &ungranted_api,
        &ungranted_authorizer,
        SUBJECT,
    )
    .await;
    assert_eq!(
        refused.effect.as_deref(),
        Some("Deny"),
        "case {case:?}: expected the authorizer with no grant to deny, got {}",
        refused.reported
    );
}

/// RULE: the tag set in force is the one a read reports, so a set REPLACED by `UpdateStack` —
/// "if you specify tags for this parameter, those tags replace any list of tags that are
/// currently associated with the stack" — is what every reader then reports, and the decision
/// taken on it changes with it.
#[tokio::test]
async fn the_tag_set_an_update_put_in_force_is_what_every_reader_then_reports() {
    const ROLE: &str = "updated-tag-reader-role";
    const FUNCTION: &str = "updated-tag-reader";
    const FUNCTION_STACK: &str = "updated-tag-reader-stack";
    const SUBJECT: &str = "retagged-subject";
    const TOPIC: &str = "retagged-notices";

    let router = emulator();
    let function_arn = probe_function(
        &router,
        ROLE,
        FUNCTION,
        FUNCTION_STACK,
        Some(allow("cloudformation:DescribeStacks", "*")),
    )
    .await;
    let (api, authorizer_id) =
        token_authorizer(&router, "retagged-gated-api", FUNCTION, &function_arn).await;

    // Created with the gate CLOSED, so an Allow below cannot be the create's own set read back.
    let template = subject_template(TOPIC);
    create_stack(&router, SUBJECT, &template, &[(GATE_KEY, "revoked")]).await;
    settled(&router, SUBJECT, CREATE_COMPLETE).await;

    let case = "the set the create recorded";
    let reported = invoke(&router, case, FUNCTION, &json!({ "stack": SUBJECT })).await;
    let created: BTreeMap<String, String> = [(GATE_KEY.to_string(), "revoked".to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        reported_map(&reported, "tags"),
        created,
        "case {case:?}: expected the created set {created:?}, got {reported}"
    );
    let decision = simulated(&router, case, &api, &authorizer_id, SUBJECT).await;
    assert_eq!(
        decision.effect.as_deref(),
        Some("Deny"),
        "case {case:?}: expected a deny on the created set, got {}",
        decision.reported
    );

    // The same stack, re-tagged: the gate pair replaces the set in force.
    let mut parameters = vec![
        ("StackName".to_string(), SUBJECT.to_string()),
        ("TemplateBody".to_string(), template.clone()),
    ];
    parameters.extend(tag_parameters(&[(GATE_KEY, GATE_VALUE), ("team", "ops")]));
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "UpdateStack",
        &borrowed(&parameters),
    )
    .await;
    settled(&router, SUBJECT, UPDATE_COMPLETE).await;

    let case = "the set the update put in force";
    let wanted: BTreeMap<String, String> = [
        (GATE_KEY.to_string(), GATE_VALUE.to_string()),
        ("team".to_string(), "ops".to_string()),
    ]
    .into_iter()
    .collect();
    let answer = described(&router, SUBJECT).await;
    assert_eq!(
        tags_of(&answer),
        wanted,
        "case {case:?}: expected DescribeStacks to report {wanted:?}, got {:?} in {answer}",
        tags_of(&answer)
    );
    let reported = invoke(&router, case, FUNCTION, &json!({ "stack": SUBJECT })).await;
    assert_eq!(
        reported_map(&reported, "tags"),
        wanted,
        "case {case:?}: expected the handler to read {wanted:?}, got {reported}"
    );
    let decision = simulated(&router, case, &api, &authorizer_id, SUBJECT).await;
    assert_eq!(
        decision.effect.as_deref(),
        Some("Allow"),
        "case {case:?}: expected an allow on the updated set, got {}",
        decision.reported
    );
}
