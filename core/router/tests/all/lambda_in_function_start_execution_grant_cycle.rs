// SPDX-License-Identifier: CC0-1.0

//! WHEN the documents that decide a running function's own `states:StartExecution` are read:
//! at the instant of each invocation, over one role whose inline document is rewritten
//! between otherwise identical calls.
//!
//! RULE: an AWS SDK call a running function makes is made as a session of that function's
//! execution role, and the serving service decides it against the identity-based documents in
//! force on that role AT THAT INSTANT — so a document written or replaced between two
//! otherwise identical invocations of the same, unchanged function decides the NEXT one and
//! nothing is carried over from the call before it: a document that allows the action on the
//! addressed resource serves the call and its effect is in the serving service afterwards, a
//! replacement that allows the action nowhere refuses it, one that allows the action on
//! ANOTHER resource refuses it, one that allows and also explicitly denies the action refuses
//! it, and a document put back after any of those serves the call again. A refused call is
//! `AccessDeniedException` (HTTP 400, the status this endpoint answers every refusal of a
//! caller under) naming the assumed-role session, the action and the ARN the request
//! addressed, and it leaves the serving service exactly as it was — no execution exists for
//! it. The invocation itself still answers HTTP 200 with no `X-Amz-Function-Error`, because
//! the handler RAN and caught the refusal.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), so the identity a handler's own call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with no identity-based
//! allow a request falls to IAM's default deny and an explicit deny wins over any allow (IAM
//! User Guide, *Policy evaluation logic*). Starting an execution is `states:StartExecution`
//! on the state machine the request names, `StartExecution` answers `executionArn` and
//! `startDate` (`smithy_query(query="operation",operation="StartExecution")`), and
//! `ListExecutions` reports the executions a state machine holds — so what the service holds
//! after a decision is readable through its own API. `iam:PutRolePolicy` is how an inline
//! identity-based document is written and replaced.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document
//! this suite rewrites between calls, CloudFormation deploys the starter function, STS mints
//! the session its calls are signed with, Lambda runs the handler and supplies it the SDK and
//! the environment it reads the state machine's ARN out of, and Step Functions takes the
//! decision — so the suite lives here, where a crate-local one could only see one side of it.
//! What the neighbours already pin and this one does not repeat:
//! `lambda_in_function_step_functions_authorization` drives the decision over SEVERAL
//! operations of this service called by functions whose roles' documents DIFFER and are in
//! place before anything runs, `events_lambda_sfn_in_function_chain` pins the granted and
//! ungranted start of one pair, and `lambda_in_function_sqs_send_follows_the_live_policies`
//! drives the instant-of-the-call rule against Amazon SQS. What only this suite reaches is ONE
//! role's document being replaced, UNDER ONE ALREADY-RUNNING FUNCTION, through a grant /
//! revoke / restore series against Step Functions, with the state machine's own execution
//! LIST read after every leg — so a refusal that still started an execution, a streak of
//! refusals that quietly started one, a restore that re-answers the execution a previous leg
//! started instead of a new one, and a document stored but never consulted all fail here.
//!
//! The rows differ along what the rule generalises over: WHICH document is in force (an allow
//! of the action on the addressed machine, a replacement that allows only logging, an allow of
//! the action on another machine of the account, an allow of the action beside an explicit deny
//! of it, an allow of every action of the service on every resource), in which DIRECTION the
//! change goes (a grant taken away, put back, narrowed, widened) and how MANY identical calls
//! in a row one document decides — the revoked leg is asked three times running, because a
//! streak is where a decision cached from the call before it would show.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the state machines and the
/// function belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and the session's own ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The state machine the handler addresses on every invocation.
const MACHINE: &str = "xsvc-sfn-grant-cycle-machine";

/// Another state machine of this account, which one row's document names as its resource
/// instead of the addressed one. It is created too, so that row's refusal cannot be about a
/// machine that is not there — and its own execution list is read after every leg, so a grant
/// scoped to it cannot be satisfied by starting something somewhere else.
const OTHER_MACHINE: &str = "xsvc-sfn-grant-cycle-elsewhere";

/// The role both state machines are created with, trusted by this service's own principal.
const MACHINE_ROLE: &str = "xsvc-sfn-grant-cycle-machine-role";

/// The one execution role the function runs as, the inline document whose CONTENTS every row
/// rewrites, and the inline document granting the logs a function writes — which stays in
/// force throughout, so replacing the first one takes nothing else with it.
const ROLE: &str = "xsvc-sfn-grant-cycle-role";
const START_POLICY: &str = "start-the-execution";
const LOGS_POLICY: &str = "write-its-logs";

/// The one function whose handler makes the call, and the stack that deploys it.
const FUNCTION: &str = "xsvc-sfn-grant-cycle-starter";
const STACK: &str = "xsvc-sfn-grant-cycle-stack";

/// The environment variable the handler reads the addressed state machine's ARN out of.
const MACHINE_VARIABLE: &str = "STATE_MACHINE_ARN";

/// The service principal a state machine's role has to trust, and the one a Lambda execution
/// role has to trust.
const SFN_PRINCIPAL: &str = "states.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The IAM action the handler's call performs, which is what a statement grants or denies and
/// what a refusal names.
const START_ACTION: &str = "states:StartExecution";

/// Every action of this service, as the wildcard form a broad grant is written with.
const EVERY_START_ACTION: &str = "states:*";

/// The code and status Step Functions refuses a call the documents in force do not permit
/// with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The reason an IMPLICIT deny states: the principal holds no document allowing the action.
const NO_IDENTITY_GRANT: &str =
    "because no identity-based policy allows the states:StartExecution action";

/// The reason an EXPLICIT deny in an identity-based document states.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The `StackStatus` the function's stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The statuses an execution of these machines reports: the one it sits in while it is still
/// running, and the terminal one an execution of a single `Pass` state reaches
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";
const SUCCEEDED: &str = "SUCCEEDED";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 900;

/// How often to look while waiting for work the clock owes — the function reaching `Active`,
/// an execution leaving [`RUNNING`] — and how many looks to take. The spans are those
/// services' own, so no fixed sleep names the instant one ends, and the deadline is generous
/// enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How many looks a reading that expects to find NOTHING takes before it reports: more than
/// one, so an execution started a moment after the answer is caught rather than raced past,
/// and far fewer than [`LOOKS`], because what these looks confirm is that nothing more is
/// owed.
const CONFIRMING_LOOKS: usize = 3;

/// The definition both state machines carry: one `Pass` state that ends the execution, so a
/// started execution settles on its own and what a row reads about it is the start's own doing.
fn definition() -> Value {
    json!({"StartAt": "Pass", "States": {"Pass": {"Type": "Pass", "End": true}}})
}

/// The handler every row runs: it imports the SDK the managed runtime supplies, starts one
/// execution of the state machine its ENVIRONMENT names under the execution name the event
/// carries, and reports either the `executionArn` Step Functions answered or the refusal it
/// answered — its code, its sentence and the HTTP status it came back on.
const STARTER_HANDLER: &str = r##"
import json
import os

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("stepfunctions")
    try:
        answered = client.start_execution(
            stateMachineArn=os.environ["STATE_MACHINE_ARN"],
            name=event["name"],
            input=json.dumps(event),
        )
        return {"ok": True, "executionArn": answered["executionArn"]}
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the state machine it addresses is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and not the role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be served.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One IAM call that must be served.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One Step Functions call over its `awsJson1_0` wire, made as the account's own credential —
/// whose calls no row's document decides — answered with its status and its parsed body.
async fn sfn(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("states.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("states"))
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
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

/// One Step Functions call that must be served.
async fn sfn_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = sfn(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got {status} with {answered}"
    );
    answered
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

/// The ARN of the role the function runs as.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The ARN the state machine `name` of this account in this Region carries, which is what a
/// statement names and what a refusal reports.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN the execution `execution` of the state machine `machine` carries
/// (`StartExecution$executionArn`).
fn execution_arn(machine: &str, execution: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:{machine}:{execution}")
}

/// The sentence Step Functions refuses the handler's call with, for the document that withheld
/// the grant. Written out here rather than built from the service's own helper, so the wording
/// has something to be wrong about: the identity is the assumed-role SESSION a function's calls
/// are signed as, named for the function the session was opened for.
fn refusal_message(resource: &str, reason: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{FUNCTION} is not authorized to \
         perform: {START_ACTION} on resource: {resource} {reason}"
    )
}

/// A trust policy with one statement allowing `principal` to assume the role.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline document granting the logs a function writes, which stays in force throughout so
/// that replacing the start grant takes nothing else with it.
fn logs_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
}

/// WHICH document one row puts in force on the role, under the one inline policy name every
/// row rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Allow the start on the state machine the handler addresses.
    OnTheMachine,
    /// Allow only the logging a function does — the replacement that takes the start grant
    /// away without touching anything else the role can do.
    LogsOnly,
    /// Allow the start, but on ANOTHER state machine of this account.
    OnAnotherMachine,
    /// Allow the start on the addressed machine and explicitly DENY it in the same document.
    AllowedAndDenied,
    /// Allow every action of this service on every resource.
    EveryAction,
}

impl Grant {
    /// The document this grant is, for the addressed machine's ARN.
    fn document(self, addressed: &str, elsewhere: &str) -> Value {
        match self {
            Self::OnTheMachine => json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": START_ACTION,
                    "Resource": addressed,
                }],
            }),
            Self::LogsOnly => logs_document(),
            Self::OnAnotherMachine => json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": START_ACTION,
                    "Resource": elsewhere,
                }],
            }),
            Self::AllowedAndDenied => json!({
                "Version": "2012-10-17",
                "Statement": [
                    {"Effect": "Allow", "Action": START_ACTION, "Resource": addressed},
                    {"Effect": "Deny", "Action": START_ACTION, "Resource": addressed},
                ],
            }),
            Self::EveryAction => json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": EVERY_START_ACTION,
                    "Resource": "*",
                }],
            }),
        }
    }
}

/// One row: the document it puts in force before the invocation, and what the invocation then
/// answers. `reason` is the sentence a refusal states, and is `None` exactly when the start is
/// served.
struct Row {
    case: &'static str,
    grant: Grant,
    allowed: bool,
    reason: Option<&'static str>,
}

/// The rows, taken IN ORDER against the one function, the one role and the one state machine:
/// each rewrites the role's inline document and asks the SAME invocation again, so what decides
/// it is what is in force at that instant and nothing the row before it was answered.
const ROWS: &[Row] = &[
    Row {
        case: "the positive control: the grant in force starts the execution",
        grant: Grant::OnTheMachine,
        allowed: true,
        reason: None,
    },
    Row {
        case: "the grant replaced with a logs-only document refuses the next start",
        grant: Grant::LogsOnly,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
    Row {
        case: "the second identical start under the same replacement is refused too",
        grant: Grant::LogsOnly,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
    Row {
        case: "the third identical start under the same replacement is refused too",
        grant: Grant::LogsOnly,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
    Row {
        case: "the grant put back starts a further, distinct execution",
        grant: Grant::OnTheMachine,
        allowed: true,
        reason: None,
    },
    Row {
        case: "the grant narrowed to another state machine refuses the start",
        grant: Grant::OnAnotherMachine,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
    Row {
        case: "the grant beside an explicit deny of the same action refuses the start",
        grant: Grant::AllowedAndDenied,
        allowed: false,
        reason: Some(EXPLICIT_DENY),
    },
    Row {
        case: "a grant of every action of the service on every resource serves the start",
        grant: Grant::EveryAction,
        allowed: true,
        reason: None,
    },
    Row {
        case: "the logs-only document put back one last time refuses the start again",
        grant: Grant::LogsOnly,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
];

/// Create the role both state machines are created with, trusted by this service's principal.
async fn given_machine_role(router: &MultiServiceRouter) -> String {
    let created = iam(
        router,
        "setup",
        "CreateRole",
        &[
            ("RoleName", MACHINE_ROLE),
            ("AssumeRolePolicyDocument", &trust_policy(SFN_PRINCIPAL)),
        ],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({MACHINE_ROLE:?}) must answer an ARN"))
}

/// Create the state machine `name` and answer the ARN Step Functions minted for it, asserting
/// it is the ARN every statement and every refusal below names.
async fn given_machine(router: &MultiServiceRouter, name: &str, role: &str) -> String {
    let created = sfn_ok(
        router,
        "setup",
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition().to_string(),
            "roleArn": role,
            "type": "STANDARD",
        }),
    )
    .await;
    let arn = created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateStateMachine({name:?}) must answer an ARN"))
        .to_string();
    assert_eq!(
        arn,
        machine_arn(name),
        "setup: the ARN a statement and a refusal name is the one the service minted, got {arn}"
    );
    arn
}

/// Create the execution role where IAM holds it, carrying the log grant alone: the start grant
/// is the first row's own doing.
async fn given_function_role(router: &MultiServiceRouter) {
    iam(
        router,
        "setup",
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy(LAMBDA_PRINCIPAL)),
        ],
    )
    .await;
    iam(
        router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", LOGS_POLICY),
            ("PolicyDocument", &logs_document().to_string()),
        ],
    )
    .await;
}

/// Deploy the one function the rows invoke, carrying the addressed machine's ARN in its
/// environment. The inline source is archived into the deployment package by CloudFormation,
/// which is how a function whose handler is written here comes to exist at all — with no layer
/// of its own, so the SDK it imports is the one the managed runtime supplies.
async fn given_function(router: &MultiServiceRouter, addressed: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Starter": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn(),
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {MACHINE_VARIABLE: addressed}},
                    "Code": {"ZipFile": STARTER_HANDLER},
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "setup",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion is owed from the instant the owning service reports its resource
    // ready, which no fixed sleep can name: this looks until it reports it, on the shared wait
    // whose deadline is generous enough that a loaded host does not decide the outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let events = cloudformation(
            router,
            "setup",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
}

/// Wait until Lambda reports the function `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// Put one row's document in force under the one inline policy name every row rewrites, and
/// answer what IAM reports it holds afterwards — so a row whose document never landed fails
/// before its invocation is judged.
async fn apply_grant(
    router: &MultiServiceRouter,
    case: &str,
    grant: Grant,
    addressed: &str,
    elsewhere: &str,
) -> Value {
    let wanted = grant.document(addressed, elsewhere);
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", START_POLICY),
            ("PolicyDocument", &wanted.to_string()),
        ],
    )
    .await;
    let read = iam(
        router,
        case,
        "GetRolePolicy",
        &[("RoleName", ROLE), ("PolicyName", START_POLICY)],
    )
    .await;
    // "The policy document returned in this response is URL-encoded compliant with RFC 3986"
    // (`GetRolePolicy`), so the comparison with the document that was put reads it through the
    // decode a caller performs.
    let decoded = ::smithy_server_runtime::percent_decode(
        &opt_element(&read, "PolicyDocument").unwrap_or_default(),
    );
    serde_json::from_str(&decoded).unwrap_or_else(|_| {
        panic!("case {case:?}: the stored document must be JSON, got {decoded:?}")
    })
}

/// Invoke the function with `event` and answer the document the handler returned, asserting on
/// the way that the call was served and that the run reported NO function error — which is what
/// a runtime that could not import the SDK reports instead of a result.
async fn invoked(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let function_error = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        function_error, None,
        "case {case:?}: a handler that catches the refusal runs to completion, got \
         {function_error:?} with {payload}"
    );
    payload
}

/// The execution ARNs `machine_arn` holds right now, as `ListExecutions` reports them.
async fn listed(router: &MultiServiceRouter, case: &str, machine: &str) -> BTreeSet<String> {
    let answered = sfn_ok(
        router,
        case,
        "ListExecutions",
        &json!({"stateMachineArn": machine}),
    )
    .await;
    answered["executions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an executions list, got {answered}"))
        .iter()
        .map(|item| {
            item["executionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every listed execution names its ARN, got {item}")
                })
                .to_string()
        })
        .collect()
}

/// The execution ARNs `machine` holds, looked at again until they are `wanted` or the
/// confirming looks run out — so a start that landed a moment after the invocation answered is
/// caught rather than raced past, and a leg expecting nothing new still looks more than once.
async fn settled_listing(
    router: &MultiServiceRouter,
    case: &str,
    machine: &str,
    wanted: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut held = listed(router, case, machine).await;
    for look in 0..LOOKS {
        if held == *wanted && look + 1 >= CONFIRMING_LOOKS {
            break;
        }
        tokio::time::sleep(LOOK).await;
        held = listed(router, case, machine).await;
    }
    held
}

/// What `DescribeExecution` reports for `execution` once it has left [`RUNNING`].
async fn settled(router: &MultiServiceRouter, case: &str, execution: &str) -> Value {
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = sfn_ok(
            router,
            case,
            "DescribeExecution",
            &json!({"executionArn": execution}),
        )
        .await;
        if reported["status"].as_str() != Some(RUNNING) {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {reported}");
}

/// Every row: the same invocation of the same function is decided by the document in force on
/// its execution role at the instant it is made, and what Step Functions holds afterwards
/// follows that decision.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_function_start_execution_follows_the_document_in_force_at_each_call() {
    let router = build_router();
    let machine_role = given_machine_role(&router).await;
    let addressed = given_machine(&router, MACHINE, &machine_role).await;
    let elsewhere = given_machine(&router, OTHER_MACHINE, &machine_role).await;
    given_function_role(&router).await;
    given_function(&router, &addressed).await;
    wait_until_active(&router).await;

    // Neither machine holds an execution to begin with, so every one read below was started by
    // the leg that read it.
    for machine in [&addressed, &elsewhere] {
        let held = listed(&router, "setup", machine).await;
        assert!(
            held.is_empty(),
            "setup: a state machine nobody started an execution of holds none, got {held:?}"
        );
    }

    // Every execution the series started so far, in the order it started them: what
    // `ListExecutions` must report after each leg, and what a restored grant must add to rather
    // than re-answer.
    let mut started: Vec<String> = Vec::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stored = apply_grant(&router, case, row.grant, &addressed, &elsewhere).await;
        assert_eq!(
            stored,
            row.grant.document(&addressed, &elsewhere),
            "case {case:?}: expected IAM to hold the document this leg put, got {stored}"
        );

        let name = format!("grant-cycle-{index}");
        let event = json!({"name": name, "leg": index});
        let payload = invoked(&router, case, &event).await;

        match row.reason {
            None => {
                assert!(
                    row.allowed,
                    "case {case:?}: a row that states no refusal reason is a served one"
                );
                let expected = execution_arn(MACHINE, &name);
                assert_eq!(
                    payload,
                    json!({"ok": true, "executionArn": expected}),
                    "case {case:?}: expected the handler to report the started execution \
                     {expected:?}, got {payload}"
                );
                assert!(
                    !started.contains(&expected),
                    "case {case:?}: a served start is a NEW execution, but {expected:?} is one \
                     an earlier leg already started ({started:?})"
                );

                // The execution Step Functions answered is one it really holds: it settles on
                // its own, carries the input the handler sent, and belongs to the addressed
                // machine.
                let described = settled(&router, case, &expected).await;
                assert_eq!(
                    described["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: an execution of a single Pass state succeeds, got {described}"
                );
                assert_eq!(
                    described["stateMachineArn"].as_str(),
                    Some(addressed.as_str()),
                    "case {case:?}: the execution belongs to the addressed machine, got \
                     {described}"
                );
                assert_eq!(
                    described["name"].as_str(),
                    Some(name.as_str()),
                    "case {case:?}: the execution carries the name the handler asked for, got \
                     {described}"
                );
                let carried: Value = serde_json::from_str(
                    described["input"].as_str().unwrap_or_else(|| {
                        panic!("case {case:?}: the execution carries its input, got {described}")
                    }),
                )
                .unwrap_or_else(|why| {
                    panic!("case {case:?}: the input is a JSON document, got {described} ({why})")
                });
                assert_eq!(
                    carried, event,
                    "case {case:?}: expected the execution to carry {event}, got {carried}"
                );

                started.push(expected);
            }
            Some(reason) => {
                assert!(
                    !row.allowed,
                    "case {case:?}: a row that states a refusal reason is a refused one"
                );
                assert_eq!(
                    payload,
                    json!({
                        "ok": false,
                        "code": ACCESS_DENIED,
                        "message": refusal_message(&addressed, reason),
                        "status": ACCESS_DENIED_STATUS,
                    }),
                    "case {case:?}: expected the handler to report the service's own refusal, \
                     got {payload}"
                );
            }
        }

        // And what STEP FUNCTIONS holds: exactly the executions the served legs started, on the
        // addressed machine, and none at all on the machine one row's document named instead.
        let wanted: BTreeSet<String> = started.iter().cloned().collect();
        let held = settled_listing(&router, case, &addressed, &wanted).await;
        assert_eq!(
            held, wanted,
            "case {case:?}: expected the addressed machine to hold exactly {wanted:?}, got \
             {held:?}"
        );
        let nowhere = settled_listing(&router, case, &elsewhere, &BTreeSet::new()).await;
        assert!(
            nowhere.is_empty(),
            "case {case:?}: no leg addresses the other machine, so it holds no execution, got \
             {nowhere:?}"
        );
    }

    // The series as a whole: every served leg started one further execution, and the restored
    // grants answered new ARNs rather than the control's.
    let served = ROWS.iter().filter(|row| row.allowed).count();
    assert_eq!(
        started.len(),
        served,
        "the series must hold one execution per served leg, got {started:?}"
    );
    let distinct: BTreeSet<&String> = started.iter().collect();
    assert_eq!(
        distinct.len(),
        started.len(),
        "every served leg starts a DISTINCT execution, got {started:?}"
    );
}
