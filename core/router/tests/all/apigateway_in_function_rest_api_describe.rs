// SPDX-License-Identifier: CC0-1.0

//! Describing ONE REST API from inside a running function: what a handler holding an
//! execution role scoped to the API Gateway REST control plane reads back about an api, and
//! what a handler whose role does not cover the call is told.
//!
//! > **RULE: `GetRestApi` is `GET /restapis/{restApiId}`, decided as the IAM action
//! > `apigateway:GET` on the api's own ARN — API Gateway's account-less, path-style
//! > `arn:aws:apigateway:{region}::/restapis/{id}` — against the identity-based documents in
//! > force on the principal the call is signed as, a function's execution-role session
//! > included. A call one of those documents allows is answered HTTP 200 with the `RestApi`
//! > record, whose `name` is the name the api was created with and whose `id` is the id it was
//! > created under; a call no readable document allows is refused HTTP 403
//! > `AccessDeniedException` with the sentence IAM words an authorization failure in, which an
//! > SDK inside the function raises as a client error. Reading an api changes nothing a later
//! > caller sees.**
//!
//! Four crates have to agree for that to hold, which is why this is a router suite: IAM holds
//! the execution role and the document a client put on it, the token service mints the session
//! the function's own calls are signed as, AWS Lambda runs the handler and signs those calls,
//! and API Gateway takes the decision and serves — or refuses — the read. Everything is driven
//! the way a client reaches it (`iam:CreateRole`, `cloudformation:CreateStack`,
//! `iam:PutRolePolicy`, `lambda:Invoke`, and this plane's own `CreateRestApi`/`GetRestApi`), so
//! the document under test is the one IAM stored and the principal a refusal blames is the one
//! the runtime signed as.
//!
//! The tables span what the rule generalises over. The grant table varies the DOCUMENT in force
//! on the execution role: which resource it names — every api in every Region (the wildcard
//! spelling a developer writes for this plane), this api's exact ARN, every path of the plane,
//! another api's ARN, this api's ARN in another Region, and an ARN carrying an ACCOUNT, which
//! this plane's ARNs never do — which action it names (the read verb, every action of the
//! plane, every action of every service, another verb of the plane, another service's actions
//! only), and which effect it states (an allow, and an allow with an explicit deny beside it).
//! The name table varies the NAME the api was created with — one word, a hyphenated name, a
//! name carrying punctuation and digits, a name carrying a space, and a single character — so
//! what is asserted is that the record reports the name it was created with rather than one
//! recorded value.
//!
//! The sibling `apigateway_control_plane_role_authorization` suite judges the same gate over
//! the collection read and the deployment write; what is added here is the SINGLE-api read
//! whose answer a handler takes a member out of, which is the "create, then describe to
//! confirm" step any in-function automation makes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// A Region nothing here is addressed in, which one row's grant names instead.
const OTHER_REGION: &str = "eu-west-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const CFN_VERSION: &str = "2010-05-15";

/// The error shape and status an authorization refusal carries: IAM's own, in no operation's
/// modelled error set, because the decision is taken ahead of the service.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on this plane.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// Where Lambda binds the collection a function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State` a
/// function reports before it may be invoked. The timeout is well past what a run needs on any
/// host: what is under test is the read and the decision about it, not how fast an interpreter
/// gets through the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;
const ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long an observation of work the CLOCK owes — a stack completing, a function going
/// `Active`, an invocation finding a run slot — waits before calling it a hang, and how long
/// between two looks. Generous, so a loaded host delays the setup instead of deciding the
/// outcome; each wait is on its own CONDITION rather than on a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The stack the probe function is deployed by, the role it runs as and its own name.
const STACK: &str = "apigw-describe-probe-stack";
const EXECUTION_ROLE: &str = "apigw-describe-probe-role";
const FUNCTION: &str = "apigw-describe-probe";

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

/// A SigV4 `Authorization` whose credential names `access_key_id` and whose scope names
/// `signing_name` and [`REGION`], which is what routes the request and what Region the
/// decision is taken in. The signature itself is never checked here.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
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
                .header("authorization", authorization(ACCOUNT_KEY_ID, signing_name))
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

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
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
    assert_eq!(
        answered.status, 200,
        "setup: {signing_name} {action} must answer 200, got {} with {}",
        answered.status, answered.text
    );
    answered.text
}

/// One API Gateway REST control-plane call under the account's own credential, which must be
/// served; answered with its document.
async fn served_control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    expected_status: u16,
) -> Value {
    let answered = dispatch(
        router,
        "apigateway",
        method,
        path,
        "application/json",
        body.map(|value| value.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(
        answered.status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {} {}",
        answered.status, answered.text
    );
    answered.json()
}

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what}: expected a non-empty {member:?}, got {body}"))
}

/// The ARN of an API Gateway resource whose URI path is `path`, in `region`: this plane's ARNs
/// carry an EMPTY account field, and their resource portion IS the path of the operation that
/// reaches them.
fn apigateway_arn_in(region: &str, path: &str) -> String {
    format!("arn:aws:apigateway:{region}::{path}")
}

/// The same ARN in the Region every call here is signed for.
fn apigateway_arn(path: &str) -> String {
    apigateway_arn_in(REGION, path)
}

/// The IAM action the read under test performs: the HTTP method of the request that serves it.
const READ_ACTION: &str = "apigateway:GET";

/// The sentence IAM refuses an unauthorized call with: who was refused, which action, on which
/// resource, and which of IAM's rules refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The clause closing a refusal no readable document allowed — IAM's default deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The clause closing a refusal a readable document DENIED.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// An identity-based policy allowing EVERYTHING and denying `action` on every resource: the
/// explicit deny no grant lifts, which a single-statement document cannot express.
fn allow_everything_but(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "*", "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": "*" },
        ],
    })
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

/// The one name an inline document is put on the role under.
const INLINE_POLICY: &str = "grants";

/// Hold the execution role, trusted by Lambda and carrying the AWS-managed execution-role
/// policy and no statement about this plane at all, through IAM's own calls. Answers its ARN.
async fn given_execution_role(router: &MultiServiceRouter) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
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
        &[("RoleName", EXECUTION_ROLE), ("PolicyArn", BASIC_EXECUTION)],
    )
    .await;
    element(&created, "Arn")
}

/// Put `document` on the execution role as its one inline policy — what a client grants an
/// existing role a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// The session a call the running function makes is signed as: "the execution role grants the
/// function permission to use Amazon Web Services services"
/// (`FunctionConfiguration$Role`), and the session a managed runtime opens for a run is named
/// after the function.
fn execution_session() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{EXECUTION_ROLE}/{FUNCTION}",
        ::engine::emulator_account_id()
    )
}

/// `CreateRestApi` for `name` under the account's own credential, answering the id it was
/// created under.
async fn create_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = served_control_plane(
        router,
        &format!("CreateRestApi({name:?})"),
        "POST",
        "/restapis",
        Some(&json!({ "name": name })),
        201,
    )
    .await;
    assert_eq!(
        created["name"],
        json!(name),
        "setup: CreateRestApi({name:?}) must report the name it was created with, got {created}"
    );
    as_str(&format!("setup: CreateRestApi({name:?})"), &created, "id")
}

/// `GetRestApi` for `api` under the account's own credential: the record a client reads
/// OUTSIDE the function, which is what the in-function read has to agree with.
async fn read_api(router: &MultiServiceRouter, case: &str, api: &str) -> Value {
    served_control_plane(
        router,
        &format!("{case} / GetRestApi"),
        "GET",
        &format!("/restapis/{api}"),
        None,
        200,
    )
    .await
}

/// The handler the probe function runs: `import boto3` and the control-plane client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than this call — and then
/// the ONE read the event names, reporting the api's name and id in a JSON `body` the way a
/// web handler does, or the refusal API Gateway answered it with.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

APIGATEWAY = boto3.client("apigateway")


def handler(event, context):
    try:
        api = APIGATEWAY.get_rest_api(restApiId=event["api"])
        return {
            "statusCode": 200,
            "body": json.dumps({"name": api["name"], "id": api["id"]}),
        }
    except botocore.exceptions.ClientError as error:
        return {
            "statusCode": error.response["ResponseMetadata"]["HTTPStatusCode"],
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
        }
"#;

/// The stack declaring the probe function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn function_stack(role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
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

/// Look again until the stack `stack` reports [`CREATE_COMPLETE`], and answer the last status
/// it reported. Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = served_query(
            router,
            "cloudformation",
            CFN_VERSION,
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the probe function reports [`ACTIVE`], which is the state it cannot be
/// invoked before. Waits on that CONDITION rather than on a span.
async fn wait_for_active(router: &MultiServiceRouter) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let answered = dispatch(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            "application/json",
            Vec::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "setup: GetFunctionConfiguration({FUNCTION}) must be served, got {} {}",
            answered.status, answered.text
        );
        let configuration = answered.json();
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {FUNCTION} never reported {ACTIVE}, last {last}");
}

/// The status Lambda answers an invocation that found no run capacity with: "Lambda returns
/// `TooManyRequestsException` if running the function would cause you to exceed a concurrency
/// limit" (`smithy_query(query="operation", operation="Invoke")`).
const THROTTLED: u16 = 429;

/// One invocation of the probe function with `event`, as it came back.
async fn invoke_once(router: &MultiServiceRouter, event: &Value) -> Answered {
    dispatch(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        "application/json",
        event.to_string().into_bytes(),
    )
    .await
}

/// Invoke the probe function for `api` and answer the payload the handler returned.
///
/// A host running many functions at once has only so many run slots, and an invocation that
/// finds none is answered the service's own capacity refusal ([`THROTTLED`]) — which is not the
/// decision under test, so this looks again until one is not throttled, as a client retries a
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`], so a loaded
/// host delays this rather than deciding it; every other answer is asserted at once.
async fn describe_in_function(router: &MultiServiceRouter, case: &str, api: &str) -> Value {
    let event = json!({ "api": api });
    let giving_up_at = Instant::now() + DEADLINE;
    let mut answered = invoke_once(router, &event).await;
    while answered.status == THROTTLED && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        answered = invoke_once(router, &event).await;
    }
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({FUNCTION}), got {} {}",
        answered.status, answered.text
    );
    let signal = answered
        .headers
        .get("x-amz-function-error")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    assert_eq!(
        signal, None,
        "case {case:?}: expected the handler to return rather than raise, got {signal:?} with {}",
        answered.text
    );
    answered.json()
}

/// The JSON object a handler's `body` carries, as the caller of a web handler parses it: the
/// payload member is a STRING holding a JSON document.
fn handler_body(case: &str, payload: &Value) -> Value {
    let body = payload
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string body, got {payload}"));
    serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON body, got {body:?}"))
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is the AUTHORIZATION of a credential, not its signature. A handler's own
    // calls are signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// One row of the grant table: the document a client put on the execution role, and whether
/// the in-function read it decides is served.
struct Grant {
    case: &'static str,
    document: Value,
    served: bool,
    /// Which of IAM's rules refused the call, for a row that is not served.
    reason: String,
}

/// The grant table for the api `api`: the documents whose resource, action and effect decide
/// this one read.
fn grants(api: &str) -> Vec<Grant> {
    let served = |case: &'static str, document: Value| Grant {
        case,
        document,
        served: true,
        reason: String::new(),
    };
    let refused = |case: &'static str, document: Value, reason: String| Grant {
        case,
        document,
        served: false,
        reason,
    };
    vec![
        served(
            "the read verb on every REST API in every Region",
            allow(READ_ACTION, &apigateway_arn_in("*", "/restapis/*")),
        ),
        served(
            "the read verb on this api's own ARN",
            allow(READ_ACTION, &apigateway_arn(&format!("/restapis/{api}"))),
        ),
        served(
            "the read verb on every path of the plane",
            allow(READ_ACTION, &apigateway_arn_in("*", "*")),
        ),
        served(
            "every action of the plane on every REST API",
            allow("apigateway:*", &apigateway_arn_in("*", "/restapis/*")),
        ),
        served("every action on every resource", allow("*", "*")),
        refused(
            "another service's actions only",
            allow("logs:PutLogEvents", "*"),
            no_grant(READ_ACTION),
        ),
        refused(
            "the read verb on another api's ARN",
            allow(READ_ACTION, &apigateway_arn("/restapis/other0000")),
            no_grant(READ_ACTION),
        ),
        refused(
            "the read verb on this api's ARN in another Region",
            allow(
                READ_ACTION,
                &apigateway_arn_in(OTHER_REGION, &format!("/restapis/{api}")),
            ),
            no_grant(READ_ACTION),
        ),
        refused(
            "the read verb on an ARN carrying an account",
            allow(
                READ_ACTION,
                &format!(
                    "arn:aws:apigateway:{REGION}:{}:/restapis/*",
                    ::engine::emulator_account_id()
                ),
            ),
            no_grant(READ_ACTION),
        ),
        refused(
            "another verb of the plane on every REST API",
            allow("apigateway:PATCH", &apigateway_arn_in("*", "/restapis/*")),
            no_grant(READ_ACTION),
        ),
        refused(
            "everything but the read verb of the plane",
            allow_everything_but(READ_ACTION),
            EXPLICIT_DENY.to_string(),
        ),
    ]
}

/// The names the apis are created with: the forms a REST API's name takes, so what the read
/// reports is the name the api carries rather than one recorded value.
const NAMES: &[(&str, &str)] = &[
    ("one word", "orders"),
    ("a hyphenated name", "orders-service-v2"),
    ("punctuation and digits", "orders.v2_2026"),
    ("a name carrying a space", "orders service"),
    ("a single character", "o"),
];

/// RULE: an in-function `GetRestApi` reports the api's own record — the `name` it was created
/// with and the `id` it was created under — wherever the documents in force on the function's
/// execution role allow `apigateway:GET` on the api's account-less path-style ARN, and is
/// refused `AccessDeniedException` with IAM's own sentence wherever they do not; and the record
/// it reads is unchanged by having been read.
#[tokio::test]
async fn an_in_function_describe_reports_the_rest_apis_name_where_the_role_grants_the_read() {
    let router = emulator();
    let principal = execution_session();

    // One api per name form, created the way a client creates one, plus the function that
    // describes whichever of them an invocation names.
    let apis: Vec<(&str, &str, String)> = {
        let mut created = Vec::new();
        for (case, name) in NAMES {
            created.push((*case, *name, create_api(&router, name).await));
        }
        created
    };
    let role_arn = given_execution_role(&router).await;
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &function_stack(&role_arn)),
        ],
    )
    .await;
    let reached = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached}"
    );
    wait_for_active(&router).await;

    let (_, first_name, first_api) = apis
        .first()
        .cloned()
        .expect("the name table names at least one api");

    // The role carries only the AWS-managed execution-role policy, which says nothing about
    // this plane: the read is refused for the action its own verb names, on the ARN of the
    // path it addressed, and the refusal names the session the runtime signed it as.
    let case = "no document about this plane at all";
    let reported = describe_in_function(&router, case, &first_api).await;
    assert_refused(case, &reported, &principal, &first_api, &no_grant(READ_ACTION));

    // The record as a client reads it from outside, before any in-function read: what the
    // served rows have to agree with, and what the reads must leave untouched.
    let before = read_api(&router, "before the in-function reads", &first_api).await;

    for grant in grants(&first_api) {
        put_role_policy(&router, &grant.document).await;
        let reported = describe_in_function(&router, grant.case, &first_api).await;
        if !grant.served {
            assert_refused(
                grant.case,
                &reported,
                &principal,
                &first_api,
                &grant.reason,
            );
            continue;
        }
        assert_eq!(
            reported["statusCode"],
            json!(200),
            "case {:?}: expected the handler to report the served read, got {reported}",
            grant.case
        );
        let body = handler_body(grant.case, &reported);
        assert_eq!(
            (body["name"].clone(), body["id"].clone()),
            (json!(first_name), json!(first_api)),
            "case {:?}: expected the api's own name and id, got {body}",
            grant.case
        );
    }

    // The api reads the same afterwards as before: describing it changed nothing a later
    // caller sees.
    let after = read_api(&router, "after the in-function reads", &first_api).await;
    assert_eq!(
        after, before,
        "case \"after the in-function reads\": expected the record to be unchanged, got {after} \
         for {before}"
    );

    // The same grant, over every name form: what the handler reads back is the name ITS api
    // was created with, and the id it addressed.
    put_role_policy(
        &router,
        &allow(READ_ACTION, &apigateway_arn_in("*", "/restapis/*")),
    )
    .await;
    for (case, name, api) in &apis {
        let reported = describe_in_function(&router, case, api).await;
        let body = handler_body(case, &reported);
        assert_eq!(
            (body["name"].clone(), body["id"].clone()),
            (json!(name), json!(api)),
            "case {case:?}: expected the name the api was created with and its id, got {body}"
        );
        // And the same record where a client outside the function reads it.
        let read = read_api(&router, case, api).await;
        assert_eq!(
            read["name"],
            json!(name),
            "case {case:?}: expected GetRestApi to report the created name, got {read}"
        );
    }
}

/// Assert one refused in-function read: the status the SDK saw, the error shape it raised, and
/// IAM's own sentence naming the function's session, the action, the api's ARN and which of
/// IAM's rules refused it.
fn assert_refused(case: &str, reported: &Value, principal: &str, api: &str, reason: &str) {
    let resource = apigateway_arn(&format!("/restapis/{api}"));
    assert_eq!(
        (
            reported["statusCode"].clone(),
            reported["error_code"].clone()
        ),
        (json!(ACCESS_DENIED_STATUS), json!(ACCESS_DENIED)),
        "case {case:?}: expected the read to be refused HTTP {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED}, got {reported}"
    );
    assert_eq!(
        reported["message"],
        json!(not_authorized(principal, READ_ACTION, &resource, reason)),
        "case {case:?}: expected IAM's authorization sentence naming the function's session, \
         got {reported}"
    );
}
