// SPDX-License-Identifier: CC0-1.0

//! The event-driven chain a Lambda handler STARTS with the AWS SDK its managed Python
//! runtime supplies, and what Step Functions does with the work on the other end of it:
//! Lambda, EventBridge, Step Functions, Amazon S3, IAM, the token service and
//! CloudFormation mounted on ONE engine, as `core/router/src/lib.rs` assembles them, so
//! every call a handler makes is served by the store these suites read.
//!
//! RULE 1: a managed `python3.x` runtime puts the AWS SDK on the handler's import path and
//! the invocation's own Region and role credentials in its environment, so a handler
//! deployed as a bare package with no layer imports the SDK and the call it makes with it
//! reaches the service it names as a session of the function's execution role —
//! `Invoke` answers HTTP 200 with no `X-Amz-Function-Error` (`Invoke` binds that member to
//! that header, so a module that could not be imported is reported there) and the
//! handler's returned document as the payload, the call's effect is in the addressed
//! service's own state, and a call the role does not grant earns that service's OWN
//! refusal inside the handler rather than a failure to import.
//!
//! RULE 2: Step Functions assumes the state machine's `roleArn` before it hands over any
//! `Task`'s work, so an execution whose role's trust policy does not admit this service's
//! principal ends `FAILED` on the refusal of that assume — which the task's own failure
//! event carries too — with the function never run, so no side effect of it exists; one
//! whose role admits it runs the task and ends `SUCCEEDED` with that side effect in place.
//!
//! RULE 3: a `Task` state whose work is a Lambda invocation contributes the
//! `LambdaFunction*` family to the execution history beside the `TaskStateEntered`/
//! `TaskStateExited` pair — `LambdaFunctionScheduled` carrying the resolved resource and
//! the input the task was scheduled with, `LambdaFunctionStarted`, and
//! `LambdaFunctionSucceeded` carrying the function's own output.
//!
//! RULE 4: a `Task` whose `Resource` ends in `.waitForTaskToken` hands its token to the
//! work it dispatches and holds the execution `RUNNING` until that token is redeemed:
//! `SendTaskSuccess` completes the task with the output supplied and the execution
//! CONTINUES TO THE NEXT STATE, and `SendTaskFailure` fails it with the `error` and
//! `cause` supplied, so the execution ends `FAILED` and no later state runs.
//!
//! Seven crates have to agree for any of them to hold, so the suite lives here: a
//! crate-local suite can only ever see one side of it. What the neighbours already pin and
//! this one does not repeat: the Lambda crate's `python_runtime_bundled_sdk` pins WHICH
//! runtimes and import forms resolve the SDK, `sfn_lambda_task_handler_writes_objects`
//! pins what a task's handler can write, the Step Functions crate's
//! `execution_role_assumption` pins how an unassumable role is WORDED and its
//! `task_work_event_families` which family a `Resource` writes, and its
//! `lambda_callback_task_token` pins a token answered from inside the invocation that
//! carried it. What only this suite reaches is the chain END TO END: a handler's own SDK
//! call starting downstream work through another service's routing, the ABSENCE of a
//! task's side effect when the execution role cannot be assumed, and a token that leaves
//! the invocation for an external worker whose redemption drives the execution on into a
//! LATER state.
//!
//! The rows differ along what the rules generalise over. Rule 1: which service and
//! operation the in-function SDK call names (a publication onto an event bus, the start of
//! an execution, the stop of one, an object write) and whether the execution role grants
//! it. Rules 2 and 3: what the execution role's trust policy says about this service's
//! principal (admits it, admits another service's instead, denies it outright). Rule 4:
//! how the token is redeemed (a success carrying an object, a success carrying a scalar, a
//! failure carrying an error and a cause, a failure carrying an error alone).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The reader of one element of an awsQuery answer, the percent-encoder for an
// `application/x-www-form-urlencoded` component and the stack read: the `all` target is one
// crate root, so this suite borrows all three rather than carrying a second copy.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the bus, the bucket, the
/// functions and the state machines belong to.
const REGION: &str = "us-east-1";

/// The statuses an execution reports: the two terminal ones these suites end on, the one it
/// sits in while its work is outstanding, and the one a stop leaves it in
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";
const ABORTED: &str = "ABORTED";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` a stack that created everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the call reached and not how fast
/// an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// The bound the callback task declares, in seconds. Generous: no row waits it out, and
/// each redeems its token long before it runs out however loaded the host is.
const CALLBACK_TIMEOUT: u64 = 300;

/// How long the holding state machine's one `Wait` state waits, in seconds: long enough
/// that an execution started on it is still `RUNNING` when a row reads it, so what a stop
/// did is the only thing that can have moved it.
const HOLD_SECONDS: u64 = 300;

/// How often to look while waiting for work the clock owes — a stack reaching
/// [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an execution leaving
/// [`RUNNING`], the object a handler owes the bucket — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The service principals a role's trust policy is written for here: the one Step Functions
/// assumes an execution role as, the one Lambda assumes a function's execution role as, and
/// the one EventBridge assumes a target role as.
const SFN_PRINCIPAL: &str = "states.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The code and status an AWS JSON service refuses an unauthorized call with (the
/// AWS-common `AccessDeniedException`, HTTP 400), and the pair Amazon S3's REST surface
/// refuses one with instead (`AccessDenied`, HTTP 403).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_EXCEPTION_STATUS: u64 = 400;
const S3_ACCESS_DENIED: &str = "AccessDenied";
const S3_ACCESS_DENIED_STATUS: u64 = 403;

/// The bucket every handler below writes to, and the prefix it writes under.
const BUCKET: &str = "xsvc-in-function-chain";
const PREFIX: &str = "chain/";

/// The `source` and `detail-type` the rule of the first suite matches on, which is the
/// custom pair the handler publishes.
const EVENT_SOURCE: &str = "xsvc.in-function";
const EVENT_DETAIL_TYPE: &str = "InFunctionPublication";

/// The body the second state of the callback machine writes, which is there only if the
/// execution went on to that state at all.
const CONTINUED_BODY: &str = "the execution continued";

/// The handler of the first suite: it imports the SDK the runtime supplies and makes the ONE
/// call the event names, reporting what the service answered — or, where the role does not
/// grant the call, the refusal that service itself raised, as the SDK's own error class
/// carries it.
const CALLING_HANDLER: &str = r#"
import json

import boto3
import botocore.exceptions


def handler(event, context):
    call = event["call"]
    try:
        if call == "put_events":
            answered = boto3.client("events").put_events(
                Entries=[
                    {
                        "EventBusName": event["bus"],
                        "Source": event["source"],
                        "DetailType": event["detail_type"],
                        "Detail": json.dumps(event["detail"]),
                    }
                ]
            )
            return {
                "failed": answered["FailedEntryCount"],
                "published": [bool(entry.get("EventId")) for entry in answered["Entries"]],
            }
        if call == "start_execution":
            answered = boto3.client("stepfunctions").start_execution(
                stateMachineArn=event["state_machine"],
                name=event["execution_name"],
                input=json.dumps(event["detail"]),
            )
            return {"started": answered["executionArn"]}
        if call == "stop_execution":
            boto3.client("stepfunctions").stop_execution(
                executionArn=event["execution"],
                error=event["stop_error"],
                cause=event["stop_cause"],
            )
            return {"stopped": event["execution"]}
        if call == "put_object":
            answered = boto3.client("s3").put_object(
                Bucket=event["bucket"],
                Key=event["key"],
                Body=event["body"].encode("utf-8"),
            )
            return {"etagged": bool(answered.get("ETag")), "key": event["key"]}
    except botocore.exceptions.ClientError as error:
        return {
            "refused": {
                "code": error.response["Error"]["Code"],
                "message": error.response["Error"]["Message"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            }
        }
    return {"unknown": call}
"#;

/// The handler a `Task` state's work is in the second and third suites: it writes the one
/// object the task was handed and returns what it wrote, so the task's own result and the
/// bucket's contents are two readings of the same run.
const WRITING_HANDLER: &str = r#"
import boto3


def handler(event, context):
    body = event["body"].encode("utf-8")
    boto3.client("s3").put_object(Bucket=event["bucket"], Key=event["key"], Body=body)
    return {"wrote": event["key"], "length": len(body)}
"#;

/// The handler the callback task hands its token to: it leaves the token where an EXTERNAL
/// worker finds it — one object of the bucket — and answers nothing itself, so the token can
/// only be redeemed from outside the invocation that carried it.
const HANDING_OFF_HANDLER: &str = r#"
import boto3


def handler(event, context):
    boto3.client("s3").put_object(
        Bucket=event["bucket"], Key=event["key"], Body=event["token"].encode("utf-8")
    )
    return {"handed_off": event["key"]}
"#;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched
/// back into this same router, so the bus, the bucket and the state machines it addresses
/// are the ones this suite reads.
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

/// One response of a REST surface: its status, its body's own bytes, and the function-error
/// signal `Invoke` binds to a header.
struct Answer {
    status: u16,
    body: Vec<u8>,
    function_error: Option<String>,
}

impl Answer {
    /// The body as text, for a refusal or a document.
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

/// One request against a REST surface of `service`, answered with its status, its bytes and
/// the function-error header.
async fn rest(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answer {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let function_error = response
        .headers()
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
        function_error,
    }
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answer {
    rest(
        router,
        "s3",
        method,
        path,
        "application/octet-stream",
        body.to_vec(),
    )
    .await
}

/// One Lambda `restJson1` request.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    rest(router, "lambda", method, uri, "application/json", payload).await
}

/// One call to an AWS JSON service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
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

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), requiring a 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call of `service`, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One IAM call, requiring a 200, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {body}"
    );
    body
}

/// A trust policy with one statement: `effect` for the service principal `principal` over
/// `sts:AssumeRole`.
fn trust(effect: &str, principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// A role carrying `trust_document` as its trust policy and `statements` inline; answers the
/// ARN IAM minted for it.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    trust_document: &Value,
    statements: Value,
) -> String {
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_document.to_string()),
        ],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// The statement every function role carries, so a run can write its own logs.
fn logs_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// The assumed-role session an SDK call made by an invocation of `function` speaks for: the
/// session is named for the FUNCTION, and it is the principal a refusal names.
fn session(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// Create the bucket every handler below writes to.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let answer = s3(router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: CreateBucket({BUCKET:?}) must answer 200, got {} with {}",
        answer.status,
        answer.text()
    );
}

/// One function a stack declares: its logical id, its name, the role it runs as, and its
/// handler's source.
struct Declared<'a> {
    logical: &'a str,
    name: &'a str,
    role_arn: &'a str,
    source: &'a str,
}

/// Deploy `functions` in ONE stack — so the provisioning spans their creations owe run down
/// together — and answer each one's ARN, in the order declared, once Lambda reports it
/// [`FUNCTION_ACTIVE`].
///
/// The handlers are declared as the stack's inline source, which CloudFormation archives
/// into each deployment package: the way a function whose handler is written out in a suite
/// comes to exist at all, with no layer of its own.
async fn given_functions(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    functions: &[Declared<'_>],
) -> Vec<String> {
    let mut resources = serde_json::Map::new();
    for function in functions {
        resources.insert(
            function.logical.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function.name,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": function.role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": function.source },
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
    );

    // The stack's completion is owed from the instant Lambda reports each function created,
    // which no fixed sleep names: this looks until it reports it.
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the stack {stack:?} must reach {CREATE_COMPLETE}, last saw {}",
        reported.1
    );

    let mut arns = Vec::new();
    for function in functions {
        arns.push(awaited_function(router, case, function.name).await);
    }
    arns
}

/// The ARN of `name` once Lambda reports it [`FUNCTION_ACTIVE`].
async fn awaited_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        configuration = lambda(router, "GET", &uri, &Value::Null).await.json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: {name:?} must report its ARN, got {configuration}")
            });
            return arn.to_string();
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: {name:?} must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// Invoke `name` with `event`, answering the document the handler returned — asserting on
/// the way that the run reported no function error, which is what a runtime that could not
/// import the SDK reports instead of a result.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answer = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.function_error, None,
        "case {case:?}: expected the handler's own result, got a function error with {}",
        answer.text()
    );
    answer.json()
}

/// Create a state machine over `definition` and answer its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &Value,
    role_arn: &str,
) -> String {
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": role_arn,
        }),
    )
    .await;
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
        .to_string()
}

/// Start one execution carrying `input` and answer its ARN.
async fn started(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> String {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": name,
            "input": input.to_string(),
        }),
    )
    .await;
    started["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartExecution must answer an ARN, got {started}"))
        .to_string()
}

/// What `DescribeExecution` reports for `execution_arn` right now.
async fn described(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    sfn(
        router,
        case,
        "DescribeExecution",
        &json!({ "executionArn": execution_arn }),
    )
    .await
}

/// The execution `execution_arn` once it has left [`RUNNING`].
async fn settled(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = described(router, case, execution_arn).await;
        if reported["status"].as_str() != Some(RUNNING) {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {reported}");
}

/// The executions of `machine_arn` right now, as `ListExecutions` reports them.
async fn listed(router: &MultiServiceRouter, case: &str, machine_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn }),
    )
    .await;
    answered["executions"].as_array().cloned().unwrap_or_default()
}

/// The history of `execution_arn`, as its events' types in the order they happened.
async fn event_types(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Vec<String> {
    history(router, case, execution_arn)
        .await
        .iter()
        .filter_map(|event| event["type"].as_str().map(ToString::to_string))
        .collect()
}

/// The events of `execution_arn`, as `GetExecutionHistory` reports them.
async fn history(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn }),
    )
    .await;
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected events in {answered}"))
}

/// The one event of `event_type` in `events`, where exactly one is expected.
fn only<'a>(events: &'a [Value], event_type: &str, case: &str) -> &'a Value {
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"].as_str() == Some(event_type))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {event_type}, got {:?}",
        events
            .iter()
            .filter_map(|event| event["type"].as_str())
            .collect::<Vec<_>>()
    );
    found[0]
}

/// The position of the one event of `event_type` in `types`.
fn position(types: &[String], event_type: &str, case: &str) -> usize {
    types
        .iter()
        .position(|seen| seen == event_type)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {event_type} among {types:?}"))
}

/// The JSON document a member of a response carries as text — an execution's `input` or
/// `output`, a history detail's own.
fn document(carrier: &Value, member: &str, case: &str) -> Value {
    let text = carrier[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} carrying a JSON document, got {carrier}")
    });
    serde_json::from_str(text).unwrap_or_else(|why| {
        panic!("case {case:?}: the {member} must be a JSON document, got {text:?} ({why})")
    })
}

/// The object at `key`, looked for again until the bucket holds it: a handler's write is
/// owed from the instant its invocation made it, which no fixed sleep names.
async fn awaited_object(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let mut last = String::new();
    for _ in 0..LOOKS {
        let answer = s3(router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
        if answer.status == 200 {
            return answer.body;
        }
        last = format!("{} {}", answer.status, answer.text());
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the bucket must hold {key:?}, last saw {last}");
}

/// Which call the handler of the first suite makes, which is one axis its rows differ along.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Call {
    /// A publication onto a custom event bus a rule of that bus matches.
    PutEvents,
    /// The start of an execution of another state machine.
    StartExecution,
    /// The stop of an execution this suite started.
    StopExecution,
    /// An object write.
    PutObject,
}

impl Call {
    /// The token the handler dispatches on.
    fn token(self) -> &'static str {
        match self {
            Self::PutEvents => "put_events",
            Self::StartExecution => "start_execution",
            Self::StopExecution => "stop_execution",
            Self::PutObject => "put_object",
        }
    }

    /// The IAM action the call is authorized against, which is what its refusal names.
    fn action(self) -> &'static str {
        match self {
            Self::PutEvents => "events:PutEvents",
            Self::StartExecution => "states:StartExecution",
            Self::StopExecution => "states:StopExecution",
            Self::PutObject => "s3:PutObject",
        }
    }

    /// The code and status the addressed service refuses the call with when the role does
    /// not grant it: its own, which is not the same pair for a REST surface as for an AWS
    /// JSON one.
    fn refusal(self) -> (&'static str, u64) {
        match self {
            Self::PutObject => (S3_ACCESS_DENIED, S3_ACCESS_DENIED_STATUS),
            _ => (ACCESS_DENIED_EXCEPTION, ACCESS_DENIED_EXCEPTION_STATUS),
        }
    }
}

/// Every row of the first suite: each call, made once by a function whose role grants it and
/// once by a function whose role does not.
fn calling_rows() -> Vec<(Call, bool)> {
    let mut rows = Vec::new();
    for call in [
        Call::PutEvents,
        Call::StartExecution,
        Call::StopExecution,
        Call::PutObject,
    ] {
        rows.push((call, true));
        rows.push((call, false));
    }
    rows
}

/// The execution of `machine_arn` whose input carries `marker`, if there is one.
async fn execution_marked(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    marker: &str,
) -> Option<Value> {
    for execution in listed(router, case, machine_arn).await {
        let arn = match execution["executionArn"].as_str() {
            Some(arn) => arn.to_string(),
            None => continue,
        };
        let reported = described(router, case, &arn).await;
        let input = document(&reported, "input", case);
        let carried = input["marker"]
            .as_str()
            .or_else(|| input["detail"]["marker"].as_str());
        if carried == Some(marker) {
            return Some(reported);
        }
    }
    None
}

/// The execution of `machine_arn` the publication owes, looked for again until it is there.
async fn awaited_execution_marked(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    marker: &str,
) -> Value {
    for _ in 0..LOOKS {
        if let Some(found) = execution_marked(router, case, machine_arn, marker).await {
            return found;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: an execution carrying {marker:?} must be started on {machine_arn}");
}

/// RULE 1: the managed runtime's own SDK is importable and credentialed, so the one call a
/// handler makes with it reaches the service it names and leaves its effect there — and a
/// call the execution role does not grant earns that service's own refusal, inside the
/// handler, rather than a failure to import.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_function_sdk_call_reaches_the_service_it_names() {
    let case = "a handler calling one service with the SDK its runtime supplies";
    let router = build_router();

    given_bucket(&router, case).await;

    // The bus the publication lands on, and the machine the bus's rule starts.
    let bus = events(
        &router,
        case,
        "CreateEventBus",
        &json!({ "Name": "xsvc-in-function-bus" }),
    )
    .await;
    let bus_arn = bus["EventBusArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must answer an ARN, got {bus}"))
        .to_string();
    let machine_role = given_role(
        &router,
        case,
        "xsvc-in-function-machine-role",
        &trust("Allow", SFN_PRINCIPAL),
        json!([]),
    )
    .await;
    let notified_arn = given_machine(
        &router,
        case,
        "xsvc-in-function-notified",
        &json!({
            "StartAt": "Record",
            "States": { "Record": { "Type": "Pass", "End": true } },
        }),
        &machine_role,
    )
    .await;
    let holding_arn = given_machine(
        &router,
        case,
        "xsvc-in-function-holding",
        &json!({
            "StartAt": "Hold",
            "States": { "Hold": { "Type": "Wait", "Seconds": HOLD_SECONDS, "End": true } },
        }),
        &machine_role,
    )
    .await;

    let target_role = given_role(
        &router,
        case,
        "xsvc-in-function-target-role",
        &trust("Allow", EVENTS_PRINCIPAL),
        json!([{
            "Effect": "Allow",
            "Action": "states:StartExecution",
            "Resource": notified_arn.clone(),
        }]),
    )
    .await;
    events(
        &router,
        case,
        "PutRule",
        &json!({
            "Name": "xsvc-in-function-rule",
            "EventBusName": "xsvc-in-function-bus",
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [EVENT_SOURCE],
                "detail-type": [EVENT_DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let targeted = events(
        &router,
        case,
        "PutTargets",
        &json!({
            "Rule": "xsvc-in-function-rule",
            "EventBusName": "xsvc-in-function-bus",
            "Targets": [{
                "Id": "notified",
                "Arn": notified_arn.clone(),
                "RoleArn": target_role,
            }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_u64(),
        Some(0),
        "case {case:?}: PutTargets must accept the state-machine target, got {targeted}"
    );

    // Two roles, differing only in what they grant: the calls below are made by a function
    // running as each.
    let granting = "xsvc-in-function-granted-role";
    let granting_arn = given_role(
        &router,
        case,
        granting,
        &trust("Allow", LAMBDA_PRINCIPAL),
        json!([
            logs_statement(),
            { "Effect": "Allow", "Action": "events:PutEvents", "Resource": bus_arn },
            {
                "Effect": "Allow",
                "Action": "states:StartExecution",
                "Resource": holding_arn.clone(),
            },
            { "Effect": "Allow", "Action": "states:StopExecution", "Resource": "*" },
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
        ]),
    )
    .await;
    let withholding = "xsvc-in-function-ungranted-role";
    let withholding_arn = given_role(
        &router,
        case,
        withholding,
        &trust("Allow", LAMBDA_PRINCIPAL),
        json!([logs_statement()]),
    )
    .await;

    let granted_function = "xsvc-in-function-caller-granted";
    let ungranted_function = "xsvc-in-function-caller-ungranted";
    given_functions(
        &router,
        case,
        "xsvc-in-function-callers",
        &[
            Declared {
                logical: "GrantedCaller",
                name: granted_function,
                role_arn: &granting_arn,
                source: CALLING_HANDLER,
            },
            Declared {
                logical: "UngrantedCaller",
                name: ungranted_function,
                role_arn: &withholding_arn,
                source: CALLING_HANDLER,
            },
        ],
    )
    .await;

    for (call, granted) in calling_rows() {
        let marker = format!("{}-{}", call.token(), if granted { "ok" } else { "no" });
        let case = format!(
            "{} the role {}",
            call.action(),
            if granted { "grants" } else { "withholds" }
        );
        let case = case.as_str();
        let function = if granted {
            granted_function
        } else {
            ungranted_function
        };
        let role = if granted { granting } else { withholding };
        let key = format!("{PREFIX}{marker}.txt");

        // The stop rows need an execution to stop, which this suite starts itself so the
        // handler's call is the only thing that can have moved it.
        let stopped_arn = if call == Call::StopExecution {
            started(
                &router,
                case,
                &holding_arn,
                &marker,
                &json!({ "marker": marker }),
            )
            .await
        } else {
            String::new()
        };

        let event = json!({
            "call": call.token(),
            "bus": "xsvc-in-function-bus",
            "source": EVENT_SOURCE,
            "detail_type": EVENT_DETAIL_TYPE,
            "detail": { "marker": marker },
            "state_machine": holding_arn,
            "execution_name": marker,
            "execution": stopped_arn,
            "stop_error": "StoppedByTheHandler",
            "stop_cause": format!("the handler stopped {marker}"),
            "bucket": BUCKET,
            "key": key,
            "body": marker,
        });
        let reported = invoked(&router, case, function, &event).await;

        if !granted {
            let (code, status) = call.refusal();
            assert_eq!(
                reported["refused"]["code"].as_str(),
                Some(code),
                "case {case:?}: expected the addressed service's own refusal code, got {reported}"
            );
            assert_eq!(
                reported["refused"]["status"].as_u64(),
                Some(status),
                "case {case:?}: expected the addressed service's own status, got {reported}"
            );
            let opening = format!(
                "User: {} is not authorized to perform: {}",
                session(role, function),
                call.action()
            );
            let message = reported["refused"]["message"].as_str().unwrap_or_default();
            assert!(
                message.starts_with(&opening),
                "case {case:?}: expected a refusal opening {opening:?}, got {message:?}"
            );
        }

        match (call, granted) {
            (Call::PutEvents, true) => {
                assert_eq!(
                    reported["failed"].as_u64(),
                    Some(0),
                    "case {case:?}: expected no failed entry, got {reported}"
                );
                assert_eq!(
                    reported["published"],
                    json!([true]),
                    "case {case:?}: expected the bus to answer the entry an EventId, got \
                     {reported}"
                );
                // What the BUS did with it: the rule matched the custom pair and started
                // its state-machine target on the envelope the handler published.
                let execution =
                    awaited_execution_marked(&router, case, &notified_arn, &marker).await;
                let input = document(&execution, "input", case);
                assert_eq!(
                    input["detail-type"].as_str(),
                    Some(EVENT_DETAIL_TYPE),
                    "case {case:?}: expected the envelope's own detail-type, got {input}"
                );
                assert_eq!(
                    input["source"].as_str(),
                    Some(EVENT_SOURCE),
                    "case {case:?}: expected the envelope's own source, got {input}"
                );
                assert_eq!(
                    input["detail"],
                    json!({ "marker": marker }),
                    "case {case:?}: expected the detail the handler published, got {input}"
                );
            }
            (Call::PutEvents, false) => {
                assert!(
                    execution_marked(&router, case, &notified_arn, &marker)
                        .await
                        .is_none(),
                    "case {case:?}: a refused publication must start nothing, yet an execution \
                     carrying {marker:?} exists"
                );
            }
            (Call::StartExecution, true) => {
                let arn = reported["started"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected the execution ARN, got {reported}")
                });
                let execution = described(&router, case, arn).await;
                assert_eq!(
                    execution["status"].as_str(),
                    Some(RUNNING),
                    "case {case:?}: expected the started execution to be {RUNNING}, got \
                     {execution}"
                );
                assert_eq!(
                    execution["name"].as_str(),
                    Some(marker.as_str()),
                    "case {case:?}: expected the name the handler gave, got {execution}"
                );
                assert_eq!(
                    document(&execution, "input", case),
                    json!({ "marker": marker }),
                    "case {case:?}: expected the input the handler sent, got {execution}"
                );
            }
            (Call::StartExecution, false) => {
                let started_names: Vec<String> = listed(&router, case, &holding_arn)
                    .await
                    .iter()
                    .filter_map(|execution| execution["name"].as_str().map(ToString::to_string))
                    .collect();
                assert!(
                    !started_names.contains(&marker),
                    "case {case:?}: a refused start must start nothing, yet {marker:?} is among \
                     {started_names:?}"
                );
            }
            (Call::StopExecution, true) => {
                let execution = settled(&router, case, &stopped_arn).await;
                assert_eq!(
                    execution["status"].as_str(),
                    Some(ABORTED),
                    "case {case:?}: expected the stopped execution to be {ABORTED}, got \
                     {execution}"
                );
                assert_eq!(
                    execution["error"].as_str(),
                    Some("StoppedByTheHandler"),
                    "case {case:?}: expected the error the handler named, got {execution}"
                );
                assert_eq!(
                    execution["cause"].as_str(),
                    Some(format!("the handler stopped {marker}").as_str()),
                    "case {case:?}: expected the cause the handler named, got {execution}"
                );
            }
            (Call::StopExecution, false) => {
                let execution = described(&router, case, &stopped_arn).await;
                assert_eq!(
                    execution["status"].as_str(),
                    Some(RUNNING),
                    "case {case:?}: a refused stop must leave the execution {RUNNING}, got \
                     {execution}"
                );
            }
            (Call::PutObject, true) => {
                assert_eq!(
                    reported["etagged"],
                    json!(true),
                    "case {case:?}: expected the object store to answer the write with its \
                     ETag, got {reported}"
                );
                let held = awaited_object(&router, case, &key).await;
                assert_eq!(
                    String::from_utf8_lossy(&held),
                    marker,
                    "case {case:?}: expected the bucket to hold the handler's own bytes"
                );
            }
            (Call::PutObject, false) => {
                let answer = s3(&router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
                assert_eq!(
                    answer.status,
                    404,
                    "case {case:?}: a refused write must store nothing, got {} with {}",
                    answer.status,
                    answer.text()
                );
            }
        }
    }
}

/// One row of the second suite: what the execution role's trust policy says about this
/// service's principal, and whether the task's function is therefore run at all.
struct Trusted {
    case: &'static str,
    effect: &'static str,
    principal: &'static str,
    ran: bool,
}

fn trusted_rows() -> Vec<Trusted> {
    vec![
        Trusted {
            case: "a role whose trust admits this service's principal",
            effect: "Allow",
            principal: SFN_PRINCIPAL,
            ran: true,
        },
        Trusted {
            case: "a role whose trust admits another service's principal instead",
            effect: "Allow",
            principal: LAMBDA_PRINCIPAL,
            ran: false,
        },
        Trusted {
            case: "a role whose trust denies this service's principal outright",
            effect: "Deny",
            principal: SFN_PRINCIPAL,
            ran: false,
        },
    ]
}

/// RULES 2 and 3: the execution role is assumed before any `Task`'s work is handed over, so
/// a trust that does not admit this service leaves the execution `FAILED` on the refusal of
/// that assume with the function never run — the task's own failure carries the same refusal
/// and the object its handler would have written is absent — while a trust that admits it
/// runs the task, records the whole `LambdaFunction*` family for it, and leaves that object
/// behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untrusted_execution_role_runs_no_lambda_task() {
    let case = "a one-task machine run under each trust policy";
    let router = build_router();

    given_bucket(&router, case).await;

    let function_role = given_role(
        &router,
        case,
        "xsvc-trust-writer-role",
        &trust("Allow", LAMBDA_PRINCIPAL),
        json!([
            logs_statement(),
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
        ]),
    )
    .await;
    let function = "xsvc-trust-writer";
    let function_arn = given_functions(
        &router,
        case,
        "xsvc-trust-writers",
        &[Declared {
            logical: "Writer",
            name: function,
            role_arn: &function_role,
            source: WRITING_HANDLER,
        }],
    )
    .await
    .remove(0);

    for (index, row) in trusted_rows().into_iter().enumerate() {
        let case = row.case;
        let machine_role = given_role(
            &router,
            case,
            &format!("xsvc-trust-machine-{index}"),
            &trust(row.effect, row.principal),
            json!([{
                "Effect": "Allow",
                "Action": "lambda:InvokeFunction",
                "Resource": function_arn.clone(),
            }]),
        )
        .await;
        let definition = json!({
            "StartAt": "Write",
            "States": {
                "Write": {
                    "Type": "Task",
                    "Resource": function_arn.clone(),
                    "Parameters": {
                        "bucket": BUCKET,
                        "key.$": "$.key",
                        "body.$": "$.body",
                    },
                    "End": true,
                },
            },
        });
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-trust-machine-{index}"),
            &definition,
            &machine_role,
        )
        .await;

        let key = format!("{PREFIX}trust-{index}.txt");
        let body = format!("written under trust row {index}");
        let input = json!({ "key": key, "body": body });
        let execution_arn = started(&router, case, &machine_arn, "only", &input).await;
        let reported = settled(&router, case, &execution_arn).await;

        let types = event_types(&router, case, &execution_arn).await;
        if !row.ran {
            assert_eq!(
                reported["status"].as_str(),
                Some(FAILED),
                "case {case:?}: expected the execution to end {FAILED}, got {reported}"
            );
            // That the execution failed on the ASSUME is read loosely here — the sentence
            // itself has one owner, the Step Functions crate's `execution_role_assumption`
            // — so what this row asserts is that the cause names this service's principal
            // and the role the state machine was configured with, and nothing else.
            let cause = reported["cause"].as_str().unwrap_or_default();
            assert!(
                cause.contains(SFN_PRINCIPAL) && cause.contains(&machine_role),
                "case {case:?}: expected a cause naming {SFN_PRINCIPAL} and {machine_role}, got \
                 {cause:?}"
            );
            // The work failed on that same refusal and never produced anything: no success
            // for it, and none of the writes its handler makes.
            assert!(
                !types.iter().any(|seen| seen == "LambdaFunctionSucceeded"),
                "case {case:?}: expected no LambdaFunctionSucceeded, got {types:?}"
            );
            let events = history(&router, case, &execution_arn).await;
            let detail = only(&events, "LambdaFunctionFailed", case)
                ["lambdaFunctionFailedEventDetails"]
                .clone();
            assert_eq!(
                detail["cause"].as_str(),
                Some(cause),
                "case {case:?}: expected the work's own failure to carry the cause the execution \
                 reports, got {detail}"
            );
            let answer = s3(&router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
            assert_eq!(
                answer.status,
                404,
                "case {case:?}: expected no object to have been written, got {} with {}",
                answer.status,
                answer.text()
            );
            continue;
        }

        assert_eq!(
            reported["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the execution to end {SUCCEEDED}, got {reported}"
        );
        let returned = json!({ "wrote": key, "length": body.len() });
        assert_eq!(
            document(&reported, "output", case),
            returned,
            "case {case:?}: expected the function's own result as the output, got {reported}"
        );

        // The family the work's own events belong to, in the order they happened, inside the
        // state's own pair.
        for wanted in [
            "LambdaFunctionScheduled",
            "LambdaFunctionStarted",
            "LambdaFunctionSucceeded",
        ] {
            assert!(
                types.iter().any(|seen| seen == wanted),
                "case {case:?}: expected a {wanted} among {types:?}"
            );
        }
        let entered = position(&types, "TaskStateEntered", case);
        let scheduled = position(&types, "LambdaFunctionScheduled", case);
        let begun = position(&types, "LambdaFunctionStarted", case);
        let succeeded = position(&types, "LambdaFunctionSucceeded", case);
        let exited = position(&types, "TaskStateExited", case);
        assert!(
            entered < scheduled && scheduled < begun && begun < succeeded && succeeded < exited,
            "case {case:?}: expected the work's events inside the state's own pair, got {types:?}"
        );

        let events = history(&router, case, &execution_arn).await;
        let detail = only(&events, "LambdaFunctionScheduled", case)
            ["lambdaFunctionScheduledEventDetails"]
            .clone();
        assert_eq!(
            detail["resource"].as_str(),
            Some(function_arn.as_str()),
            "case {case:?}: expected the resolved resource on the scheduled event, got {detail}"
        );
        assert_eq!(
            document(&detail, "input", case),
            json!({ "bucket": BUCKET, "key": key, "body": body }),
            "case {case:?}: expected the input the task was scheduled with, got {detail}"
        );
        let detail = only(&events, "LambdaFunctionSucceeded", case)
            ["lambdaFunctionSucceededEventDetails"]
            .clone();
        assert_eq!(
            document(&detail, "output", case),
            returned,
            "case {case:?}: expected the function's output on the succeeded event, got {detail}"
        );

        let held = awaited_object(&router, case, &key).await;
        assert_eq!(
            String::from_utf8_lossy(&held),
            body,
            "case {case:?}: expected the bucket to hold what the task's handler wrote"
        );
    }
}

/// How one callback row's token is redeemed.
enum Redeemed {
    /// A `SendTaskSuccess` carrying this output, which the task's result must be.
    Success(Value),
    /// A `SendTaskFailure` carrying this error and, where the row supplies one, this cause —
    /// both optional members of that call
    /// (`smithy_query(query="operation",operation="SendTaskFailure")`).
    Failure {
        error: &'static str,
        cause: Option<&'static str>,
    },
}

/// One row of the third suite: how the token the paused task handed out is redeemed.
struct Callback {
    case: &'static str,
    redeemed: Redeemed,
}

fn callback_rows() -> Vec<Callback> {
    vec![
        Callback {
            case: "a success carrying an object output",
            redeemed: Redeemed::Success(json!({ "decision": "approved", "by": "the worker" })),
        },
        Callback {
            case: "a success carrying a scalar output",
            redeemed: Redeemed::Success(json!("approved")),
        },
        Callback {
            case: "a failure carrying an error and a cause",
            redeemed: Redeemed::Failure {
                error: "WorkerRejected",
                cause: Some("the worker declined the unit of work"),
            },
        },
        Callback {
            case: "a failure carrying an error alone",
            redeemed: Redeemed::Failure {
                error: "WorkerUnavailable",
                cause: None,
            },
        },
    ]
}

/// RULE 4: the paused task's token is the one the function was handed, the execution is held
/// while it is outstanding, and the redeeming call decides the task — a success carrying it
/// on into the next state, a failure ending it there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lambda_callback_task_is_ended_by_the_token_it_handed_out() {
    let case = "a machine whose first state hands its token to a function";
    let router = build_router();

    given_bucket(&router, case).await;

    let function_role = given_role(
        &router,
        case,
        "xsvc-callback-function-role",
        &trust("Allow", LAMBDA_PRINCIPAL),
        json!([
            logs_statement(),
            {
                "Effect": "Allow",
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
            },
        ]),
    )
    .await;
    let arns = given_functions(
        &router,
        case,
        "xsvc-callback-functions",
        &[
            Declared {
                logical: "HandOff",
                name: "xsvc-callback-handoff",
                role_arn: &function_role,
                source: HANDING_OFF_HANDLER,
            },
            Declared {
                logical: "Recorder",
                name: "xsvc-callback-recorder",
                role_arn: &function_role,
                source: WRITING_HANDLER,
            },
        ],
    )
    .await;
    let handoff_arn = arns[0].clone();
    let recorder_arn = arns[1].clone();

    let machine_role = given_role(
        &router,
        case,
        "xsvc-callback-machine-role",
        &trust("Allow", SFN_PRINCIPAL),
        json!([{
            "Effect": "Allow",
            "Action": "lambda:InvokeFunction",
            "Resource": [handoff_arn.clone(), recorder_arn.clone()],
        }]),
    )
    .await;
    // Two states: the callback task, and a LATER one that runs only if the token's
    // redemption carried the execution on to it.
    let definition = json!({
        "StartAt": "HandOff",
        "States": {
            "HandOff": {
                "Type": "Task",
                "Resource": "arn:aws:states:::lambda:invoke.waitForTaskToken",
                "Parameters": {
                    "FunctionName": handoff_arn,
                    "Payload": {
                        "token.$": "$$.Task.Token",
                        "bucket": BUCKET,
                        "key.$": "$.token_key",
                    },
                },
                "TimeoutSeconds": CALLBACK_TIMEOUT,
                "ResultPath": "$.worker",
                "Next": "Record",
            },
            "Record": {
                "Type": "Task",
                "Resource": recorder_arn,
                "Parameters": {
                    "bucket": BUCKET,
                    "key.$": "$.result_key",
                    "body": CONTINUED_BODY,
                },
                "ResultPath": "$.recorded",
                "End": true,
            },
        },
    });
    let machine_arn = given_machine(
        &router,
        case,
        "xsvc-callback-machine",
        &definition,
        &machine_role,
    )
    .await;

    for (index, row) in callback_rows().into_iter().enumerate() {
        let case = row.case;
        let token_key = format!("{PREFIX}token-{index}.txt");
        let result_key = format!("{PREFIX}continued-{index}.txt");
        let execution_arn = started(
            &router,
            case,
            &machine_arn,
            &format!("callback-{index}"),
            &json!({ "token_key": token_key, "result_key": result_key }),
        )
        .await;

        // The token the INVOCATION was handed, read where the function left it.
        let token = String::from_utf8_lossy(&awaited_object(&router, case, &token_key).await)
            .into_owned();
        assert!(
            !token.is_empty(),
            "case {case:?}: expected the function to have been handed a task token"
        );

        // Nothing has redeemed it, so the execution is held in the callback state.
        let holding = described(&router, case, &execution_arn).await;
        assert_eq!(
            holding["status"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected {RUNNING} while the token is outstanding, got {holding}"
        );

        match &row.redeemed {
            Redeemed::Success(output) => {
                sfn(
                    &router,
                    case,
                    "SendTaskSuccess",
                    &json!({ "taskToken": token, "output": output.to_string() }),
                )
                .await;
                let reported = settled(&router, case, &execution_arn).await;
                assert_eq!(
                    reported["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} once the token was redeemed, got \
                     {reported}"
                );
                let produced = document(&reported, "output", case);
                assert_eq!(
                    &produced["worker"], output,
                    "case {case:?}: expected the output the redeeming call supplied as the \
                     task's result, got {produced}"
                );
                // The execution went ON to the next state, whose own work is the only thing
                // that can have written this object.
                let held = awaited_object(&router, case, &result_key).await;
                assert_eq!(
                    String::from_utf8_lossy(&held),
                    CONTINUED_BODY,
                    "case {case:?}: expected the later state's own write"
                );
                assert_eq!(
                    produced["recorded"],
                    json!({ "wrote": result_key, "length": CONTINUED_BODY.len() }),
                    "case {case:?}: expected the later state's own result, got {produced}"
                );
            }
            Redeemed::Failure { error, cause } => {
                let mut request = json!({ "taskToken": token, "error": error });
                if let Some(cause) = cause {
                    request["cause"] = Value::String((*cause).to_string());
                }
                sfn(&router, case, "SendTaskFailure", &request).await;
                let reported = settled(&router, case, &execution_arn).await;
                assert_eq!(
                    reported["status"].as_str(),
                    Some(FAILED),
                    "case {case:?}: expected {FAILED} once the token was failed, got {reported}"
                );
                assert_eq!(
                    reported["error"].as_str(),
                    Some(*error),
                    "case {case:?}: expected the error the redeeming call named, got {reported}"
                );
                assert_eq!(
                    reported.get("cause").and_then(Value::as_str),
                    *cause,
                    "case {case:?}: expected the cause the redeeming call supplied, got \
                     {reported}"
                );
                let answer = s3(&router, "GET", &format!("/{BUCKET}/{result_key}"), b"").await;
                assert_eq!(
                    answer.status,
                    404,
                    "case {case:?}: a failed task must run no later state, got {} with {}",
                    answer.status,
                    answer.text()
                );
            }
        }
    }
}
