// SPDX-License-Identifier: CC0-1.0

//! The two halves of a Lambda/Amazon SQS/Step Functions pipeline that only exist where all
//! three services are assembled on one engine: a FUNCTION driving the workflow service and
//! the queue with the AWS SDK its managed runtime supplies, and a `Task` state driving a
//! FUNCTION whose work did not come back as a result.
//!
//! RULE 1: a managed `python3.x` runtime supplies the AWS SDK on the handler's import path
//! and the function role's own credentials in the handler's environment, so an SDK call the
//! handler makes is really served by the service it addresses, under the policies in force on
//! that role — the invocation answers HTTP 200 with no `X-Amz-Function-Error` (`Invoke` binds
//! that member to the `X-Amz-Function-Error` header, so a module that could not be imported is
//! reported there) and the handler's own document as the payload, the effect of a served call
//! is in the addressed service afterwards where a client reads it, and a call that service
//! refuses is reported to the handler as THAT service's own error code and status.
//!
//! RULE 2: a `Task` state's work is a result only when the invocation was served, returned,
//! and returned a document the state's own bounds admit. Every other outcome fails the task
//! under its own error name — the `errorType` of the run's error document for a handler that
//! raised, `States.Timeout` for a run longer than the state's `TimeoutSeconds`,
//! `States.DataLimitExceeded` with the service's quota sentence naming the task's resolved
//! resource for a result past the state-payload quota, and the invoked service's own SDK
//! exception for an execution role that is not permitted the integration's action — so a
//! `Catch` naming that error takes the failure over and routes it on, and an uncaught one ends
//! the execution `FAILED` carrying that error.
//!
//! Four crates have to agree for either rule to hold — CloudFormation archives the inline
//! handler into the deployment package, IAM holds the roles and documents every decision is
//! taken against, Lambda runs the handler and supplies it the SDK, Step Functions interprets
//! the definition and makes the integration calls, and Amazon SQS takes the messages and hands
//! them back — so the suite lives here: a crate-local suite can only ever see one side of it.
//! What the neighbours pin and this one does not repeat: `sfn_lambda_task_invocation` pins
//! that the invocation HAPPENS and is metered, `sfn_lambda_task_result_and_sqs_callback_token`
//! pins the function's returned value as the state's result and the callback token's redemption,
//! and `lambda_in_function_sqs_authorization` pins the decision taken on one in-function queue
//! call. `events_lambda_sfn_in_function_chain` states rule 1 too, over an execution a handler
//! STARTS and one whose role does not grant the start; the overlap at those two rows is
//! deliberate, because what is new here needs the same ground under it — the workflow READS a
//! handler drives (a describe of the execution's status, a list under the status the execution
//! IS in and under another, and a stop whose effect a client then reads as `ABORTED` under the
//! error the handler named), the queue send and the refusal of a queue the service does not
//! hold, and then the whole of rule 2, which no neighbour states: the FAILURE of a `Task`
//! state's function work, routed on by the definition's own `Catch` to a queue a client drains.
//!
//! The rows differ along what the rules generalise over. For rule 1: WHICH operation of which
//! addressed service the handler calls (a workflow start, a read of what it started, a list
//! filtered two ways, a stop, a queue send), and why a call is refused (a queue the service
//! does not hold, and an operation the role is granted nothing of). For rule 2: WHAT kept the
//! invocation from being a result (a raised exception, a run longer than the state's bound, a
//! document past the state-payload quota, a role not permitted the call), and whether the
//! definition declares a `Catch` naming the error that outcome carries.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The stack poll, the percent-encoder for an `application/x-www-form-urlencoded` component and
// the readers of an awsQuery answer: the `all` target is one crate root, so this suite borrows
// them rather than carrying second copies.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};
// The log statement a STATE MACHINE's role carries, published by the suite that first needed
// it: a vended delivery write, which is not the set a function's own role carries.
use crate::sfn_task_target_authorization::logs_only;

/// The Region every request here is signed for, and so the Region the queues, the functions,
/// the roles and the state machines belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` every stack below must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The statuses an execution reaches (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const ABORTED: &str = "ABORTED";

/// The managed runtime every handler below is written for, and the module and attribute the
/// archived package carries it as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what any run
/// below needs on any host, so what ends a run is the handler or the STATE's bound and never
/// the function's own.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack settling, a function
/// reaching [`FUNCTION_ACTIVE`], an execution reaching a status, a message a queue is owed —
/// and how many looks to take. Those spans are the services' own, so no fixed sleep names the
/// instant one ends: the wait is on the CONDITION, with a deadline generous enough that a
/// loaded host cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How many looks a NEGATIVE check takes before it believes a queue was handed nothing. A
/// single look would also pass on a message that arrived a moment later, so the absence is
/// looked for repeatedly — a span, not one glance, and tiny beside [`LOOKS`] because what is
/// asserted is that no sending state exists on that path at all.
const EMPTY_LOOKS: usize = 20;

/// The quota a state's payload is measured against: "the maximum input or output size for a
/// task, state, or execution is 256 KiB of data as a UTF-8 encoded string", which is this many
/// bytes (Step Functions service quotas, *Quotas related to state machine executions*).
const STATE_PAYLOAD_LIMIT_BYTES: usize = 262_144;

/// How many bytes of filler the oversize row's handler returns: past
/// [`STATE_PAYLOAD_LIMIT_BYTES`] by construction, so the result is over the quota whatever the
/// rest of the document weighs, and far under what one invocation may answer with.
const OVERSIZE_FILLER_BYTES: usize = STATE_PAYLOAD_LIMIT_BYTES + 38_000;

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked here, as in every other suite of this crate.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK calls are dispatched
/// back into this same router, so the execution it starts and the message it sends are the ones
/// this suite reads back.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One awsQuery call of `service`, answered with its HTTP status and its XML body.
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One `awsJson1_0` call of the service `signing_name` routes to, as its status and body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
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

/// One Step Functions call that must be served.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// One Amazon SQS call that must be served, made as the account's own credential.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(router, "sqs", "AmazonSQS", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answered}"
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

/// A role `service` may assume, carrying `statements` as its inline document; answers the ARN
/// IAM persisted for it, which is what every grant and every refusal names.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    service: &str,
    statements: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({name:?}) must answer 200, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, attached) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({name:?}) must answer 200, got {status} with {attached}"
    );
    arn
}

/// The log statement a FUNCTION's execution role carries whatever else it does, which grants
/// nothing of any service a row addresses: the writes a function's own log stream needs.
///
/// A state machine's log statement is the other set — the vended delivery a state machine
/// writes through — so the roles below take [`logs_only`], the sibling suite's, for those.
fn function_log_writes() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// A statement allowing EVERY action of `actions` on every resource of this account.
///
/// The sibling suite's `allow` names one action and one resource, which is what a refusal there
/// is stated about; a role here is granted a whole set at once, because what these suites are
/// about is the call being made and not which ARN it was scoped to.
fn allow_all_of(actions: &[&str]) -> Value {
    json!({ "Effect": "Allow", "Action": actions, "Resource": "*" })
}

/// Deploy `template` as the stack `name`, wait for it to settle, and answer the physical id of
/// each declared resource.
///
/// An inline handler is archived into the function's deployment package by CloudFormation,
/// which is how a function whose handler is written out in a suite comes to exist at all.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &Value,
) -> BTreeMap<String, String> {
    let body = template.to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", name), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({name:?}) must be accepted, got {status} with {answered}"
    );
    let reported = wait_for_stack_status(router, name, CREATE_COMPLETE).await;
    if reported.0 != CREATE_COMPLETE {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", name)],
        )
        .await;
        panic!(
            "case {case:?}: the stack {name:?} must reach {CREATE_COMPLETE}, got {} with \
             resource failures {:?}",
            reported.0,
            elements(&events, "ResourceStatusReason")
        );
    }
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources({name:?}) must succeed, got {status} with \
         {resources}"
    );
    physical_ids_of(&resources)
}

/// Deploy `template` as `name` and assert the stack reports each logical id's physical id as
/// the resource the owning service then answers for.
async fn deployed_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &Value,
    expected: &[(&str, &str)],
) {
    let physical = deployed(router, case, name, template).await;
    for (logical, wanted) in expected {
        let reported = physical.get(*logical).cloned().unwrap_or_default();
        assert_eq!(
            reported, *wanted,
            "case {case:?}: expected the stack to report {logical} as {wanted:?}, got \
             {reported:?}"
        );
    }
}

/// One `AWS::Lambda::Function` resource running `source` as `role_arn`.
fn function_resource(name: &str, role_arn: &str, source: &str) -> Value {
    json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": name,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Role": role_arn,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": { "ZipFile": source },
        },
    })
}

/// Wait until Lambda reports `name` [`FUNCTION_ACTIVE`], which is the state an invocation
/// needs, and answer the ARN it reports — the resource a grant for it names.
async fn active_function_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(router, "GET", &uri, &[]).await;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {name:?} must report its ARN, got {configuration}")
                })
                .to_string();
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: Lambda must report {name:?} {FUNCTION_ACTIVE}, last read {last}");
}

/// Invoke `function` with `event`, asserting the CALL was served and the RUN did not fail, and
/// answer the handler's own document.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let reported = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        reported, None,
        "case {case:?}: expected the handler's own result, got the function error {reported:?} \
         with {payload}"
    );
    payload
}

/// The queue `name`, created and answered as the url Amazon SQS reports for it.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue({name:?}) must answer a url"))
        .to_string()
}

/// Take everything `url` still hands over, deleting each message so the next row starts from an
/// empty queue, and answer the bodies.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them or the
/// generous deadline passes, so a host slow to make a message available cannot decide an
/// outcome, and a caller expecting none takes one look and reports what it found.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<String> {
    let mut bodies = Vec::new();
    for look in 0..LOOKS {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        for message in &handed {
            bodies.push(
                message["Body"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a message with no body {message}"))
                    .to_string(),
            );
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
        if bodies.len() >= wanted || look + 1 == LOOKS {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    bodies
}

/// Assert `url` is handed NOTHING over [`EMPTY_LOOKS`] looks, deleting anything it does hand
/// over so a failure reports what arrived rather than leaving it for the next row.
async fn sends_nothing(router: &MultiServiceRouter, case: &str, url: &str) {
    for _ in 0..EMPTY_LOOKS {
        let found = drained(router, case, url, 0).await;
        assert!(
            found.is_empty(),
            "case {case:?}: expected nothing on the queue, got {found:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create the state machine `name` over `definition` as `role_arn`, and answer its ARN.
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

/// Start one execution of `machine_arn` carrying `input`, and answer its ARN.
async fn started(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> String {
    let answered = sfn(
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
    answered["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartExecution must answer an ARN"))
        .to_string()
}

/// What `DescribeExecution` reports for `execution_arn` right now.
async fn describe(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    sfn(
        router,
        case,
        "DescribeExecution",
        &json!({ "executionArn": execution_arn }),
    )
    .await
}

/// Look until the service reports `execution_arn` in `wanted`, and answer what it then reports.
async fn holds(
    router: &MultiServiceRouter,
    case: &str,
    execution_arn: &str,
    wanted: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let described = describe(router, case, execution_arn).await;
        if described["status"].as_str() == Some(wanted) {
            return described;
        }
        last = described;
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: expected the execution to be {wanted}, last read {last}");
}

/// Look until `execution_arn` has LEFT [`RUNNING`], and answer what it then reports.
async fn terminal(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let described = describe(router, case, execution_arn).await;
        if described["status"].as_str() != Some(RUNNING) {
            return described;
        }
        last = described;
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: expected the execution to reach a terminal status, last read {last}");
}

// ---------------------------------------------------------------------------------------------
// Rule 1: the function as the DRIVER
// ---------------------------------------------------------------------------------------------

/// The driving handler. It imports the AWS SDK at MODULE scope — a runtime shipping no SDK
/// fails the initialization phase there, before the handler body runs at all — builds a client
/// for the service the event names, makes the ONE call the event names, and reports either what
/// that call answered or the refusal the ADDRESSED service answered it with, as the SDK's own
/// error class carries it.
const DRIVER_HANDLER: &str = r#"
import json

import boto3
from botocore.exceptions import ClientError


def made(event):
    op = event["op"]
    if op == "start":
        answered = boto3.client("stepfunctions").start_execution(
            stateMachineArn=event["machineArn"],
            name=event["name"],
            input=json.dumps(event["payload"]),
        )
        return {"executionArn": answered["executionArn"]}
    if op == "describe":
        answered = boto3.client("stepfunctions").describe_execution(
            executionArn=event["executionArn"]
        )
        return {
            "executionArn": answered["executionArn"],
            "status": answered["status"],
        }
    if op == "list":
        answered = boto3.client("stepfunctions").list_executions(
            stateMachineArn=event["machineArn"], statusFilter=event["statusFilter"]
        )
        return {"arns": sorted(item["executionArn"] for item in answered["executions"])}
    if op == "stop":
        boto3.client("stepfunctions").stop_execution(
            executionArn=event["executionArn"], error=event["error"]
        )
        return {"stopped": event["executionArn"]}
    answered = boto3.client("sqs").send_message(
        QueueUrl=event["url"], MessageBody=event["body"]
    )
    return {"sent": bool(answered.get("MessageId"))}


def handler(event, context):
    try:
        return {"ok": True, "answered": made(event)}
    except ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// The stack rule 1's functions are deployed by, and the two functions it declares: one running
/// as a role granted every workflow and queue action a row calls, one running as a role granted
/// the reads but no stop.
const DRIVER_STACK: &str = "xsvc-sdkwf-drivers";
const DRIVER_FUNCTION: &str = "xsvc-sdkwf-driver";
/// See [`DRIVER_FUNCTION`].
const NARROW_FUNCTION: &str = "xsvc-sdkwf-narrow-driver";

/// The queue rule 1's send addresses, and the body it sends.
const DRIVER_QUEUE: &str = "xsvc-sdkwf-signals";
/// See [`DRIVER_QUEUE`].
const DRIVER_BODY: &str = "started by the handler";

/// A queue of this account that was never created, which the refused send addresses.
const ABSENT_QUEUE: &str = "xsvc-sdkwf-not-a-queue";

/// The code and status Amazon SQS refuses a call naming a queue it does not hold with: its own
/// legacy code for `QueueDoesNotExist` (`awsQueryError` code
/// `AWS.SimpleQueueService.NonExistentQueue`, HTTP 400), which is what the AWS SDK reports to
/// the handler.
const NO_SUCH_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
/// See [`NO_SUCH_QUEUE`].
const REFUSAL_STATUS: u64 = 400;

/// The code and status Step Functions refuses a call the policies in force do not permit with:
/// the AWS-common `AccessDeniedException`, HTTP 400.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The `error` the handler's stop names, which the stopped execution must then carry.
const STOP_ERROR: &str = "StoppedByTheHandler";

/// What one row's SDK call must come to.
enum Outcome {
    /// The call was served and the handler reported this document under `answered`.
    Answered(Value),
    /// The ADDRESSED service refused the call with this code, on [`REFUSAL_STATUS`].
    Refused(&'static str),
}

/// One row of rule 1: which function makes the call, the event naming it, what must come of it,
/// and the status the SERVICE must report for the driven execution right afterwards.
struct Driven {
    case: &'static str,
    function: &'static str,
    event: Value,
    outcome: Outcome,
    /// The status a client reading the driven execution must then see, for a row that says
    /// something about it.
    held: Option<&'static str>,
}

/// A handler running under a managed runtime drives the workflow service and the queue with the
/// SDK that runtime supplies: every served call's effect is in the addressed service afterwards,
/// and every refused call is reported as that service's own.
#[tokio::test]
async fn a_handler_drives_the_workflow_service_and_the_queue_with_the_runtime_sdk() {
    let case = "the SDK a managed runtime supplies, driving two services";
    let router = emulator();

    // The machine the handler drives: one long `Wait`, so an execution it starts is still
    // RUNNING at every later row however fast the host runs, and nothing it does depends on an
    // integration call.
    let machine_role = given_role(
        &router,
        case,
        "xsvc-sdkwf-machine-role",
        "states.amazonaws.com",
        json!([logs_only()]),
    )
    .await;
    let machine = given_machine(
        &router,
        case,
        "xsvc-sdkwf-waiting",
        &json!({
            "StartAt": "Hold",
            "States": { "Hold": { "Type": "Wait", "Seconds": 3600, "End": true } },
        }),
        &machine_role,
    )
    .await;

    let url = given_queue(&router, case, DRIVER_QUEUE).await;
    let absent_url = url.replace(DRIVER_QUEUE, ABSENT_QUEUE);
    assert_ne!(
        url, absent_url,
        "case {case:?}: the refused send must address another queue, got {url:?} for both"
    );

    // Two execution roles over the same handler: one granted every action a row calls, one
    // granted the reads and nothing that stops an execution.
    let driver_role = given_role(
        &router,
        case,
        "xsvc-sdkwf-driver-role",
        "lambda.amazonaws.com",
        json!([
            allow_all_of(&[
                "states:StartExecution",
                "states:DescribeExecution",
                "states:ListExecutions",
                "states:StopExecution",
            ]),
            allow_all_of(&["sqs:SendMessage"]),
            function_log_writes(),
        ]),
    )
    .await;
    let narrow_role = given_role(
        &router,
        case,
        "xsvc-sdkwf-narrow-role",
        "lambda.amazonaws.com",
        json!([
            allow_all_of(&[
                "states:StartExecution",
                "states:DescribeExecution",
                "states:ListExecutions",
            ]),
            function_log_writes(),
        ]),
    )
    .await;

    deployed_as(
        &router,
        case,
        DRIVER_STACK,
        &json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                "Driver": function_resource(DRIVER_FUNCTION, &driver_role, DRIVER_HANDLER),
                "Narrow": function_resource(NARROW_FUNCTION, &narrow_role, DRIVER_HANDLER),
            },
        }),
        &[("Driver", DRIVER_FUNCTION), ("Narrow", NARROW_FUNCTION)],
    )
    .await;
    for function in [DRIVER_FUNCTION, NARROW_FUNCTION] {
        active_function_arn(&router, case, function).await;
    }

    // The execution name the handler's start asks for, and the ARN that start must produce —
    // which is the record Step Functions itself holds, not one built here.
    let execution_name = "driven-by-the-handler";
    let execution_arn =
        format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:xsvc-sdkwf-waiting:{execution_name}");

    let rows = vec![
        Driven {
            case: "a write: the SDK start starts an execution a client then reads",
            function: DRIVER_FUNCTION,
            event: json!({
                "op": "start",
                "machineArn": machine.clone(),
                "name": execution_name,
                "payload": { "from": "the handler" },
            }),
            outcome: Outcome::Answered(json!({ "executionArn": execution_arn.clone() })),
            held: Some(RUNNING),
        },
        Driven {
            case: "a read: the SDK describe reports the status the service holds",
            function: DRIVER_FUNCTION,
            event: json!({ "op": "describe", "executionArn": execution_arn.clone() }),
            outcome: Outcome::Answered(json!({
                "executionArn": execution_arn.clone(),
                "status": RUNNING,
            })),
            held: Some(RUNNING),
        },
        Driven {
            case: "a list filtered by the status the execution is in carries it",
            function: DRIVER_FUNCTION,
            event: json!({
                "op": "list",
                "machineArn": machine.clone(),
                "statusFilter": RUNNING,
            }),
            outcome: Outcome::Answered(json!({ "arns": [execution_arn.clone()] })),
            held: None,
        },
        Driven {
            case: "a list filtered by another status carries none of it",
            function: DRIVER_FUNCTION,
            event: json!({
                "op": "list",
                "machineArn": machine.clone(),
                "statusFilter": SUCCEEDED,
            }),
            outcome: Outcome::Answered(json!({ "arns": [] })),
            held: None,
        },
        Driven {
            case: "a queue send: the SDK send puts the body on the queue",
            function: DRIVER_FUNCTION,
            event: json!({ "op": "send", "url": url.clone(), "body": DRIVER_BODY }),
            outcome: Outcome::Answered(json!({ "sent": true })),
            held: None,
        },
        Driven {
            case: "a queue the service does not hold is that service's own refusal",
            function: DRIVER_FUNCTION,
            event: json!({ "op": "send", "url": absent_url.clone(), "body": DRIVER_BODY }),
            outcome: Outcome::Refused(NO_SUCH_QUEUE),
            held: None,
        },
        Driven {
            case: "an action the role is granted nothing of is refused, and stops nothing",
            function: NARROW_FUNCTION,
            event: json!({
                "op": "stop",
                "executionArn": execution_arn.clone(),
                "error": STOP_ERROR,
            }),
            outcome: Outcome::Refused(ACCESS_DENIED),
            held: Some(RUNNING),
        },
        Driven {
            case: "a write the role IS granted: the SDK stop halts the running execution",
            function: DRIVER_FUNCTION,
            event: json!({
                "op": "stop",
                "executionArn": execution_arn.clone(),
                "error": STOP_ERROR,
            }),
            outcome: Outcome::Answered(json!({ "stopped": execution_arn.clone() })),
            held: Some(ABORTED),
        },
    ];

    for row in &rows {
        let reported = invoked(&router, row.case, row.function, &row.event).await;
        let expected = match &row.outcome {
            Outcome::Answered(answered) => json!({ "ok": true, "answered": answered }),
            Outcome::Refused(code) => {
                json!({ "ok": false, "code": code, "status": REFUSAL_STATUS })
            }
        };
        assert_eq!(
            reported, expected,
            "case {:?}: expected the handler to report {expected}, got {reported}",
            row.case
        );
        if let Some(wanted) = row.held {
            let held = holds(&router, row.case, &execution_arn, wanted).await;
            assert_eq!(
                held["status"].as_str(),
                Some(wanted),
                "case {:?}: expected the service to hold the execution {wanted}, got {held}",
                row.case
            );
        }
    }

    // The stop the handler made is the record Step Functions holds, under the error it named.
    let stopped = describe(&router, case, &execution_arn).await;
    assert_eq!(
        stopped["error"].as_str(),
        Some(STOP_ERROR),
        "case {case:?}: expected the stopped execution to carry the error the handler named, \
         got {stopped}"
    );

    // And what the QUEUE itself holds: the one served send, and nothing from the refused one.
    let held = drained(&router, case, &url, 1).await;
    assert_eq!(
        held,
        vec![DRIVER_BODY.to_string()],
        "case {case:?}: expected the queue to hold exactly the served send, got {held:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Rule 2: the `Task` state's function work that did not come back as a result
// ---------------------------------------------------------------------------------------------

/// The worked handler: what it does is decided by the `mode` the state's input carries, so each
/// row's outcome is that run's own rather than anything fixed in the definition.
const WORKER_HANDLER: &str = r#"
import time


class OrderRejected(Exception):
    pass


def handler(event, context):
    mode = event["mode"]
    if mode == "raise":
        raise OrderRejected("order %s cannot be processed" % event["order"])
    if mode == "sleep":
        time.sleep(event["seconds"])
        return {"slept": event["seconds"]}
    if mode == "oversize":
        return {"filler": "x" * event["bytes"]}
    return {"echoed": mode}
"#;

/// The stack rule 2's function is deployed by, and the function it declares.
const WORKER_STACK: &str = "xsvc-taskfail-worker";
const WORKER_FUNCTION: &str = "xsvc-taskfail-worker-fn";

/// The queue each `Catch` row's handler state sends the caught error name to.
const FAILURE_QUEUE: &str = "xsvc-taskfail-notices";

/// The error a `Task` state reports when the execution role may not make the call: Lambda's
/// base SDK exception, which is what a refusal `Invoke` does not model arrives under.
const REFUSED_INVOKE_ERROR: &str = "Lambda.AWSLambdaException";
/// The fixed fragment of the sentence that refusal carries, naming the action Lambda publishes
/// for `Invoke`.
const REFUSED_INVOKE_TEXT: &str = "not authorized to perform: lambda:InvokeFunction";

/// The error a task whose run outlived the state's own bound reports ("States.Timeout").
const TIMEOUT_ERROR: &str = "States.Timeout";

/// The error a result past [`STATE_PAYLOAD_LIMIT_BYTES`] reports ("States.DataLimitExceeded").
const DATA_LIMIT_ERROR: &str = "States.DataLimitExceeded";

/// The exception class the raising row's handler ends with, which is the `errorType` of its
/// error document and so the name the failed task carries.
const RAISED_ERROR: &str = "OrderRejected";

/// The whole cause a document past the state-payload quota fails with: the service's own
/// sentence, whose only variable part is the task's resolved resource.
fn result_too_large(resource: &str) -> String {
    format!(
        "The state/task '{resource}' returned a result with a size exceeding the maximum number \
         of bytes service limit."
    )
}

/// How long the overrunning row's handler sleeps, and the `TimeoutSeconds` its state declares.
/// The sleep is comfortably past the bound, and the bound is the one limit this row tests, so it
/// is the only tight figure here.
const SLEEP_SECONDS: u64 = 4;
/// See [`SLEEP_SECONDS`].
const STATE_BOUND_SECONDS: u64 = 1;

/// One row of rule 2: what kept the invocation from being a result, and the error the task must
/// then carry.
struct Unservable {
    case: &'static str,
    /// The input the execution carries, which is the event the function is handed.
    input: Value,
    /// The `TimeoutSeconds` the state declares, for the row whose bound is what closes it.
    bound: Option<u64>,
    /// Whether the state machine runs under a role that is NOT permitted the invocation.
    denied: bool,
    error: &'static str,
    /// Whether this row also drives the `Catch` half: a failure whose error a catcher names is
    /// taken over by it.
    caught: bool,
}

fn unservable_rows() -> Vec<Unservable> {
    vec![
        Unservable {
            case: "a handler that raises its own exception class",
            input: json!({ "mode": "raise", "order": 17 }),
            bound: None,
            denied: false,
            error: RAISED_ERROR,
            caught: true,
        },
        Unservable {
            case: "a run longer than the state's own TimeoutSeconds",
            input: json!({ "mode": "sleep", "seconds": SLEEP_SECONDS }),
            bound: Some(STATE_BOUND_SECONDS),
            denied: false,
            error: TIMEOUT_ERROR,
            caught: true,
        },
        Unservable {
            case: "a result past the state-payload quota",
            input: json!({ "mode": "oversize", "bytes": OVERSIZE_FILLER_BYTES }),
            bound: None,
            denied: false,
            error: DATA_LIMIT_ERROR,
            // The language states this failure is the execution's: nothing retries or catches
            // it, so this row drives only the uncaught half.
            caught: false,
        },
        Unservable {
            case: "an execution role not permitted the invocation",
            input: json!({ "mode": "echo" }),
            bound: None,
            denied: true,
            error: REFUSED_INVOKE_ERROR,
            caught: true,
        },
    ]
}

/// The definition one row runs: ONE `Task` state naming the function by its ARN, with the row's
/// own bound, and — for the `Catch` half — a catcher naming the error that row's outcome carries
/// and a handler state that sends that error name to the queue.
fn task_definition(
    function_arn: &str,
    bound: Option<u64>,
    catch: Option<(&str, &str)>,
) -> Value {
    let mut task = serde_json::Map::new();
    task.insert("Type".to_string(), json!("Task"));
    task.insert("Resource".to_string(), json!(function_arn));
    if let Some(seconds) = bound {
        task.insert("TimeoutSeconds".to_string(), json!(seconds));
    }
    let mut states = serde_json::Map::new();
    match catch {
        None => {
            task.insert("End".to_string(), json!(true));
            states.insert("Invoke".to_string(), Value::Object(task));
        }
        Some((error, url)) => {
            task.insert(
                "Catch".to_string(),
                json!([{
                    "ErrorEquals": [error],
                    "ResultPath": "$.failure",
                    "Next": "Notify",
                }]),
            );
            task.insert("End".to_string(), json!(true));
            states.insert("Invoke".to_string(), Value::Object(task));
            states.insert(
                "Notify".to_string(),
                json!({
                    "Type": "Task",
                    "Resource": "arn:aws:states:::sqs:sendMessage",
                    "Parameters": { "QueueUrl": url, "MessageBody.$": "$.failure.Error" },
                    "End": true,
                }),
            );
        }
    }
    json!({ "StartAt": "Invoke", "States": Value::Object(states) })
}

/// Every outcome that is not a served invocation returning a result the state's bounds admit
/// fails the task under its own error name: a catcher naming that name takes it over and routes
/// it on, and an uncaught one ends the execution `FAILED` carrying it.
#[tokio::test]
async fn every_unservable_task_outcome_fails_the_task_under_its_own_error() {
    let case = "a Task state's function work that did not come back as a result";
    let router = emulator();

    let url = given_queue(&router, case, FAILURE_QUEUE).await;

    // The function's own execution role, which says nothing about any integration call.
    let function_role = given_role(
        &router,
        case,
        "xsvc-taskfail-function-role",
        "lambda.amazonaws.com",
        json!([function_log_writes()]),
    )
    .await;
    // The two state-machine roles: one permitted the invocation and the queue send a catcher's
    // handler state makes, one permitted the send alone.
    let permitted_role = given_role(
        &router,
        case,
        "xsvc-taskfail-machine-role",
        "states.amazonaws.com",
        json!([
            allow_all_of(&["lambda:InvokeFunction"]),
            allow_all_of(&["sqs:SendMessage"]),
            logs_only(),
        ]),
    )
    .await;
    let denied_role = given_role(
        &router,
        case,
        "xsvc-taskfail-denied-role",
        "states.amazonaws.com",
        json!([allow_all_of(&["sqs:SendMessage"]), logs_only()]),
    )
    .await;

    deployed_as(
        &router,
        case,
        WORKER_STACK,
        &json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                "Worker": function_resource(WORKER_FUNCTION, &function_role, WORKER_HANDLER),
            },
        }),
        &[("Worker", WORKER_FUNCTION)],
    )
    .await;
    let function_arn = active_function_arn(&router, case, WORKER_FUNCTION).await;

    for (index, row) in unservable_rows().iter().enumerate() {
        let role = if row.denied {
            &denied_role
        } else {
            &permitted_role
        };

        // The uncaught half: with no catcher, the task's outcome is the execution's.
        let machine = given_machine(
            &router,
            row.case,
            &format!("xsvc-taskfail-plain-{index}"),
            &task_definition(&function_arn, row.bound, None),
            role,
        )
        .await;
        let execution = started(
            &router,
            row.case,
            &machine,
            &format!("plain-{index}"),
            &row.input,
        )
        .await;
        let described = terminal(&router, row.case, &execution).await;
        assert_eq!(
            described["status"].as_str(),
            Some(FAILED),
            "case {:?}: expected the execution to end {FAILED}, got {described}",
            row.case
        );
        assert_eq!(
            described["error"].as_str(),
            Some(row.error),
            "case {:?}: expected the failure to carry {:?}, got {described}",
            row.case,
            row.error
        );
        assert!(
            described.get("output").is_none(),
            "case {:?}: an execution that failed puts out nothing, got {described}",
            row.case
        );
        // Where the service's own sentence IS the contract, the whole of it: the quota sentence
        // naming the task's resolved resource, and the refusal naming the published action.
        if row.error == DATA_LIMIT_ERROR {
            let expected = result_too_large(&function_arn);
            assert_eq!(
                described["cause"].as_str(),
                Some(expected.as_str()),
                "case {:?}: expected the cause {expected:?}, got {described}",
                row.case
            );
        }
        if row.error == REFUSED_INVOKE_ERROR {
            let reported = described["cause"].as_str().unwrap_or_default();
            for fragment in [REFUSED_INVOKE_TEXT, function_arn.as_str()] {
                assert!(
                    reported.contains(fragment),
                    "case {:?}: expected the cause to carry {fragment:?}, got {described}",
                    row.case
                );
            }
        }
        // Nothing of the uncaught half reached the queue: only a catcher's handler state sends.
        sends_nothing(&router, row.case, &url).await;

        if !row.caught {
            continue;
        }

        // The caught half: the catcher naming that error takes the failure over, so the
        // execution succeeds and the error name is on the queue where a client drains it.
        let machine = given_machine(
            &router,
            row.case,
            &format!("xsvc-taskfail-caught-{index}"),
            &task_definition(&function_arn, row.bound, Some((row.error, &url))),
            role,
        )
        .await;
        let execution = started(
            &router,
            row.case,
            &machine,
            &format!("caught-{index}"),
            &row.input,
        )
        .await;
        let described = terminal(&router, row.case, &execution).await;
        assert_eq!(
            described["status"].as_str(),
            Some(SUCCEEDED),
            "case {:?}: expected the catcher to take the failure over, got {described}",
            row.case
        );
        let notices = drained(&router, row.case, &url, 1).await;
        assert_eq!(
            notices,
            vec![row.error.to_string()],
            "case {:?}: expected the caught error name on the queue, got {notices:?}",
            row.case
        );
    }
}
