// SPDX-License-Identifier: CC0-1.0

//! What a Step Functions `Task` state whose `Resource` is a Lambda function's own ARN does
//! as the documents in force on the state machine's EXECUTION ROLE are granted, revoked and
//! granted again, driven across the four services the question spans — Step Functions
//! interprets the definition and makes the invocation, IAM holds the documents the
//! invocation is judged against, Lambda runs the handler, and Amazon S3 takes the write the
//! handler makes — mounted on ONE engine, as `core/router/src/lib.rs` assembles them.
//!
//! RULE: Step Functions makes a `Task` state's integration call acting as the state
//! machine's `roleArn` and decides it against the documents in force on that role at the
//! moment of the call, so every execution a client starts settles either way: a call those
//! documents allow is really made, the task ends on what the call answered and the execution
//! reaches `SUCCEEDED`, while one they withhold is NOT made — the task fails under the
//! invoked service's own access-denied class, naming the session the call was attempted as,
//! the action and the resource, and the execution reaches `FAILED` carrying that error and
//! cause. Because the decision is taken per call rather than once per execution or once per
//! state machine, the same definition under the same role settles differently as the
//! documents change, and nothing the refused call would have done happens — the handler's
//! own side effect in another service is absent exactly for the legs that were refused.
//!
//! Only this target can hold that: the task's refusal is Step Functions' to report, the
//! documents are IAM's to hold, the run is Lambda's, and the effect of a run that did happen
//! is read back out of Amazon S3's own store, so a crate-local suite sees at most one side of
//! it. `all/sfn_task_target_authorization.rs` asks which RESOURCE an access request is over
//! and `all/sfn_execution_role_documents_in_force.rs` which DOCUMENTS are in force; what is
//! under test here is that each decision terminates its execution and that a refused one
//! leaves no trace of the work.
//!
//! The rows differ along what the rule generalises over: what the documents in force say
//! about the call (a grant on the function's own ARN, no document at all once the only one is
//! deleted, the identical grant put back, a document speaking only of another action, a grant
//! of the same action over another function, the grant beside an explicit `Deny` of it, and a
//! grant whose `Resource` is the wildcard) — and so whether the call is made at all. They run
//! in order against ONE role and ONE state machine, which is what makes the grant/revoke/
//! regrant series a series rather than three independent setups.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};
use crate::sfn_task_target_authorization::session_principal;

/// The Region every request here is signed for, and so the Region the bucket, the function,
/// the role and the state machine belong to.
const REGION: &str = "us-east-1";

/// The account this emulator answers for, which is the account the ARN a grant names belongs
/// to.
const ACCOUNT: &str = "000000000000";

/// The bucket the handler writes its marker into.
const BUCKET: &str = "sfn-grant-cycle-markers";

/// The prefix the handler writes its marker under, and the extension it gives it.
const MARKER_PREFIX: &str = "triggered/";
const MARKER_SUFFIX: &str = ".txt";

/// The function the `Task` state's `Resource` names, the stack it is declared in, and the
/// `StackStatus` that stack must reach for it to exist.
const FUNCTION: &str = "sfn-grant-cycle-marker-writer";
const FUNCTION_STACK: &str = "sfn-grant-cycle-handler-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The state machine every leg runs, and the roles the two sides of the work are done as:
/// the function runs as its own execution role, the execution's tasks as the state machine's.
const MACHINE: &str = "sfn-grant-cycle-task";
const FUNCTION_ROLE: &str = "SfnGrantCycleHandlerRole";
const MACHINE_ROLE: &str = "SfnGrantCycleMachineRole";

/// The name of the ONE inline policy the machine role carries, which is what each leg puts
/// and what the revoking leg deletes.
const MACHINE_POLICY: &str = "invoke-the-task-function";

/// The service principals the two roles are trusted by, which is what lets each service act
/// as its own.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The IAM action an invocation is subject to: the permission Lambda PUBLISHES for `Invoke`
/// ("requires permission for the `lambda:InvokeFunction` action"), which is not the
/// operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action that has nothing to do with an invocation, which is what one row's document
/// speaks about instead.
const UNRELATED_ACTION: &str = "logs:PutLogEvents";

/// The error a `Task` state reports for a call the documents withheld: Lambda's base SDK
/// exception, which is what a refusal its `Invoke` models no error of its own arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The two terminal statuses of this series and the non-terminal one an execution sits in
/// while its work runs (`smithy_query(query="shape",shape="ExecutionStatus")`:
/// `RUNNING`, `SUCCEEDED`, `FAILED`, `TIMED_OUT`, `ABORTED`, `PENDING_REDRIVE`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The history events one attempt of a `Task` state whose `Resource` is a function's ARN
/// writes, and the ones the execution itself ends with
/// (`smithy_query(query="shape",shape="HistoryEventType")`).
const LAMBDA_SCHEDULED: &str = "LambdaFunctionScheduled";
const LAMBDA_SUCCEEDED: &str = "LambdaFunctionSucceeded";
const LAMBDA_FAILED: &str = "LambdaFunctionFailed";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";
const EXECUTION_FAILED: &str = "ExecutionFailed";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run of
/// this handler needs on any host, because what is under test is whether the call happened at
/// all and not how fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// [`FUNCTION_ACTIVE`], an execution leaving [`RUNNING`] — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler the `Task` state invokes: it builds a client with the SDK the runtime supplies
/// and writes ONE marker object named after the token the execution carries, then reports
/// what it wrote. The marker is the handler's own side effect, so an execution whose call was
/// never made leaves none.
const MARKER_HANDLER: &str = r#"
import boto3


def handler(event, context):
    bucket = event["bucket"]
    token = event["token"]
    key = "triggered/" + token + ".txt"
    objects = boto3.client("s3")
    objects.put_object(Bucket=bucket, Key=key, Body=token.encode("utf-8"))
    return {"token": token, "key": key}
"#;

/// What the documents in force on the machine role say about the invocation when a leg's
/// execution is started.
#[derive(Debug, Clone, Copy)]
enum Document {
    /// One inline policy granting the invocation on the function's own ARN.
    InvokeThisFunction,
    /// No inline policy at all: the position the role is left in once the only document it
    /// carried is deleted, which is the implicit deny every identity starts from.
    Nothing,
    /// One inline policy granting the invocation over every resource.
    InvokeEveryFunction,
    /// One granting the invocation over ANOTHER function's ARN, which says nothing about this
    /// one.
    InvokeAnotherFunction,
    /// One speaking only of an action that is not the invocation.
    AnotherAction,
    /// The grant on the function's own ARN beside an explicit `Deny` of the same action.
    InvokeDeniedExplicitly,
}

impl Document {
    /// The statements the inline policy carries, or `None` for the leg that carries no
    /// document at all.
    fn statements(self, function_arn: &str, other_function_arn: &str) -> Option<Value> {
        match self {
            Self::Nothing => None,
            Self::InvokeThisFunction => Some(json!([allow(INVOKE_ACTION, function_arn)])),
            Self::InvokeEveryFunction => Some(json!([allow(INVOKE_ACTION, "*")])),
            Self::InvokeAnotherFunction => Some(json!([allow(INVOKE_ACTION, other_function_arn)])),
            Self::AnotherAction => Some(json!([allow(UNRELATED_ACTION, "*")])),
            Self::InvokeDeniedExplicitly => Some(json!([
                allow(INVOKE_ACTION, function_arn),
                { "Effect": "Deny", "Action": INVOKE_ACTION, "Resource": function_arn },
            ])),
        }
    }
}

/// What a leg's execution must come to.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The documents allow the call, so it is made: the task ends on the handler's own result,
    /// the execution reaches [`SUCCEEDED`] and the marker the handler wrote is in the bucket.
    Invoked,
    /// The documents withhold it, so nothing runs: the task fails under [`REFUSED_ERROR`], the
    /// execution reaches [`FAILED`] carrying the refusal, and no marker exists.
    Refused(Withheld),
}

/// WHY the documents withhold the call, which is the clause IAM's own sentence ends with:
/// naming the document that withheld it is how an operator finds the policy to change.
#[derive(Debug, Clone, Copy)]
enum Withheld {
    /// No statement the account holds allows the call, which is the implicit deny every
    /// identity starts from.
    NoGrant,
    /// An explicit `Deny` in a document the account holds refuses it whatever a grant says.
    ExplicitDeny,
}

impl Withheld {
    /// The clause the refusal's sentence ends with.
    fn clause(self) -> String {
        match self {
            Self::NoGrant => {
                format!("because no identity-based policy allows the {INVOKE_ACTION} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// One leg of the series.
struct Leg {
    case: &'static str,
    /// The token the leg's execution carries, which names the object the handler writes for
    /// it: one per leg, so a marker's absence is this leg's own evidence rather than a
    /// previous leg's.
    token: &'static str,
    document: Document,
    expected: Expected,
}

/// The series, in order: the positive control, the identical grant revoked, the identical
/// grant restored, and then the other ways a document can say nothing that covers the call.
fn legs() -> Vec<Leg> {
    vec![
        Leg {
            case: "the grant on the function's own ARN",
            token: "granted-first",
            document: Document::InvokeThisFunction,
            expected: Expected::Invoked,
        },
        Leg {
            case: "the identical grant revoked, leaving the role carrying no document",
            token: "revoked",
            document: Document::Nothing,
            expected: Expected::Refused(Withheld::NoGrant),
        },
        Leg {
            case: "the identical grant restored",
            token: "granted-again",
            document: Document::InvokeThisFunction,
            expected: Expected::Invoked,
        },
        Leg {
            case: "a document speaking only of another action",
            token: "another-action",
            document: Document::AnotherAction,
            expected: Expected::Refused(Withheld::NoGrant),
        },
        Leg {
            case: "a grant of the same action over another function",
            token: "another-function",
            document: Document::InvokeAnotherFunction,
            expected: Expected::Refused(Withheld::NoGrant),
        },
        Leg {
            case: "the grant beside an explicit Deny of the same action",
            token: "explicitly-denied",
            document: Document::InvokeDeniedExplicitly,
            expected: Expected::Refused(Withheld::ExplicitDeny),
        },
        Leg {
            case: "a grant whose Resource is the wildcard",
            token: "granted-by-wildcard",
            document: Document::InvokeEveryFunction,
            expected: Expected::Invoked,
        },
    ]
}

/// A statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The emulator as it is ASSEMBLED, on one engine: the invocation a task makes and the write
/// the handler makes are dispatched back into this same router, so the function Lambda runs
/// and the bucket Amazon S3 holds are the ones this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One response of a REST surface: its status and its body's own bytes.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    /// The body as text, for a refusal or a JSON document.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as the JSON document a `restJson1` answer carries.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/octet-stream")
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
    }
}

/// One Lambda `restJson1` request.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
    }
}

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("states.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
        .header("authorization", authorization("states"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM, requiring a 200, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {text}"
    );
    text
}

/// One awsQuery call to CloudFormation, answered with its status and its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// A role `service` may assume, by its own ARN. Its PERMISSIONS are provisioned separately,
/// because what this series changes between legs is exactly that.
async fn bare_role(router: &MultiServiceRouter, case: &str, name: &str, service: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"))
}

/// Put `statements` on `role` as its one inline policy called `policy`.
async fn put_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    policy: &str,
    statements: &Value,
) {
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", policy),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// Create the bucket the handler writes its markers into.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let answer = s3(router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: CreateBucket({BUCKET:?}) must answer 200, got {} with {}",
        answer.status,
        answer.text()
    );
}

/// Deploy the function the `Task` state invokes, wait until Lambda reports it
/// [`FUNCTION_ACTIVE`], and answer its ARN.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — how a function whose handler is written out in a suite comes to exist
/// at all.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "MarkerWriter": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": MARKER_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with \
         {answered}"
    );
    let (reported, body) = wait_for_stack_status(router, FUNCTION_STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {body}"
    );

    let mut arn = String::new();
    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let answer = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        )
        .await;
        configuration = answer.json();
        if let Some(reported) = configuration["FunctionArn"].as_str() {
            arn = reported.to_string();
        }
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            assert!(
                !arn.is_empty(),
                "case {case:?}: the function must report its ARN, got {configuration}"
            );
            return arn;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// The definition every leg runs: ONE `Task` state whose `Resource` is the function's own ARN,
/// invoked with the bucket it writes into and the token the execution carries.
fn definition(function_arn: &str) -> String {
    json!({
        "StartAt": "WriteMarker",
        "States": {
            "WriteMarker": {
                "Type": "Task",
                "Resource": function_arn,
                "Parameters": { "bucket": BUCKET, "token.$": "$.token" },
                "End": true,
            },
        },
    })
    .to_string()
}

/// Start one execution of `machine_arn` carrying `token` and report it once it has left
/// [`RUNNING`].
async fn ran(router: &MultiServiceRouter, case: &str, machine_arn: &str, token: &str) -> Value {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": token,
            "input": json!({ "token": token }).to_string(),
        }),
    )
    .await;
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartExecution must answer an ARN, got {started}"))
        .to_string();
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described["status"].as_str() != Some(RUNNING) {
            return described;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {described}");
}

/// The events of an execution's history, in the order it holds them.
async fn history(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn, "maxResults": 200 }),
    )
    .await;
    answered["events"].as_array().cloned().unwrap_or_default()
}

/// The names of `events`, for a message that has to say what the history held.
fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The events of `events` that are of `kind`.
fn of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"].as_str() == Some(kind))
        .collect()
}

/// The marker key the handler writes for `token`, as the handler builds it.
fn marker_key(token: &str) -> String {
    format!("{MARKER_PREFIX}{token}{MARKER_SUFFIX}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_documents_in_force_at_each_call_decide_the_executions_terminal_state() {
    let setup = "the grant/revoke/regrant series over one role and one state machine";
    let router = build_router();

    given_bucket(&router, setup).await;

    // The function runs as a role allowed the write its handler makes; what the state machine
    // runs as carries nothing yet, because each leg below states what it carries.
    let function_role = bare_role(&router, setup, FUNCTION_ROLE, LAMBDA_PRINCIPAL).await;
    put_policy(
        &router,
        setup,
        FUNCTION_ROLE,
        "write-the-marker",
        &json!([
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
        ]),
    )
    .await;
    let machine_role = bare_role(&router, setup, MACHINE_ROLE, STATES_PRINCIPAL).await;

    let function_arn = given_function(&router, setup, &function_role).await;
    // The ARN one leg's grant names INSTEAD: a function of this account and Region that is not
    // the one the `Task` state invokes.
    let other_function_arn =
        format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}-elsewhere");

    let created = sfn(
        &router,
        setup,
        "CreateStateMachine",
        &json!({
            "name": MACHINE,
            "definition": definition(&function_arn),
            "roleArn": machine_role,
            "type": "STANDARD",
        }),
    )
    .await;
    let machine_arn = created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: CreateStateMachine must answer an ARN"))
        .to_string();

    // Whether the role carries its inline policy right now, so the leg that revokes it deletes
    // a document that is really there.
    let mut carried = false;

    for leg in legs() {
        let Leg {
            case,
            token,
            document,
            expected,
        } = leg;

        match document.statements(&function_arn, &other_function_arn) {
            Some(statements) => {
                put_policy(&router, case, MACHINE_ROLE, MACHINE_POLICY, &statements).await;
                carried = true;
            }
            None => {
                assert!(
                    carried,
                    "case {case:?}: this leg revokes the document a previous leg put, so one \
                     must be in force when it runs"
                );
                iam(
                    &router,
                    case,
                    "DeleteRolePolicy",
                    &[("RoleName", MACHINE_ROLE), ("PolicyName", MACHINE_POLICY)],
                )
                .await;
                carried = false;
            }
        }

        let described = ran(&router, case, &machine_arn, token).await;
        let execution_arn = described["executionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an executionArn, got {described}"))
            .to_string();
        let events = history(&router, case, &execution_arn).await;
        let key = marker_key(token);
        let stored = s3(&router, "GET", &format!("/{BUCKET}/{key}")).await;

        // Whatever the documents said, the state handed its work over once: scheduling an
        // attempt is recorded before anything about the call's outcome is known.
        assert_eq!(
            of_kind(&events, LAMBDA_SCHEDULED).len(),
            1,
            "case {case:?}: expected exactly one {LAMBDA_SCHEDULED}, got {:?}",
            kinds(&events)
        );

        match expected {
            Expected::Invoked => {
                assert_eq!(
                    described["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: expected the execution to end {SUCCEEDED}, got {} with {} {}",
                    described["status"],
                    described["error"],
                    described["cause"]
                );
                // The task ended on what the call answered, which is the handler's own report.
                let output: Value =
                    serde_json::from_str(described["output"].as_str().unwrap_or("null"))
                        .unwrap_or(Value::Null);
                assert_eq!(
                    output,
                    json!({ "token": token, "key": key }),
                    "case {case:?}: expected the handler's own answer as the task's result, got \
                     {output}"
                );
                assert_eq!(
                    of_kind(&events, LAMBDA_SUCCEEDED).len(),
                    1,
                    "case {case:?}: expected exactly one {LAMBDA_SUCCEEDED}, got {:?}",
                    kinds(&events)
                );
                assert!(
                    of_kind(&events, LAMBDA_FAILED).is_empty()
                        && of_kind(&events, EXECUTION_FAILED).is_empty(),
                    "case {case:?}: expected no failure in the history, got {:?}",
                    kinds(&events)
                );
                // And the effect of the run, read back out of the object store: a call that
                // answered without the function running fails here.
                assert_eq!(
                    stored.status,
                    200,
                    "case {case:?}: expected GetObject({key:?}) to answer 200, got {} with {}",
                    stored.status,
                    stored.text()
                );
                assert_eq!(
                    stored.body,
                    token.as_bytes(),
                    "case {case:?}: expected the marker to hold the token the handler wrote, \
                     got {:?}",
                    stored.text()
                );
            }
            Expected::Refused(withheld) => {
                assert_eq!(
                    (described["status"].as_str(), described["error"].as_str()),
                    (Some(FAILED), Some(REFUSED_ERROR)),
                    "case {case:?}: expected the execution to end {FAILED} under \
                     {REFUSED_ERROR}, got {described}"
                );
                let cause = described["cause"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: expected a cause, got {described}"))
                    .to_string();
                // The sentence AWS words an authorization failure with: the identity the call
                // was attempted as — a session of the state machine's role, since the task does
                // not act as whoever started the execution — the action the invocation is
                // subject to, the resource it was over, and the clause naming which document
                // withheld it.
                let principal = session_principal(&machine_role, &cause);
                let clause = withheld.clause();
                let sentence = format!(
                    "User: {principal} is not authorized to perform: {INVOKE_ACTION} on \
                     resource: {function_arn} {clause}"
                );
                assert!(
                    cause.contains(&sentence),
                    "case {case:?}: expected the cause to carry {sentence:?}, got {cause:?}"
                );
                // The task failed, the execution failed under it, and neither succeeded.
                let failed = of_kind(&events, LAMBDA_FAILED);
                assert_eq!(
                    failed.len(),
                    1,
                    "case {case:?}: expected exactly one {LAMBDA_FAILED}, got {:?}",
                    kinds(&events)
                );
                assert_eq!(
                    failed[0]["lambdaFunctionFailedEventDetails"]["error"].as_str(),
                    Some(REFUSED_ERROR),
                    "case {case:?}: expected the failed attempt to name {REFUSED_ERROR}, got {}",
                    failed[0]
                );
                assert!(
                    failed[0]["lambdaFunctionFailedEventDetails"]["cause"]
                        .as_str()
                        .is_some_and(|written| written.contains(&sentence)),
                    "case {case:?}: expected the failed attempt to carry the refusal, got {}",
                    failed[0]
                );
                assert_eq!(
                    (
                        of_kind(&events, EXECUTION_FAILED).len(),
                        of_kind(&events, EXECUTION_SUCCEEDED).len(),
                        of_kind(&events, LAMBDA_SUCCEEDED).len(),
                    ),
                    (1, 0, 0),
                    "case {case:?}: expected the execution to end failed and nothing to have \
                     succeeded, got {:?}",
                    kinds(&events)
                );
                // Nothing the call would have done happened: the handler's own marker for this
                // leg is not in the bucket.
                assert_eq!(
                    stored.status,
                    404,
                    "case {case:?}: expected no marker at {key:?}, got {} with {}",
                    stored.status,
                    stored.text()
                );
            }
        }
    }
}
