// SPDX-License-Identifier: CC0-1.0

//! WHAT a Step Functions `Task` state reports when the Lambda function it invoked failed:
//! whose failure it is, and whose text.
//!
//! RULE: a failed invocation is reported to the state that asked for it as the run's OWN
//! failure — the `errorType` of the error document the invocation answered is the error
//! name, and that document is the cause: carried as the invocation ANSWERED it where the
//! `Resource` named the function (the answer is the task's own), and as the document the
//! integration read out of the SDK response where the `Resource` named the optimized
//! `lambda:invoke` integration. An access-denied refusal of the invocation is reported
//! instead wherever no document in force on the execution role ALLOWS the call, which
//! includes a role of this account carrying no identity-based document at all: a grant has
//! to come from a document, so such a role allows nothing. And a
//! redriven attempt resumes the step the last attempt stopped at without entering it
//! again, so the first event it writes for that step names the PRESERVED event that
//! entered it as its `previousEventId`.
//!
//! Three crates have to agree for that to hold — Step Functions decides the call and
//! records the history, IAM holds the documents it is judged against, and Lambda runs the
//! handler and writes the document its failure answers — so the suite lives here rather
//! than in any one of them. The functions are declared as a CloudFormation stack's inline
//! source, which is how a handler written out in a suite comes to be a deployment package
//! at all.
//!
//! The rows differ along what the rule generalises over: what the execution role's
//! documents say about the invocation (a grant on the function's own ARN, a grant of an
//! unrelated action, and no identity-based document at all), the form the `Task` names its
//! work in (the function's own ARN and the optimized integration token), what ended the
//! run (a handler that raised, and the function's own `Timeout`), and the order number the
//! event carries, which the raised message is built from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the function and the
/// state machine belong to.
const REGION: &str = "us-east-1";

/// The runtime both handlers are written for, and the module the archived inline source
/// lands in, which the handler name below resolves against.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The handler whose run RAISES: the class is Python's own `RuntimeError` and the message
/// is built from the event, so the error document is the run's and its input's rather than
/// anything fixed here.
const RAISING_HANDLER: &str = "def handler(event, context):\n    \
     raise RuntimeError('order %s cannot be processed' % event['order'])\n";

/// The handler whose run is ended by the function's own `Timeout`: it sleeps far past any
/// bound a row gives it, so what ends the run is that bound rather than the handler.
const SLEEPING_HANDLER: &str = "import time\n\n\ndef handler(event, context):\n    \
     time.sleep(30)\n    return {'slept': True}\n";

/// The class Python's own `RuntimeError` reports, which is the `errorType` of the document
/// that raise answers and so the name the failed task and the failed execution carry.
const RAISED_CLASS: &str = "RuntimeError";

/// The class Lambda reports for a run its SANDBOX ended at the function's `Timeout`: the
/// platform's own, because no handler raised.
const TIMED_OUT_CLASS: &str = "Sandbox.Timedout";

/// The `Timeout`, in seconds, the sleeping function declares — the bound that ends its
/// runs — and the ample one the raising function declares, so nothing but the raise ends
/// its own.
const SHORT_TIMEOUT_SECS: u64 = 1;
/// See [`SHORT_TIMEOUT_SECS`].
const AMPLE_TIMEOUT_SECS: u64 = 300;

/// The IAM action an invocation is subject to, which is the one Lambda PUBLISHES for
/// `Invoke` rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action that has nothing to do with an invocation, which is what one row's role grants
/// instead.
const UNRELATED_ACTION: &str = "logs:PutLogEvents";

/// The error name a `Task` state reports when the role's documents refuse the call: Lambda's
/// base SDK exception, which is what a refusal `Invoke` models no error of its own arrives
/// under.
const REFUSED_CLASS: &str = "Lambda.AWSLambdaException";

/// The fixed text of the sentence such a refusal carries, and the code the answer it stands
/// in for came back under.
const REFUSAL_SENTENCE: &str = "is not authorized to perform: lambda:InvokeFunction";
/// See [`REFUSAL_SENTENCE`].
const REFUSAL_CODE: &str = "Error Code: AccessDeniedException";

/// The `Resource` of the optimized Lambda integration, which names the service and the
/// action rather than the function; the function is named by a `FunctionName` parameter.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The service principal a state machine's execution role is assumed by, and the one a
/// function's own execution role is assumed by — which `CreateFunction` checks.
const SFN_PRINCIPAL: &str = "states.amazonaws.com";
/// See [`SFN_PRINCIPAL`].
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The `State` a function reports once it can be invoked, and the status a stack reports
/// once every resource it declares is created.
const FUNCTION_ACTIVE: &str = "Active";
/// See [`FUNCTION_ACTIVE`].
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status an execution reports while it is still running, which every wait below polls
/// out of.
const RUNNING: &str = "RUNNING";

/// How long a wait for a documented transition looks before giving up — generously long,
/// because what it waits for is a CONDITION rather than a span — and how long one look
/// sleeps.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The two functions this suite deploys, by the logical id and the name each is declared
/// under.
const RAISER: &str = "raiser";
/// See [`RAISER`].
const SLEEPER: &str = "sleeper";

/// Which form a row's `Task` names its work in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The function's own ARN: the invocation's answer IS the task's result, so a failed
    /// run's document is reported as the invocation answered it.
    FunctionArn,
    /// The optimized integration token: the task's result is the SDK response, whose
    /// `Payload` member the integration read as a document.
    Optimized,
}

impl Named {
    /// The event type the state's own failure is recorded under for this form: the Lambda
    /// family for a `Resource` that named the function, the `Task` family otherwise.
    fn failure_event(self) -> &'static str {
        match self {
            Self::FunctionArn => "LambdaFunctionFailed",
            Self::Optimized => "TaskFailed",
        }
    }

    /// The event type that records the work being HANDED OVER for this form, which is the
    /// first event an attempt of the state writes.
    fn scheduled_event(self) -> &'static str {
        match self {
            Self::FunctionArn => "LambdaFunctionScheduled",
            Self::Optimized => "TaskScheduled",
        }
    }

    /// The detail member [`Self::failure_event`] carries its error and cause under.
    fn failure_detail(self) -> &'static str {
        match self {
            Self::FunctionArn => "lambdaFunctionFailedEventDetails",
            Self::Optimized => "taskFailedEventDetails",
        }
    }

    /// The definition of ONE `Task` state invoking `function_arn` in this form, with the
    /// execution's whole input as the event and no `Retry` and no `Catch` — so the task's
    /// outcome is the execution's.
    fn definition(self, function_arn: &str) -> String {
        let state = match self {
            Self::FunctionArn => json!({
                "Type": "Task",
                "Resource": function_arn,
                "End": true,
            }),
            Self::Optimized => json!({
                "Type": "Task",
                "Resource": OPTIMIZED_RESOURCE,
                "Parameters": { "FunctionName": function_arn, "Payload.$": "$" },
                "End": true,
            }),
        };
        let mut states = serde_json::Map::new();
        states.insert(WORK_STATE.to_string(), state);
        json!({ "StartAt": WORK_STATE, "States": Value::Object(states) }).to_string()
    }
}

/// The one state every definition here declares.
const WORK_STATE: &str = "Invoke";

/// The emulator as it is ASSEMBLED, on one engine: a task's invocation is dispatched back
/// into this same router, so the function it runs is the one this suite deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `restJson1` request to Lambda, as its status and its body's own BYTES — the bytes
/// matter here, because what an invocation answered is the text its runtime wrote.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Vec<u8>) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map(|body| body.to_string().into_bytes()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
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
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One `awsQuery` call to IAM, requiring a 200, answered with its XML body.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)]) -> String {
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

/// One `awsQuery` call to CloudFormation, as its status and its XML body.
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

/// A role `principal` may assume, carrying `statements` as its inline policy where the
/// caller asks for one; answers the ARN IAM's own `CreateRole` reported for it.
///
/// `None` statements leave the role carrying NO identity-based document at all, which is
/// the row whose permissions this account cannot see.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
    statements: Option<Value>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
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
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    if let Some(statements) = statements {
        let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
        iam(
            router,
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "TaskWork"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    }
    arn
}

/// Deploy both handlers as one stack's functions and answer each one's ARN, waiting until
/// the stack reports every resource created and Lambda reports each function invocable.
async fn given_functions(router: &MultiServiceRouter, case: &str, role_arn: &str) -> (String, String) {
    let function = |name: &str, source: &str, timeout: u64| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Role": role_arn,
                "Timeout": timeout,
                "Code": { "ZipFile": source },
            },
        })
    };
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Raiser": function(RAISER, RAISING_HANDLER, AMPLE_TIMEOUT_SECS),
            "Sleeper": function(SLEEPER, SLEEPING_HANDLER, SHORT_TIMEOUT_SECS),
        },
    })
    .to_string();
    let stack = "sfn-lambda-failure-identity";
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {answered}"
    );
    let giving_up_at = Instant::now() + DEADLINE;
    let mut reported = stack_status(router, stack).await;
    while reported.0 != CREATE_COMPLETE && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the functions' stack must reach {CREATE_COMPLETE}, last saw {}",
        reported.1
    );
    (
        active_function(router, case, RAISER).await,
        active_function(router, case, SLEEPER).await,
    )
}

/// The ARN of the function `name`, once Lambda reports it [`FUNCTION_ACTIVE`] — the state
/// an invocation of it is one Lambda admits.
async fn active_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    let giving_up_at = Instant::now() + DEADLINE;
    let mut configuration = Value::Null;
    while Instant::now() < giving_up_at {
        let (_, body) = lambda(router, "GET", &uri, None).await;
        configuration = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
            return configuration
                .get("FunctionArn")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {name} must report its ARN, got {configuration}")
                })
                .to_string();
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: {name} must reach {FUNCTION_ACTIVE}, last saw {configuration}")
}

/// Create `definition` as a state machine running as `role_arn`, start one execution of it
/// with `input`, and report that execution once it has left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &str,
    input: &Value,
    role_arn: &str,
) -> Value {
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({ "name": name, "definition": definition, "roleArn": role_arn }),
    )
    .await;
    let machine = created
        .get("stateMachineArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine answers an ARN, got {created}"))
        .to_string();
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({ "stateMachineArn": machine, "name": "only", "input": input.to_string() }),
    )
    .await;
    let execution = started
        .get("executionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: StartExecution answers an ARN, got {started}"))
        .to_string();
    settled(router, case, &execution).await
}

/// `execution` once it has left [`RUNNING`] — the wait is on that CONDITION, looking again
/// until it holds or [`DEADLINE`] passes.
async fn settled(router: &MultiServiceRouter, case: &str, execution: &str) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut described = Value::Null;
    while Instant::now() < giving_up_at {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some(RUNNING) {
            return described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {described}")
}

/// Every event of `execution`'s history, oldest first.
async fn history(router: &MultiServiceRouter, case: &str, execution: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution }),
    )
    .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected events in {answered}"))
}

/// The detail `member` of the one event of type `kind` the history holds.
fn detail(events: &[Value], kind: &str, member: &str, case: &str) -> Value {
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(kind))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {kind}, got {:?}",
        types_of(events)
    );
    found[0]
        .get(member)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {}", found[0]))
}

/// The type of each event, in the order the history holds them.
fn types_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The `error` and `cause` a failure detail carries, as text.
fn failure(detail: &Value, case: &str) -> (String, String) {
    let error = detail
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error on {detail}"))
        .to_string();
    let cause = detail
        .get("cause")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a cause on {detail}"))
        .to_string();
    (error, cause)
}

/// `text` with every identifier of the form an invocation's own id takes replaced by a
/// fixed marker, so two runs of the same failure compare equal.
///
/// An invocation's id is minted per call — a correlation value — so it is the one part of a
/// run's error document that two runs do not share; everything else about the document
/// (its members, their order, the separators its writer wrote and the text of every value)
/// is the writer's and compares exactly.
fn without_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let glyphs: Vec<char> = text.chars().collect();
    let mut at = 0;
    while at < glyphs.len() {
        if is_id(&glyphs[at..]) {
            out.push_str("<id>");
            at += ID_GLYPHS;
        } else {
            out.push(glyphs[at]);
            at += 1;
        }
    }
    out
}

/// How many characters an invocation id spells: eight, four, four, four then twelve
/// hexadecimal digits, separated by four hyphens.
const ID_GLYPHS: usize = 36;

/// Whether `glyphs` OPENS with an invocation id ([`ID_GLYPHS`]).
fn is_id(glyphs: &[char]) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    if glyphs.len() < ID_GLYPHS {
        return false;
    }
    let mut at = 0;
    for (position, group) in groups.iter().enumerate() {
        if position > 0 {
            if glyphs[at] != '-' {
                return false;
            }
            at += 1;
        }
        if !glyphs[at..at + group]
            .iter()
            .all(|glyph| glyph.is_ascii_hexdigit())
        {
            return false;
        }
        at += group;
    }
    true
}

/// What the invocation of `function` with `event` ANSWERS, as the text its body carried:
/// the authority on the error document a failed run produces, read from the service that
/// wrote it rather than written out here.
async fn answered_document(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> String {
    let (status, body) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(event),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Invoke must answer 200 whatever the function did, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    String::from_utf8_lossy(&body).to_string()
}

/// What the documents in force on a row's role say about the invocation, and so what the
/// task closes with.
struct RoleRow {
    case: &'static str,
    /// The statements the role's inline policy carries, or `None` for a role carrying no
    /// identity-based document at all.
    statements: Option<Value>,
    /// Whether the call is made: the run's own failure where it is, Lambda's access-denied
    /// refusal where it is not.
    made: bool,
}

/// The three readings this suite makes, over ONE emulator and ONE pair of deployed
/// functions: the setup is the same for all of them, and running them in one test keeps a
/// suite that really runs handlers to one emulator rather than three.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_lambda_tasks_failure_identity_is_the_runs_own() {
    let router = build_router();
    let function_role = given_role(
        &router,
        "setup",
        "FailureIdentityFunctionRole",
        LAMBDA_PRINCIPAL,
        Some(json!([{ "Effect": "Allow", "Action": UNRELATED_ACTION, "Resource": "*" }])),
    )
    .await;
    let (raiser, sleeper) = given_functions(&router, "setup", &function_role).await;
    let granting = given_role(
        &router,
        "setup",
        "FailureIdentityTaskRole",
        SFN_PRINCIPAL,
        Some(json!([{ "Effect": "Allow", "Action": INVOKE_ACTION, "Resource": "*" }])),
    )
    .await;

    a_task_is_made_only_where_a_document_in_force_allows_it(&router, &raiser).await;
    a_failed_invocations_cause_is_the_document_the_run_answered(
        &router, &raiser, &sleeper, &granting,
    )
    .await;
    a_redriven_attempt_chains_its_first_event_to_the_preserved_entry(&router, &raiser, &granting)
        .await;
}

/// A task's call is made only where a document in force on its role ALLOWS it: a role
/// whose documents say nothing about the invocation, and a role of this account carrying no
/// identity-based document at all, both refuse it, and only a grant lets the call be made
/// and the task close on the run's own failure.
async fn a_task_is_made_only_where_a_document_in_force_allows_it(
    router: &MultiServiceRouter,
    raiser: &str,
) {
    let rows = vec![
        RoleRow {
            case: "a role granted the invocation on the function's own ARN",
            statements: Some(json!([{
                "Effect": "Allow",
                "Action": INVOKE_ACTION,
                "Resource": raiser.to_string(),
            }])),
            made: true,
        },
        RoleRow {
            case: "a role whose document grants an unrelated action",
            statements: Some(
                json!([{ "Effect": "Allow", "Action": UNRELATED_ACTION, "Resource": "*" }]),
            ),
            made: false,
        },
        RoleRow {
            case: "a role the account holds that carries no identity-based document at all",
            statements: None,
            made: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let RoleRow {
            case,
            statements,
            made,
        } = row;
        let role = given_role(
            router,
            case,
            &format!("FailureIdentityRole{index}"),
            SFN_PRINCIPAL,
            statements,
        )
        .await;
        let described = ran(
            router,
            case,
            &format!("failure-identity-role-{index}"),
            &Named::Optimized.definition(raiser),
            &json!({ "order": format!("o-{index}") }),
            &role,
        )
        .await;
        let expected = if made { RAISED_CLASS } else { REFUSED_CLASS };
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(expected)),
            "case {case:?}: expected the execution to fail under {expected:?}, got {described}"
        );
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a cause on {described}"))
            .to_string();
        if made {
            // The call was made, so the cause is the run's OWN error document.
            let document: Value = serde_json::from_str(&cause).unwrap_or_else(|error| {
                panic!("case {case:?}: expected the run's error document, got {error} for {cause:?}")
            });
            assert_eq!(
                document.get("errorType").and_then(Value::as_str),
                Some(RAISED_CLASS),
                "case {case:?}: expected the document to name the raised class, got {document}"
            );
            assert_eq!(
                document.get("errorMessage").and_then(Value::as_str),
                Some(format!("order o-{index} cannot be processed").as_str()),
                "case {case:?}: expected the message the handler raised, got {document}"
            );
        } else {
            // Nothing ran, so the cause is the refusal the gate answered for Lambda.
            assert!(
                cause.contains(REFUSAL_SENTENCE) && cause.contains(REFUSAL_CODE),
                "case {case:?}: expected the refusal to name the action and the code, got \
                 {cause:?}"
            );
        }
    }
}

/// What one row of the document rule names: the form the `Task` names its work in, which
/// function it invokes, and the class the failure carries.
struct DocumentRow {
    case: &'static str,
    named: Named,
    /// Whether the row invokes the raising function (else the sleeping one, whose runs the
    /// function's own `Timeout` ends).
    raises: bool,
}

/// A failed invocation's cause is the error document the run ANSWERED: byte for byte as the
/// invocation answered it where the `Resource` named the function, and as the document the
/// integration read out of the answer where the `Resource` named the optimized integration.
async fn a_failed_invocations_cause_is_the_document_the_run_answered(
    router: &MultiServiceRouter,
    raiser: &str,
    sleeper: &str,
    role: &str,
) {
    let rows = vec![
        DocumentRow {
            case: "a handler that raised, named by the function's own ARN",
            named: Named::FunctionArn,
            raises: true,
        },
        DocumentRow {
            case: "the same run, named by the optimized integration",
            named: Named::Optimized,
            raises: true,
        },
        DocumentRow {
            case: "a run the function's own Timeout ended, named by its ARN",
            named: Named::FunctionArn,
            raises: false,
        },
        DocumentRow {
            case: "the same run, named by the optimized integration",
            named: Named::Optimized,
            raises: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let DocumentRow { case, named, raises } = row;
        let (function, class) = if raises {
            (raiser, RAISED_CLASS)
        } else {
            (sleeper, TIMED_OUT_CLASS)
        };
        let event = json!({ "order": format!("o-{index}") });
        // What the invocation itself answers for this event, which is what the task's
        // cause is about.
        let answered =
            answered_document(router, case, if raises { RAISER } else { SLEEPER }, &event).await;
        let as_document: Value = serde_json::from_str(&answered).unwrap_or_else(|error| {
            panic!("case {case:?}: Invoke must answer an error document, got {error} for {answered:?}")
        });

        let described = ran(
            router,
            case,
            &format!("failure-identity-document-{index}"),
            &named.definition(function),
            &event,
            role,
        )
        .await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(class)),
            "case {case:?}: expected the execution to fail under {class:?}, got {described}"
        );

        let events = history(router, case, &string(&described, "executionArn")).await;
        let failed = detail(&events, named.failure_event(), named.failure_detail(), case);
        let ended = detail(&events, "ExecutionFailed", "executionFailedEventDetails", case);
        let (task_error, task_cause) = failure(&failed, case);
        let (execution_error, execution_cause) = failure(&ended, case);
        assert_eq!(
            (task_error.as_str(), execution_error.as_str()),
            (class, class),
            "case {case:?}: expected both failures to name {class:?}, got {failed} and {ended}"
        );
        assert_eq!(
            task_cause, execution_cause,
            "case {case:?}: expected the execution's cause to repeat the task's, got {failed} and \
             {ended}"
        );

        // Whatever the form, the cause IS that document: the same members carrying the
        // same values, the class the run reported among them.
        let carried: Value = serde_json::from_str(&task_cause).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the cause to be the error document, got {error} for \
                    {task_cause:?}")
        });
        assert_eq!(
            without_ids(&carried.to_string()),
            without_ids(&as_document.to_string()),
            "case {case:?}: expected the document the invocation answered, got {carried}"
        );
        assert_eq!(
            carried.get("errorType").and_then(Value::as_str),
            Some(class),
            "case {case:?}: expected the document to name {class:?}, got {carried}"
        );
        match named {
            // The invocation's answer IS this form's result, so the document reaches the
            // state as its writer wrote it — the separators and the member order of the
            // runtime (for a raise) or of the platform (for a run it stopped) included.
            Named::FunctionArn => assert_eq!(
                without_ids(&task_cause),
                without_ids(&answered),
                "case {case:?}: expected the invocation's own text, got {task_cause:?}"
            ),
            // The optimized form's result is the SDK response, whose `Payload` member this
            // emulator read as a document, so the cause is that document as rendered.
            Named::Optimized => assert_eq!(
                without_ids(&task_cause),
                without_ids(&carried.to_string()),
                "case {case:?}: expected the document as the integration renders it, got \
                 {task_cause:?}"
            ),
        }
    }
}

/// The string `member` of `document`.
fn string(document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected {member} in {document}"))
        .to_string()
}

/// A redriven attempt resumes the step the last attempt stopped at WITHOUT entering it
/// again, so the first event it writes for that step names the preserved event that entered
/// it — not the event the attempt opened with.
async fn a_redriven_attempt_chains_its_first_event_to_the_preserved_entry(
    router: &MultiServiceRouter,
    raiser: &str,
    role: &str,
) {
    // The rows differ in the form the `Task` names its work in, which decides the event
    // family the resumed step writes — the chain is the same rule either way.
    for (index, named) in [Named::FunctionArn, Named::Optimized].into_iter().enumerate() {
        let case = match named {
            Named::FunctionArn => "a step named by the function's own ARN",
            Named::Optimized => "a step named by the optimized integration",
        };
        let described = ran(
            router,
            case,
            &format!("failure-identity-redrive-{index}"),
            &named.definition(raiser),
            &json!({ "order": "o-1" }),
            role,
        )
        .await;
        let execution = string(&described, "executionArn");
        let first = history(router, case, &execution).await;
        let entered = first
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some("TaskStateEntered"))
            .and_then(|event| event.get("id").and_then(Value::as_i64))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the state's entry, got {:?}", types_of(&first))
            });

        sfn(
            router,
            case,
            "RedriveExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
        settled(router, case, &execution).await;
        let events = history(router, case, &execution).await;

        // The attempt opens with its own event, and the step it resumes is NOT entered
        // again: no second entry is recorded for it.
        let redriven = events
            .iter()
            .position(|event| {
                event.get("type").and_then(Value::as_str) == Some("ExecutionRedriven")
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the redrive's own event, got {:?}", types_of(&events))
            });
        let entries = types_of(&events)
            .iter()
            .filter(|kind| *kind == "TaskStateEntered")
            .count();
        assert_eq!(
            entries, 1,
            "case {case:?}: expected the preserved entry alone, got {:?}",
            types_of(&events)
        );
        let resumed = events
            .get(redriven + 1)
            .unwrap_or_else(|| panic!("case {case:?}: expected the resumed step's own event"));
        assert_eq!(
            resumed.get("type").and_then(Value::as_str),
            Some(named.scheduled_event()),
            "case {case:?}: expected the work to be handed over again, got {resumed}"
        );
        assert_eq!(
            resumed.get("previousEventId").and_then(Value::as_i64),
            Some(entered),
            "case {case:?}: expected the resumed step to follow its preserved entry \
             ({entered}), got {resumed}"
        );
        // And the events after it chain to the one before them, as "events are numbered
        // sequentially, starting at one" requires of the attempt's own run.
        for pair in events[redriven + 2..].windows(2) {
            let [before, after] = pair else { continue };
            assert_eq!(
                after.get("previousEventId").and_then(Value::as_i64),
                before.get("id").and_then(Value::as_i64),
                "case {case:?}: expected {after} to follow {before}"
            );
        }
    }
}
