// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that deploys a Lambda function plus the IAM execution role it runs
//! as SETTLES — on the create and on a later update — and what the function's own AWS SDK
//! calls are allowed to do is decided by the statements in force on that role at the instant
//! each call is made.
//!
//! RULE: **every stack operation CloudFormation accepts reaches a TERMINAL status
//! `DescribeStacks` publishes — `CREATE_COMPLETE` or a create rollback, `UPDATE_COMPLETE` or
//! an update rollback — so a client polling it always terminates; and a deployed function's
//! own SDK call is authorized against the identity policies in force on its execution role
//! when the call is made, served when some statement in force allows that action and refused
//! with `AccessDeniedException` when none does — so the SAME call flips from refused to served
//! the moment a stack update adds the statement, and back when an update takes it away, and
//! two functions of one stack on different roles are decided independently.**
//!
//! Five crates have to agree for that to hold — CloudFormation drives the deployment and
//! decides the stack's status, IAM holds the role and its documents, STS vends the role's
//! session, Lambda RUNS the handler with the AWS SDK its runtime supplies, and the service the
//! handler calls takes the decision — so the suite lives here rather than in any one of them.
//! Every observation is made where a client makes one: `DescribeStacks`,
//! `DescribeStackResources`, `GetFunctionConfiguration`, `Invoke`, and the called services'
//! own listings; never an emulator internal.
//!
//! The rows differ along what the rule generalises over: WHICH service's action is withheld at
//! create time (neither of the two, one, the other, both) and WHICH DIRECTION the update moves
//! the grant in (a statement added, a statement taken away) — so neither verdict rests on one
//! action, one service or one direction of change. The second case is one stack carrying TWO
//! roles and TWO functions, which decides the two functions' identical calls independently
//! inside a single create.
//!
//! Every expected value is computed here: a refusal is held against the AWS-common error code,
//! and a served listing's count against what the OWNING service answers that same listing with,
//! read through its own API — so no count is a literal.
//!
//! `cfn_lambda_role_grant_decides_its_secret_read` pins WHERE on one role a grant may be
//! attached, and `cfn_agent_function_stack_settles` pins how many owning services one settling
//! template may span; both hold the role's documents FIXED once the stack is created, which is
//! the half this suite varies.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region every resource belongs to.
const REGION: &str = "us-east-1";

/// Where Lambda binds `GetFunctionConfiguration` and `Invoke`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The terminal statuses this suite's accepted operations must settle on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The suffix of every status CloudFormation reports for an operation still in flight, and so
/// the one status class a poll for a terminal status looks past.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The `State` an invocation needs the function to be in ("the function is ready to be
/// invoked", `FunctionConfiguration$State`), and the `LastUpdateStatus` it must be past.
const STATE_ACTIVE: &str = "Active";
const UPDATE_IN_PROGRESS: &str = "InProgress";

/// The error code AWS answers a call no policy in force allows with — the AWS-common
/// `AccessDeniedException`, which is the code the SDK hands the handler and the handler
/// reports back as its own `error_code`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The status Lambda answers an invocation it has NOT served with, which carries no answer of
/// the handler's: the concurrent-execution limit of this process was reached
/// (`TooManyRequestsException`, `@httpError(429)`). Whether one arrives is decided by what the
/// rest of this process is running at that instant and by nothing a row varies, so a row looks
/// again on it rather than reading a verdict out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// How often to look again for work the clock owes — a stack settling, a function reaching
/// `Active`, an invocation being served — and how long to go on looking. The spans are the
/// services' own, so no fixed sleep names the instant one ends; the deadlines are generous
/// because every case asserts the CONDITION and never the time it took, so a loaded host
/// cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(50);
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SERVED_DEADLINE: Duration = Duration::from_secs(180);

/// The runtime and handler every function below declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and never how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: i64 = 120;

/// The name of the inline policy every execution role below carries.
const INLINE_POLICY_NAME: &str = "handler-grants";

/// The actions every Lambda execution role is given, whatever a row withholds: the log writes
/// a function's runtime performs ("Actions defined by Amazon CloudWatch Logs").
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The two actions the rows withhold and add, each as IAM names the listing the handler makes:
/// Amazon S3 Vectors publishes its actions under its own prefix, and the Bedrock Agent runtime
/// plane publishes its session actions under `bedrock` (that model's `awsService.arnNamespace`,
/// which is the prefix of its IAM actions).
const VECTOR_LISTING_ACTION: &str = "s3vectors:ListVectorBuckets";
const SESSION_LISTING_ACTION: &str = "bedrock:ListSessions";

/// The logical ids the templates declare, and the output keys they publish.
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Fn";
const SECOND_ROLE: &str = "SpareRole";
const SECOND_FUNCTION: &str = "SpareFn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const SECOND_FUNCTION_NAME_OUTPUT: &str = "SpareFunctionName";

/// The module every function below runs: it makes one listing call per service with the AWS SDK
/// its runtime supplies and reports, per service, either the number of entries it was served or
/// the error code the service refused the call with. Nothing of the request reaches it, so what
/// comes back is the handler's own verdict rather than an echo.
const PROBE_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def vector_buckets():
    client = boto3.client("s3vectors")
    try:
        answered = client.list_vector_buckets()
    except botocore.exceptions.ClientError as error:
        return {"ok": False, "error_code": error.response["Error"]["Code"]}
    return {"ok": True, "count": len(answered["vectorBuckets"])}


def sessions():
    client = boto3.client("bedrock-agent-runtime")
    try:
        answered = client.list_sessions()
    except botocore.exceptions.ClientError as error:
        return {"ok": False, "error_code": error.response["Error"]["Code"]}
    return {"ok": True, "count": len(answered["sessionSummaries"])}


def handler(event, context):
    return {"s3vectors": vector_buckets(), "bedrock": sessions()}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the services that own the declared resources
/// are the ones CloudFormation's in-process calls reach and the ones the handler's own SDK
/// calls are dispatched back into, so every read below is of the record the deployment wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and where each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation action over its awsQuery wire, answered with the status and the XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One Amazon S3 Vectors operation over its `restJson1` wire: a POST to the operation's own
/// URI, which is how that model binds every one of them.
async fn s3vectors(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("s3vectors.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3vectors"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Bedrock Agent runtime session call, on the method and URI the operation's own `@http`
/// binding names (`CreateSession` is `PUT /sessions/`, `ListSessions` is `POST /sessions/`),
/// signed for `bedrock` — the signing name that model declares.
async fn sessions_api(router: &MultiServiceRouter, method: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri("/sessions/")
        .header(
            "host",
            format!("bedrock-agent-runtime.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization("bedrock"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// What one role's inline document grants: the log actions, plus each listing action the caller
/// asks for, each on IAM's own any-resource wildcard — the two listings act on no single
/// resource, so a statement about them names no ARN.
fn role_document(vector_listing: bool, session_listing: bool) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": LOG_ACTIONS,
        "Resource": "*",
    })];
    if vector_listing {
        statements.push(json!({
            "Effect": "Allow",
            "Action": VECTOR_LISTING_ACTION,
            "Resource": "*",
        }));
    }
    if session_listing {
        statements.push(json!({
            "Effect": "Allow",
            "Action": SESSION_LISTING_ACTION,
            "Resource": "*",
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The trust policy every execution role below carries: AWS Lambda assumes it to run the
/// function.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One execution role as a template resource, carrying the document `grants` describes.
fn role_resource(stack: &str, suffix: &str, grants: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": format!("{stack}-{suffix}"),
            "AssumeRolePolicyDocument": trust_policy(),
            "Policies": [{
                "PolicyName": INLINE_POLICY_NAME,
                "PolicyDocument": grants,
            }],
        },
    })
}

/// One function as a template resource, running [`PROBE_SOURCE`] as the role `role` names.
fn function_resource(stack: &str, suffix: &str, role: &str) -> Value {
    json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": format!("{stack}-{suffix}"),
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Role": { "Fn::GetAtt": [role, "Arn"] },
            "Timeout": FUNCTION_TIMEOUT,
            "Code": { "ZipFile": PROBE_SOURCE },
        },
    })
}

/// The single-function template, with the execution role granted the listings asked for.
fn template(stack: &str, vector_listing: bool, session_listing: bool) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: role_resource(stack, "role", role_document(vector_listing, session_listing)),
            FUNCTION: function_resource(stack, "fn", ROLE),
        },
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
        },
    })
    .to_string()
}

/// The two-role template: one function on a role granted both listings, and a second function
/// on a role granted neither, deployed by ONE create.
fn dual_role_template(stack: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: role_resource(stack, "role", role_document(true, true)),
            SECOND_ROLE: role_resource(stack, "spare-role", role_document(false, false)),
            FUNCTION: function_resource(stack, "fn", ROLE),
            SECOND_FUNCTION: function_resource(stack, "spare-fn", SECOND_ROLE),
        },
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
            SECOND_FUNCTION_NAME_OUTPUT: { "Value": { "Ref": SECOND_FUNCTION } },
        },
    })
    .to_string()
}

/// The members every stack operation of this suite sends: the body, the two capabilities a
/// template declaring NAMED IAM resources needs acknowledged, and a tag.
fn stack_members<'a>(stack: &'a str, body: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_IAM"),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ("Tags.member.1.Key", "suite"),
        ("Tags.member.1.Value", "role-widening-update"),
    ]
}

/// Poll `DescribeStacks` the way a client's waiter does and answer the TERMINAL status the stack
/// settled on, with the answer it was read from: the first status that is not an operation in
/// flight. The wait is on that CONDITION, looking again until it holds or [`SETTLE_DEADLINE`]
/// passes — which is the rule itself, since a stack that never leaves `*_IN_PROGRESS` is what
/// the deadline catches.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS) {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack} to reach a terminal status, last saw {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The reasons the stack recorded for the resources whose own operation did not succeed, so a
/// settled status other than the expected one is reported with what caused it.
async fn failure_reasons(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let (status, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {events}"
    );
    elements(&events, "ResourceStatusReason")
}

/// Assert the stack settled on `wanted`, and answer the `DescribeStacks` answer it settled in.
async fn settled_on(router: &MultiServiceRouter, case: &str, stack: &str, wanted: &str) -> String {
    let (reached, described) = settled(router, case, stack).await;
    if reached != wanted {
        let reasons = failure_reasons(router, case, stack).await;
        panic!(
            "case {case:?}: expected {stack} to settle on {wanted}, got {reached} with \
             resource failures {reasons:?} in {described}"
        );
    }
    described
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function ready to be invoked
/// — `Active` with no update still in flight — and fail, naming the deadline and what it last
/// reported, when it does not. The span a function spends coming up is that service's own, so
/// no fixed sleep names the instant it ends.
async fn ready(router: &MultiServiceRouter, case: &str, function: &str) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {function:?}, \
             got {status} with {configuration}"
        );
        let state = configuration.get("State").and_then(Value::as_str);
        let updating = configuration.get("LastUpdateStatus").and_then(Value::as_str);
        if state == Some(STATE_ACTIVE) && updating != Some(UPDATE_IN_PROGRESS) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected Lambda to report {function:?} ready to invoke, last \
             reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Invoke `function` and answer the payload the handler returned, looking again while this
/// process answers that it did not run the invocation at all ([`UNSERVED`]).
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str) -> Value {
    let giving_up_at = Instant::now() + SERVED_DEADLINE;
    loop {
        let (status, headers, payload) = lambda(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            b"{}",
        )
        .await;
        if status == UNSERVED {
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected an invocation of {function:?} to be served, last \
                 answered {status} with {payload}"
            );
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke on {function:?}, got {status} with \
             {payload}"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError from {function:?}, got {signal:?} with \
             {payload}"
        );
        return payload;
    }
}

/// How many vector buckets Amazon S3 Vectors itself reports for this account right now — what
/// a handler allowed to make that listing has to count.
async fn vector_bucket_count(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, answered) = s3vectors(router, "ListVectorBuckets", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListVectorBuckets, got {status} with {answered}"
    );
    answered["vectorBuckets"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("case {case:?}: no vectorBuckets list in {answered}"))
}

/// How many sessions the Bedrock Agent runtime plane itself reports for this account right now
/// — what a handler allowed to make that listing has to count.
async fn session_count(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, answered) = sessions_api(router, "POST", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListSessions, got {status} with {answered}"
    );
    answered["sessionSummaries"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("case {case:?}: no sessionSummaries list in {answered}"))
}

/// What one of the handler's two listings must report: the number of entries the owning service
/// answers that same listing with when a statement in force allows the action, and the AWS
/// refusal's own error code when none does.
fn expected_listing(allowed: bool, held: usize) -> Value {
    if allowed {
        json!({ "ok": true, "count": held })
    } else {
        json!({ "ok": false, "error_code": ACCESS_DENIED })
    }
}

/// Invoke `function` and assert each of its two listings was decided the way the statements in
/// force decide it. `when` names the point in the stack's life the invocation was made at, so a
/// failure says which side of an update it is about.
async fn verdicts(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    function: &str,
    vector_allowed: bool,
    session_allowed: bool,
) {
    let case = format!("{case} ({when})");
    ready(router, &case, function).await;
    let payload = invoked(router, &case, function).await;
    let expected = json!({
        "s3vectors": expected_listing(
            vector_allowed,
            vector_bucket_count(router, &case).await,
        ),
        "bedrock": expected_listing(session_allowed, session_count(router, &case).await),
    });
    assert_eq!(
        payload, expected,
        "case {case:?}: expected {function}'s own verdicts {expected}, got {payload}"
    );
}

/// Create the two resources the handlers count, so a served listing reports a number the
/// emulator's state really holds rather than an empty one: one vector bucket and one session,
/// each created through the owning service's own API.
async fn seed(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, created) = s3vectors(
        router,
        "CreateVectorBucket",
        &json!({ "vectorBucketName": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVectorBucket, got {status} with {created}"
    );
    let (status, opened) = sessions_api(router, "PUT", &json!({})).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateSession, got {status} with {opened}"
    );
}

/// The output value a stack published under `key`.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
}

/// One row: which listings the create-time role grants, and which the update's role grants.
struct Row {
    case: &'static str,
    vector_at_create: bool,
    session_at_create: bool,
    vector_after_update: bool,
    session_after_update: bool,
}

/// The rows: every combination of the two listings at create time, and an update that moves the
/// grant in each direction.
const ROWS: &[Row] = &[
    Row {
        case: "neither listing granted, both added by the update",
        vector_at_create: false,
        session_at_create: false,
        vector_after_update: true,
        session_after_update: true,
    },
    Row {
        case: "only the vector listing granted, the session listing added by the update",
        vector_at_create: true,
        session_at_create: false,
        vector_after_update: true,
        session_after_update: true,
    },
    Row {
        case: "only the session listing granted, the vector listing added by the update",
        vector_at_create: false,
        session_at_create: true,
        vector_after_update: true,
        session_after_update: true,
    },
    Row {
        case: "both listings granted, the session listing taken away by the update",
        vector_at_create: true,
        session_at_create: true,
        vector_after_update: true,
        session_after_update: false,
    },
];

/// One create, one invocation, one update and one further invocation per row.
#[tokio::test]
async fn a_stack_update_decides_the_deployed_handlers_own_listings() {
    // ONE emulator for every row, so the refused rows and the served ones are stacks of the
    // same account: a refusal here cannot be an account in which nothing works.
    let router = build_router();
    seed(&router, "the resources every row counts", "widening-store").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-role-widening-{index}");

        let created = template(&stack, row.vector_at_create, row.session_at_create);
        let (status, answered) =
            cloudformation(&router, "CreateStack", &stack_members(&stack, &created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, &stack, CREATE_COMPLETE).await;

        // What the stack HAS: a physical id for each resource it declared, in the services
        // that own them.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let physical = physical_ids_of(&resources);
        for logical in [ROLE, FUNCTION] {
            assert!(
                physical.contains_key(logical),
                "case {case:?}: expected a physical id for {logical}, got {physical:?}"
            );
        }

        let outputs = outputs_of(&described);
        let function = output(&outputs, case, FUNCTION_NAME_OUTPUT);
        verdicts(
            &router,
            case,
            "as created",
            &function,
            row.vector_at_create,
            row.session_at_create,
        )
        .await;

        // The same template with the role's statements moved, through the operation a client
        // widens or narrows a least-privilege role with.
        let updated = template(&stack, row.vector_after_update, row.session_after_update);
        let (status, answered) =
            cloudformation(&router, "UpdateStack", &stack_members(&stack, &updated)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        settled_on(&router, case, &stack, UPDATE_COMPLETE).await;

        verdicts(
            &router,
            case,
            "after the update",
            &function,
            row.vector_after_update,
            row.session_after_update,
        )
        .await;
    }
}

/// ONE create carrying two roles and two functions: the permitted function is served both
/// listings and its twin is refused both, so the verdict follows the role each function runs as
/// and not the stack they share.
#[tokio::test]
async fn two_functions_of_one_stack_are_decided_by_their_own_roles() {
    let case = "one stack, a permitted function and an under-privileged twin";
    let router = build_router();
    seed(&router, case, "dual-role-store").await;

    let stack = "cfn-role-mismatch";
    let body = dual_role_template(stack);
    let (status, answered) =
        cloudformation(&router, "CreateStack", &stack_members(stack, &body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
    );
    let described = settled_on(&router, case, stack, CREATE_COMPLETE).await;

    let outputs = outputs_of(&described);
    let permitted = output(&outputs, case, FUNCTION_NAME_OUTPUT);
    let under_privileged = output(&outputs, case, SECOND_FUNCTION_NAME_OUTPUT);
    assert_ne!(
        permitted, under_privileged,
        "case {case:?}: expected the stack to publish two different function names, got \
         {outputs:?}"
    );

    verdicts(&router, case, "the permitted role", &permitted, true, true).await;
    verdicts(
        &router,
        case,
        "the role granted neither listing",
        &under_privileged,
        false,
        false,
    )
    .await;
}
