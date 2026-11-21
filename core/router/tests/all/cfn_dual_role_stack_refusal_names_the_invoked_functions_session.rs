// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares TWO IAM execution roles and TWO Lambda functions — one
//! function per role, deployed by ONE create — settles, and each deployed function's own SDK
//! calls are refused or served according to the role THAT function runs as, with the refusal
//! naming the session the refused call was made under.
//!
//! RULE: **every stack operation CloudFormation accepts reaches a TERMINAL status
//! `DescribeStacks` publishes — `CREATE_COMPLETE` or a rollback for a create — however many
//! roles and functions one template declares, so a client polling it always terminates; and a
//! call a running function's AWS SDK makes is authorized against the identity-based documents
//! in force on the execution role THAT function runs as, served when some statement in force
//! allows the action and otherwise refused with `AccessDeniedException` (HTTP 403) worded in
//! IAM's own sentence — `User: <principal> is not authorized to perform: <action>`, the
//! resource clause where the call addresses one, then the clause naming the rule that withheld
//! the grant — whose principal is the assumed-role session of that function's own role
//! (`arn:aws:sts::<account>:assumed-role/<role>/<function>`) and whose action is the fully
//! qualified action of the refused call.**
//!
//! Two functions of one stack therefore get their OWN verdicts and their OWN sentences: the
//! refusal a least-privilege function reads blames its role and its invocation, never the
//! permitted twin beside it in the same template, and a client reads out of it which role to
//! widen.
//!
//! Six crates have to agree for that to hold — CloudFormation drives the deployment and decides
//! the stack's status, IAM holds both roles and their documents, the token service vends each
//! function's session, Lambda RUNS the handlers with the AWS SDK its runtime supplies, and
//! Amazon S3 Vectors and the Amazon Bedrock Agent runtime plane each take their own decision and
//! own the entries a served listing counts — so the suite lives here rather than in any one of
//! them. Every observation is made where a client makes one: `DescribeStacks`,
//! `DescribeStackResources`, `GetFunctionConfiguration`, `Invoke`, and the called services' own
//! listings; never an emulator internal.
//!
//! The rows differ along what the rule generalises over: WHICH of the two called services' actions
//! the second role withholds (both of them, the session listing only, the vector listing only), so
//! neither the refusal nor the sentence rests on one service, and every row holds a function that
//! is served beside one that is refused inside a single stack. The sentences are built here from
//! the role name the stack reports for the role the function points at and the function name the
//! stack publishes, so no expected value is a literal of one run; a served listing's count is read
//! off the OWNING service's own listing for the same reason.
//!
//! `cfn_role_widening_update_decides_a_functions_listings` pins which DIRECTION a stack update may
//! move a grant in and `cfn_widened_role_listing_counts_are_stable` how much a served listing has
//! to count; both hold the refusal against its error code alone, which is the half this suite
//! takes further — what the refused handler is TOLD. `lambda_execution_role_decides_every_service_it_calls`
//! pins the same sentence for two other services' actions on ONE role; what only this suite
//! reaches is two roles and two functions of ONE stack, where the sentence has a wrong session to
//! name.

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

/// The account this emulator serves, which every ARN and every session below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where Lambda binds `GetFunctionConfiguration` and `Invoke`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The terminal status an accepted create must settle on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The suffix of every status CloudFormation reports for an operation still in flight, and so the
/// one status class a poll for a terminal status looks past.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The `State` an invocation needs the function to be in ("the function is ready to be invoked",
/// `FunctionConfiguration$State`), and the `LastUpdateStatus` it must be past.
const STATE_ACTIVE: &str = "Active";
const UPDATE_IN_PROGRESS: &str = "InProgress";

/// The error code and HTTP status a call no document in force allows is refused with: the
/// AWS-common `AccessDeniedException`, which both called models bind at `@httpError(403)`
/// (`smithy_query(query="operation", operation="ListVectorBuckets")` and
/// `…, operation="ListSessions")`), and which the SDK hands the handler as its `Error.Code` and
/// its `ResponseMetadata.HTTPStatusCode`.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The status Lambda answers an invocation it has NOT served with, which carries no answer of the
/// handler's: the concurrent-execution limit of this process was reached
/// (`TooManyRequestsException`, `@httpError(429)`). Whether one arrives is decided by what the rest
/// of this process is running at that instant and by nothing a row varies, so a row looks again on
/// it rather than reading a verdict out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// The `errorType` classes of a run THIS PROCESS's own bounds ended rather than the handler: the
/// sandbox stopping a run at the host wall clock an invocation is given, and the memory bound read
/// from the whole process. Neither says anything about what the handler came to, so an invocation
/// answered with one is made again; every OTHER function error fails the row.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";
const RUNTIME_EXIT_ERROR: &str = "Runtime.ExitError";
const RUNTIME_KILLED: &str = "signal: killed";

/// How often to look again for work the clock owes — a stack settling, a function reaching
/// `Active`, an invocation being served — and how long to go on looking. The spans are the
/// services' own, so no fixed sleep names the instant one ends; the deadlines are generous because
/// every case asserts the CONDITION and never the time it took, so a loaded host cannot decide an
/// outcome.
const LOOK: Duration = Duration::from_millis(50);
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SERVED_DEADLINE: Duration = Duration::from_secs(180);

/// The runtime and handler every function below declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs on
/// any host, because what is under test is the authorization decision and its wording, never how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: i64 = 120;

/// The name of the inline policy every execution role below carries.
const INLINE_POLICY_NAME: &str = "handler-grants";

/// The actions every execution role is given whatever a row withholds: the log writes a function's
/// runtime performs ("Actions defined by Amazon CloudWatch Logs").
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The two listings each handler makes, as IAM names them: Amazon S3 Vectors publishes its actions
/// under its own prefix, and the Bedrock Agent runtime plane publishes its session actions under
/// `bedrock` (that model's `awsService.arnNamespace`, which is the prefix of its IAM actions).
const VECTOR_LISTING_ACTION: &str = "s3vectors:ListVectorBuckets";
const SESSION_LISTING_ACTION: &str = "bedrock:ListSessions";

/// The logical ids both templates declare, and the output keys they publish.
const PERMITTED_ROLE: &str = "GoodRole";
const MISMATCHED_ROLE: &str = "BadRole";
const PERMITTED_FUNCTION: &str = "GoodFn";
const MISMATCHED_FUNCTION: &str = "BadFn";
const PERMITTED_FUNCTION_OUTPUT: &str = "GoodFunctionName";
const MISMATCHED_FUNCTION_OUTPUT: &str = "BadFunctionName";

/// The module every function below runs: it makes one listing per called service with the AWS SDK
/// its runtime supplies and reports, per service, either the number of entries it was served or the
/// whole refusal the SDK handed it — the error code, the sentence and the HTTP status. Nothing of
/// the request reaches it, so what comes back is what that service answered rather than an echo.
const PROBE_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def probe(listing):
    try:
        answered = listing()
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {"ok": True, "count": len(answered)}


def handler(event, context):
    vectors = boto3.client("s3vectors")
    agents = boto3.client("bedrock-agent-runtime")
    return {
        "s3vectors": probe(lambda: vectors.list_vector_buckets()["vectorBuckets"]),
        "bedrock": probe(lambda: agents.list_sessions()["sessionSummaries"]),
    }
"#;

/// The emulator as it is ASSEMBLED, on one engine: the services that own the declared resources are
/// the ones CloudFormation's in-process calls reach and the ones each handler's own SDK calls are
/// dispatched back into, so every read below is of the record the deployment wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request. Each HANDLER's own calls are signed by the runtime, with the
    // credential its invocation was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request and where each handler reads its Region from. Its credential is the
/// long-term key this emulator publishes, so every call the suite makes on its own behalf is the
/// account's and not any row's role's.
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

/// One Amazon S3 Vectors operation over its `restJson1` wire: a POST to the operation's own URI,
/// which is how that model binds every one of them.
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
/// binding names (`CreateSession` is `PUT /sessions/`, `ListSessions` is `POST /sessions/`), signed
/// for `bedrock` — the signing name that model declares.
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

/// What one role's inline document grants: the log actions, plus each listing action asked for, on
/// IAM's own any-resource wildcard — neither listing acts on a single resource, so a statement
/// about it names no ARN.
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

/// One execution role as a template resource, named after the stack and carrying `grants`. AWS
/// Lambda assumes it to run the function bound to it.
fn role_resource(stack: &str, suffix: &str, grants: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": format!("{stack}-{suffix}"),
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "lambda.amazonaws.com" },
                    "Action": "sts:AssumeRole",
                }],
            },
            "Policies": [{
                "PolicyName": INLINE_POLICY_NAME,
                "PolicyDocument": grants,
            }],
        },
    })
}

/// One function as a template resource, running [`PROBE_SOURCE`] as the role `role` names — the
/// IDENTICAL handler for both functions of a stack, so the only thing that can decide their calls
/// differently is the role each runs as.
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

/// The template one row deploys: the permitted role granted both listings, the mismatched role
/// granted what the row leaves it, and one function bound to each — four resources in ONE create,
/// with both function names published as outputs.
fn template(stack: &str, row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            PERMITTED_ROLE: role_resource(stack, "good-role", role_document(true, true)),
            MISMATCHED_ROLE: role_resource(
                stack,
                "bad-role",
                role_document(row.mismatched_vector, row.mismatched_session),
            ),
            PERMITTED_FUNCTION: function_resource(stack, "good-fn", PERMITTED_ROLE),
            MISMATCHED_FUNCTION: function_resource(stack, "bad-fn", MISMATCHED_ROLE),
        },
        "Outputs": {
            PERMITTED_FUNCTION_OUTPUT: { "Value": { "Ref": PERMITTED_FUNCTION } },
            MISMATCHED_FUNCTION_OUTPUT: { "Value": { "Ref": MISMATCHED_FUNCTION } },
        },
    })
    .to_string()
}

/// The members every stack operation of this suite sends: the body, the two capabilities a template
/// declaring NAMED IAM resources needs acknowledged, and a tag.
fn stack_members<'a>(stack: &'a str, body: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_IAM"),
        ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ("Tags.member.1.Key", "suite"),
        ("Tags.member.1.Value", "dual-role-refusal"),
    ]
}

/// Poll `DescribeStacks` the way a client's waiter does and answer the TERMINAL status the stack
/// settled on, with the answer it was read from: the first status that is not an operation in
/// flight. The wait is on that CONDITION, looking again until it holds or [`SETTLE_DEADLINE`]
/// passes — a stack that never leaves `*_IN_PROGRESS` is what the deadline catches.
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
            "case {case:?}: expected {stack} to settle on {wanted}, got {reached} with resource \
             failures {reasons:?} in {described}"
        );
    }
    described
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function ready to be invoked —
/// `Active` with no update still in flight — and fail, naming what it last reported, when it does
/// not. The span a function spends coming up is that service's own, so no fixed sleep names the
/// instant it ends.
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
            "case {case:?}: expected HTTP 200 from GetFunctionConfiguration on {function:?}, got \
             {status} with {configuration}"
        );
        let state = configuration.get("State").and_then(Value::as_str);
        let updating = configuration.get("LastUpdateStatus").and_then(Value::as_str);
        if state == Some(STATE_ACTIVE) && updating != Some(UPDATE_IN_PROGRESS) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected Lambda to report {function:?} ready to invoke, last reported \
             {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Whether an answered function error reports one of THIS PROCESS's own bounds rather than what the
/// handler came to.
fn host_bound(payload: &Value) -> bool {
    let class = payload["errorType"].as_str().unwrap_or_default();
    let message = payload["errorMessage"].as_str().unwrap_or_default();
    class == SANDBOX_TIMEDOUT || (class == RUNTIME_EXIT_ERROR && message.contains(RUNTIME_KILLED))
}

/// Invoke `function` and answer the payload the handler returned, looking again while this process
/// answers that it did not run the invocation at all ([`UNSERVED`]) or ended the run on one of its
/// own bounds ([`host_bound`]) — neither of which is an outcome of the deployed function.
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
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if Instant::now() < giving_up_at
            && (status == UNSERVED || (signal.is_some() && host_bound(&payload)))
        {
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke on {function:?}, got {status} with \
             {payload}"
        );
        // A refusal the handler CAUGHT is not a failure of the run: what decides this header is
        // whether the handler returned.
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError from {function:?}, got {signal:?} with \
             {payload}"
        );
        return payload;
    }
}

/// How many vector buckets Amazon S3 Vectors itself reports for this account right now — what a
/// handler allowed to make that listing has to count.
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

/// How many sessions the Bedrock Agent runtime plane itself reports for this account right now —
/// what a handler allowed to make that listing has to count.
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

/// Give the called services one entry each, through the OWNING service's own API, so a served
/// listing reports a number the emulator's state really holds and a refusal cannot be an account in
/// which there is nothing to list.
async fn seed(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, created) = s3vectors(
        router,
        "CreateVectorBucket",
        &json!({ "vectorBucketName": bucket }),
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

/// The value a stack published under `key`, and the physical id it reports for `logical`.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
}
fn reported(physical: &BTreeMap<String, String>, case: &str, logical: &str) -> String {
    physical
        .get(logical)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no physical id for {logical}, got {physical:?}"))
}

/// The identity a call made by an invocation of `function` running as `role` speaks for: the
/// session a function's calls are made under is named for the FUNCTION, so a refusal says which
/// function, and which of a stack's roles, was refused. Both parts come from what the stack
/// reported for the row rather than from a literal.
fn session_arn(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// The resource the session listing addresses: every session of the account in the Region the
/// caller signed for, which an identity-based statement names with IAM's own wildcard. The vector
/// listing addresses none — it reports "the vector buckets that are owned by the authenticated
/// sender of the request" (`ListVectorBuckets`) — so its refusal states no resource clause.
fn all_sessions_arn() -> String {
    format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:session/*")
}

/// What the handler reports for one listing: the count the owning service holds when a statement in
/// force allows the action, and the whole refusal when none does — IAM's sentence naming the
/// session the call was made under and the action it asked for, the code and the status.
fn expected_listing(
    allowed: bool,
    held: usize,
    session: &str,
    action: &str,
    resource: Option<&str>,
) -> Value {
    if allowed {
        return json!({ "ok": true, "count": held });
    }
    let reason = format!("because no identity-based policy allows the {action} action");
    let message = match resource {
        Some(resource) => format!(
            "User: {session} is not authorized to perform: {action} on resource: {resource} \
             {reason}"
        ),
        None => format!("User: {session} is not authorized to perform: {action} {reason}"),
    };
    json!({
        "ok": false,
        "error_code": ACCESS_DENIED,
        "message": message,
        "status": ACCESS_DENIED_STATUS,
    })
}

/// Invoke `function`, which runs as `role`, and assert each of its two listings was decided — and
/// worded — the way the statements in force on THAT role decide it. `when` names which of the
/// stack's two functions the invocation was of, so a failure says which one it is about.
async fn verdicts(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    role: &str,
    function: &str,
    vector_allowed: bool,
    session_allowed: bool,
) {
    let case = format!("{case} ({when})");
    ready(router, &case, function).await;
    let session = session_arn(role, function);
    let payload = invoked(router, &case, function).await;
    let expected = json!({
        "s3vectors": expected_listing(
            vector_allowed,
            vector_bucket_count(router, &case).await,
            &session,
            VECTOR_LISTING_ACTION,
            None,
        ),
        "bedrock": expected_listing(
            session_allowed,
            session_count(router, &case).await,
            &session,
            SESSION_LISTING_ACTION,
            Some(&all_sessions_arn()),
        ),
    });
    assert_eq!(
        payload, expected,
        "case {case:?}: expected {function}, running as {role}, to be answered {expected}, got \
         {payload}"
    );
}

/// One row: what the mismatched role's document grants of the two listings. The permitted role of
/// every row grants both.
struct Row {
    case: &'static str,
    mismatched_vector: bool,
    mismatched_session: bool,
}

/// The rows: the mismatched role withholding both listings, then each one of them on its own — so
/// the refusal and its sentence are read for each called service, beside a grant of the other.
const ROWS: &[Row] = &[
    Row {
        case: "a permitted function and a twin whose role grants neither listing",
        mismatched_vector: false,
        mismatched_session: false,
    },
    Row {
        case: "a permitted function and a twin whose role grants only the vector listing",
        mismatched_vector: true,
        mismatched_session: false,
    },
    Row {
        case: "a permitted function and a twin whose role grants only the session listing",
        mismatched_vector: false,
        mismatched_session: true,
    },
];

/// One create of a two-role, two-function stack per row, and one invocation of each function it
/// deployed.
#[tokio::test]
async fn each_functions_refusal_names_the_role_and_session_it_ran_as() {
    // ONE emulator for every row, so the refused functions and the served ones are of the same
    // account: a refusal here cannot be an account in which nothing works.
    let router = build_router();
    seed(&router, "the entries every row counts", "dual-role-refusal-store").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-dual-role-{index}");

        let body = template(&stack, row);
        let (status, answered) =
            cloudformation(&router, "CreateStack", &stack_members(&stack, &body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, &stack, CREATE_COMPLETE).await;

        // What the stack HAS: a physical id for each of the four resources it declared, which for
        // a role is the role NAME a refusal's session is built from.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let physical = physical_ids_of(&resources);
        let permitted_role = reported(&physical, case, PERMITTED_ROLE);
        let mismatched_role = reported(&physical, case, MISMATCHED_ROLE);
        for logical in [PERMITTED_FUNCTION, MISMATCHED_FUNCTION] {
            assert!(
                physical.contains_key(logical),
                "case {case:?}: expected a physical id for {logical}, got {physical:?}"
            );
        }
        assert_ne!(
            permitted_role, mismatched_role,
            "case {case:?}: expected the stack to report two different roles, got {physical:?}"
        );

        let outputs = outputs_of(&described);
        let permitted = output(&outputs, case, PERMITTED_FUNCTION_OUTPUT);
        let mismatched = output(&outputs, case, MISMATCHED_FUNCTION_OUTPUT);
        assert_ne!(
            permitted, mismatched,
            "case {case:?}: expected the stack to publish two different function names, got \
             {outputs:?}"
        );

        verdicts(
            &router,
            case,
            "the function on the permitted role",
            &permitted_role,
            &permitted,
            true,
            true,
        )
        .await;
        verdicts(
            &router,
            case,
            "the function on the mismatched role",
            &mismatched_role,
            &mismatched,
            row.mismatched_vector,
            row.mismatched_session,
        )
        .await;
    }
}
