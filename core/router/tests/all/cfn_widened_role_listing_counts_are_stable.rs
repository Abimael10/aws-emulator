// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed function's own SDK listing answers is a REPORT of what the
//! called service holds — so the same invocation, repeated, answers the same thing, and the
//! listing leaves the called service's own answer alone.
//!
//! RULE: **a listing a deployed function makes is authorized against the statements in force
//! on its execution role at the instant of the call — refused with `AccessDeniedException`
//! while no statement allows the action, served the moment a stack update adds one — and a
//! SERVED listing reports the number of entries the OWNING service itself holds at that
//! instant, whatever that number is; because a listing only reports, making it again answers
//! the same count, within one run and across back-to-back invocations, and the owning
//! service's own listing is the same after those invocations as before them.**
//!
//! Five crates have to agree for that to hold — CloudFormation drives the deployment and
//! decides the stack's status, IAM holds the role and its documents, STS vends the role's
//! session, Lambda RUNS the handler with the AWS SDK its runtime supplies, and the called
//! service takes the decision and owns the entries being counted — so the suite lives here
//! rather than in any one of them. Every observation is made where a client makes one:
//! `DescribeStacks`, `Invoke`, `GetFunctionConfiguration` and the called services' own
//! listings; never an emulator internal.
//!
//! The rows differ along what the rule generalises over: HOW MANY entries the called services
//! hold when the listing is made (none, one, several) and HOW MANY times the identical
//! invocation is repeated at each observation point (two, three) — so no verdict rests on a
//! singleton collection or on a single call. Each invocation also makes each listing
//! [`READS_PER_INVOCATION`] times inside ONE run, so a count that moved because something was
//! read would differ between the two numbers one payload carries, between two payloads of one
//! observation point, or between the handler's count and what the owning service answers the
//! same listing with — and every expected count is read off the owning service's own API here,
//! so none is a literal.
//!
//! `cfn_role_widening_update_decides_a_functions_listings` pins WHICH actions a role's
//! documents decide and in which direction an update may move a grant, holding ONE entry per
//! service and ONE invocation per observation; `cfn_agent_function_stack_settles` pins how many
//! owning services one settling template may span. This suite holds the grant pattern fixed —
//! one action withheld, added by the update — and varies the two things those leave alone: how
//! much there is to count, and how often it is counted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this test crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{elements, form_encode, opt_element, outputs_of};

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
/// `AccessDeniedException`, which both called models bind at `@httpError(403)` and which the
/// SDK hands the handler as its `Error.Code`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The status Lambda answers an invocation it has NOT served with, which carries no answer of
/// the handler's: the concurrent-execution limit of this process was reached
/// (`TooManyRequestsException`, `@httpError(429)`). Whether one arrives is decided by what the
/// rest of this process is running at that instant and by nothing a row varies, so a row looks
/// again on it rather than reading a verdict out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// The `errorType` classes of a run THIS PROCESS's own bounds ended rather than the handler:
/// the sandbox stopping a run at the host wall clock an invocation is given, and the memory
/// bound read from the whole process. Neither says anything about what the handler came to, so
/// an invocation answered with one is made again; every OTHER function error fails the row.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";
const RUNTIME_EXIT_ERROR: &str = "Runtime.ExitError";
const RUNTIME_KILLED: &str = "signal: killed";

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
/// needs on any host, because what is under test is what the listings answer and never how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: i64 = 120;

/// How many times the handler makes EACH listing inside one run. Two is enough for the
/// invariant — a count that changed because it was read differs between the first and the
/// second — and the number is stated once, here, because the handler's loop and the payload
/// this suite expects are both written from it.
const READS_PER_INVOCATION: usize = 2;

/// The name of the inline policy every execution role below carries.
const INLINE_POLICY_NAME: &str = "handler-grants";

/// The actions every execution role here is given whatever a row does: the log writes a
/// function's runtime performs ("Actions defined by Amazon CloudWatch Logs").
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The two listings the handler makes, as IAM names them: Amazon S3 Vectors publishes its
/// actions under its own prefix, and the Bedrock Agent runtime plane publishes its session
/// actions under `bedrock` (that model's `awsService.arnNamespace`, which is the prefix of its
/// IAM actions). The first is the one the create withholds and the update adds; the second is
/// granted throughout, so every row watches a grant it does not move as well as one it does.
const VECTOR_LISTING_ACTION: &str = "s3vectors:ListVectorBuckets";
const SESSION_LISTING_ACTION: &str = "bedrock:ListSessions";

/// The logical ids the template declares, and the output key it publishes.
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Fn";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";

/// The emulator as it is ASSEMBLED, on one engine: the services that own the declared
/// resources are the ones CloudFormation's in-process calls reach and the ones the handler's
/// own SDK calls are dispatched back into, so every read below is of the record the deployment
/// wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// The module every function below runs: per called service it makes the SAME listing
/// [`READS_PER_INVOCATION`] times with the AWS SDK its runtime supplies, and reports either
/// every count it was served or the error code the service refused the call with, with how many
/// reads it had got through when it was refused. Nothing of the request reaches it, so what
/// comes back is the handler's own account of what it was answered rather than an echo.
fn probe_source() -> String {
    format!(
        r#"
import boto3
import botocore.exceptions


def probe(listing):
    counts = []
    for _ in range({READS_PER_INVOCATION}):
        try:
            answered = listing()
        except botocore.exceptions.ClientError as error:
            return {{
                "ok": False,
                "error_code": error.response["Error"]["Code"],
                "reads": len(counts),
            }}
        counts.append(len(answered))
    return {{"ok": True, "counts": counts}}


def vector_buckets():
    client = boto3.client("s3vectors")
    return probe(lambda: client.list_vector_buckets()["vectorBuckets"])


def sessions():
    client = boto3.client("bedrock-agent-runtime")
    return probe(lambda: client.list_sessions()["sessionSummaries"])


def handler(event, context):
    return {{"s3vectors": vector_buckets(), "bedrock": sessions()}}
"#
    )
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

/// What one role's inline document grants: the log actions, the session listing, and — once the
/// update has widened it — the vector listing, each on IAM's own any-resource wildcard, since
/// neither listing acts on a single resource.
fn role_document(vector_listing: bool) -> Value {
    let mut statements = vec![
        json!({ "Effect": "Allow", "Action": LOG_ACTIONS, "Resource": "*" }),
        json!({ "Effect": "Allow", "Action": SESSION_LISTING_ACTION, "Resource": "*" }),
    ];
    if vector_listing {
        statements.push(json!({
            "Effect": "Allow",
            "Action": VECTOR_LISTING_ACTION,
            "Resource": "*",
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The template one row deploys and then updates: the execution role AWS Lambda assumes to run
/// the function, and the function itself, whose name the stack publishes.
fn template(stack: &str, vector_listing: bool) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{stack}-role"),
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
                        "PolicyDocument": role_document(vector_listing),
                    }],
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-fn"),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": probe_source() },
                },
            },
        },
        "Outputs": {
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION } },
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
        ("Tags.member.1.Value", "listing-count-stability"),
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
            "case {case:?}: expected {stack} to settle on {wanted}, got {reached} with \
             resource failures {reasons:?} in {described}"
        );
    }
    described
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function ready to be invoked
/// — `Active` with no update still in flight — and fail, naming what it last reported, when it
/// does not. The span a function spends coming up is that service's own, so no fixed sleep
/// names the instant it ends.
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

/// Whether an answered function error reports one of THIS PROCESS's own bounds rather than what
/// the handler came to.
fn host_bound(payload: &Value) -> bool {
    let class = payload["errorType"].as_str().unwrap_or_default();
    let message = payload["errorMessage"].as_str().unwrap_or_default();
    class == SANDBOX_TIMEDOUT || (class == RUNTIME_EXIT_ERROR && message.contains(RUNTIME_KILLED))
}

/// Invoke `function` and answer the payload the handler returned, looking again while this
/// process answers that it did not run the invocation at all ([`UNSERVED`]) or ended the run on
/// one of its own bounds ([`host_bound`]) — neither of which is an outcome of the deployed
/// function.
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
    assert_eq!(
        answered.get("nextToken"),
        None,
        "case {case:?}: this suite holds fewer buckets than one page, so the whole listing is \
         one answer, got {answered}"
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
    assert_eq!(
        answered.get("nextToken"),
        None,
        "case {case:?}: this suite holds fewer sessions than the default page \
         (`ListSessionsRequest$maxResults` defaults to 10), so the whole listing is one \
         answer, got {answered}"
    );
    answered["sessionSummaries"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("case {case:?}: no sessionSummaries list in {answered}"))
}

/// What the handler reports for one listing: every read of a served one answers the count the
/// owning service holds, and a refused one carries AWS's own error code and the number of reads
/// that had been served before it — none, since the role either allows the action for the whole
/// run or for none of it.
fn expected_listing(allowed: bool, held: usize) -> Value {
    if allowed {
        json!({ "ok": true, "counts": vec![held; READS_PER_INVOCATION] })
    } else {
        json!({ "ok": false, "error_code": ACCESS_DENIED, "reads": 0 })
    }
}

/// Invoke `function` `invocations` times back to back and hold every payload against what the
/// owning services hold, then read those services again: the counts the handler reports agree
/// with theirs, agree with each other across the invocations, and leave their own listings as
/// they were. `when` names the point in the stack's life the invocations were made at, so a
/// failure says which side of the update it is about.
async fn stable_verdicts(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    function: &str,
    invocations: usize,
    vector_allowed: bool,
) {
    let case = format!("{case} ({when})");
    ready(router, &case, function).await;

    let buckets_held = vector_bucket_count(router, &case).await;
    let sessions_held = session_count(router, &case).await;
    let expected = json!({
        "s3vectors": expected_listing(vector_allowed, buckets_held),
        "bedrock": expected_listing(true, sessions_held),
    });

    for attempt in 1..=invocations {
        let payload = invoked(router, &case, function).await;
        assert_eq!(
            payload, expected,
            "case {case:?}: invocation {attempt} of {invocations} of {function} expected \
             {expected}, got {payload}"
        );
    }

    let buckets_after = vector_bucket_count(router, &case).await;
    let sessions_after = session_count(router, &case).await;
    assert_eq!(
        buckets_after, buckets_held,
        "case {case:?}: a listing reports, so {invocations} invocations of {function} leave \
         the vector buckets as they were, expected {buckets_held}, got {buckets_after}"
    );
    assert_eq!(
        sessions_after, sessions_held,
        "case {case:?}: a listing reports, so {invocations} invocations of {function} leave \
         the sessions as they were, expected {sessions_held}, got {sessions_after}"
    );
}

/// Give the called services the entries this row's handler has to count, each created through
/// the OWNING service's own API, so a served listing reports a number the emulator's state
/// really holds.
async fn seed(router: &MultiServiceRouter, case: &str, stack: &str, row: &Row) {
    for index in 0..row.buckets {
        let (status, created) = s3vectors(
            router,
            "CreateVectorBucket",
            &json!({ "vectorBucketName": format!("{stack}-store-{index}") }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateVectorBucket, got {status} with \
             {created}"
        );
    }
    for _ in 0..row.sessions {
        let (status, opened) = sessions_api(router, "PUT", &json!({})).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateSession, got {status} with {opened}"
        );
    }
}

/// The output value a stack published under `key`.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
}

/// One row: how many entries this row adds to each called service before it deploys, and how
/// many times it repeats the identical invocation at each observation point.
struct Row {
    case: &'static str,
    buckets: usize,
    sessions: usize,
    invocations: usize,
}

/// The rows, run in order against ONE emulator — so a row counting entries counts the ones its
/// predecessors created as well, which is why every expected count is read off the owning
/// service rather than written here. The counts stay well inside one page of either listing.
const ROWS: &[Row] = &[
    Row {
        case: "neither service holding anything, invoked twice at each point",
        buckets: 0,
        sessions: 0,
        invocations: 2,
    },
    Row {
        case: "one entry added to each service, invoked twice at each point",
        buckets: 1,
        sessions: 1,
        invocations: 2,
    },
    Row {
        case: "several entries added to each service, invoked three times at each point",
        buckets: 3,
        sessions: 2,
        invocations: 3,
    },
];

/// One create, one update, and `invocations` back-to-back invocations on either side of it per
/// row.
#[tokio::test]
async fn a_deployed_handlers_listing_counts_are_stable_across_back_to_back_invocations() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-listing-stability-{index}");
        seed(&router, case, &stack, row).await;

        // Deployed with the vector listing WITHHELD: the role allows the session listing and
        // the log writes, and nothing else.
        let withheld = template(&stack, false);
        let (status, answered) =
            cloudformation(&router, "CreateStack", &stack_members(&stack, &withheld)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_on(&router, case, &stack, CREATE_COMPLETE).await;
        let function = output(&outputs_of(&described), case, FUNCTION_NAME_OUTPUT);

        stable_verdicts(&router, case, "as created", &function, row.invocations, false).await;

        // The same template with the vector listing APPENDED to the same inline policy.
        let widened = template(&stack, true);
        let (status, answered) =
            cloudformation(&router, "UpdateStack", &stack_members(&stack, &widened)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        settled_on(&router, case, &stack, UPDATE_COMPLETE).await;

        stable_verdicts(
            &router,
            case,
            "after the update",
            &function,
            row.invocations,
            true,
        )
        .await;
    }
}
