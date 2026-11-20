// SPDX-License-Identifier: CC0-1.0

//! The Lambda callback worker that HEARTBEATS, on the ASSEMBLED emulator: a
//! `.waitForTaskToken` task whose function reports progress inside the window its state
//! declared, and the identical wiring whose function reports none.
//!
//! RULE: "this action resets the Heartbeat clock" (`SendTaskHeartbeat`), and
//! "HeartbeatSeconds […] if more time than the specified seconds elapses between
//! heartbeats from the task, this state fails with a States.Timeout error name" — so the
//! member bounds the IDLE span between two reports of progress and not the span the work
//! takes. A worker that heartbeats inside the window therefore drives its execution to
//! `SUCCEEDED` however long its whole run takes, with no `TaskTimedOut` in its history,
//! while the same wiring with no heartbeat fails with `States.Timeout` one window after
//! the hand-over.
//!
//! Five crates have to agree for that to hold — IAM holds the roles, CloudFormation
//! archives the worker's handler into a deployment package, AWS Lambda runs it, STS mints
//! the session its SDK calls are signed with, and Step Functions interprets the state,
//! hands the token to that invocation and decides what the execution ends as — so the
//! suite lives here. `service-com-amazonaws-sfn-awsstepfunctions`'s own
//! `callback_heartbeat_window` suite is what spans the class (the number of heartbeats,
//! the window's length, the work after the last of them, the whole-duration bound beside
//! it); what only this suite adds is the pair run end to end on the emulator as it is
//! ASSEMBLED, over a function deployed from a stack.
//!
//! The two rows differ in the one thing the rule turns on: whether the worker reports
//! progress inside the window at all. Both do the same amount of work, which is more than
//! one window's worth.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the function, the
/// machines and the tokens belong to.
const REGION: &str = "us-east-1";

/// The optimized Lambda invocation with the callback pattern: the `Task` hands the
/// function the token out of the context object and waits until it is redeemed.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The worker both rows invoke, and the stack that deploys it; the template declares the
/// function under the logical id [`LOGICAL_ID`].
const FUNCTION: &str = "xsvc-heartbeat-window-worker";
/// See [`FUNCTION`].
const STACK: &str = "xsvc-heartbeat-window-worker-stack";
/// See [`FUNCTION`].
const LOGICAL_ID: &str = "Worker";

/// The role the worker runs as, and the role the state machines are executed as.
const WORKER_ROLE: &str = "xsvc-heartbeat-window-worker-role";
/// See [`WORKER_ROLE`].
const MACHINE_ROLE: &str = "xsvc-heartbeat-window-machine-role";

/// The `StackStatus` the worker's stack must reach, and the `State` Lambda reports once a
/// function "is ready to be invoked" (`FunctionConfiguration$State`).
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const FUNCTION_ACTIVE: &str = "Active";

/// The statuses an execution of these machines is read in (`ExecutionStatus`).
const RUNNING: &str = "RUNNING";
/// See [`RUNNING`].
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`RUNNING`].
const FAILED: &str = "FAILED";

/// The ASL predefined error name a bound a `Task` state declared reports when it runs out.
const TIMEOUT_ERROR: &str = "States.Timeout";

/// The history event a service-integration task closed by such a bound is recorded with
/// (`HistoryEventType`).
const EVENT_TASK_TIMED_OUT: &str = "TaskTimedOut";

/// The window both rows' `Task` states declare as `HeartbeatSeconds`, and the two spans
/// the worker works: one heartbeat inside the window, and work on either side of it adding
/// up to more than one window.
const HEARTBEAT_SECONDS: u64 = 5;
/// See [`HEARTBEAT_SECONDS`].
const WORKS_BEFORE_SECONDS: f64 = 3.0;
/// See [`HEARTBEAT_SECONDS`].
const WORKS_AFTER_SECONDS: f64 = 3.0;

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a row
/// works, because what is under test is which bound closes the task and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 900;

/// How long to keep looking for work the clock owes — a stack reaching
/// [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an execution leaving
/// [`RUNNING`] — and how often to look again. Every loop stops as soon as its condition
/// holds, and the deadline is generous so a loaded host cannot decide a row's outcome.
const PATIENCE: Duration = Duration::from_secs(180);
/// See [`PATIENCE`].
const LOOK: Duration = Duration::from_millis(50);

/// The document the worker settles its task with once it has done its work.
fn settlement_output() -> Value {
    json!({ "approved": true, "by": "the worker holding the token" })
}

/// The worker both rows invoke: it is handed a task token, works, reports progress on that
/// token where its event asks it to, works again and then settles the task.
///
/// Whatever this service refuses it CATCHES and reports instead of raising, so the row
/// whose task a lapsed window closed still runs its handler to the end.
const WORKER_HANDLER: &str = r#"
import json
import time

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    states = boto3.client("stepfunctions")
    token = event["token"]
    beat = None
    time.sleep(event["before"])
    if event["heartbeats"]:
        try:
            states.send_task_heartbeat(taskToken=token)
            beat = "answered"
        except ClientError as refused:
            beat = refused.response["Error"]["Code"]
    time.sleep(event["after"])
    try:
        states.send_task_success(
            taskToken=token,
            output=json.dumps(event["output"]),
        )
        return {"settled": True, "beat": beat}
    except ClientError as refused:
        return {
            "settled": False,
            "beat": beat,
            "code": refused.response["Error"]["Code"],
        }
"#;

/// One row: whether the worker reports progress inside the window, and what the execution
/// must therefore end as.
struct Row {
    case: &'static str,
    heartbeats: bool,
    /// The terminal status this service must report for the execution.
    status: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a worker that heartbeats inside the window and works on past it",
        heartbeats: true,
        status: SUCCEEDED,
    },
    Row {
        case: "the identical wiring whose worker never heartbeats",
        heartbeats: false,
        status: FAILED,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: the worker's own SDK calls are
/// dispatched back into this same router, so the task they heartbeat is the one this suite
/// parked.
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

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
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
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Step Functions operation over its `awsJson1_0` wire, made as the account's own
/// credential, asserting the call itself was served.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
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
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must succeed, got {status} with {parsed}"
    );
    parsed
}

/// The trust policy a Lambda execution role needs.
fn lambda_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The trust policy the machines' own role carries, so this service may assume it.
fn machine_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// What the worker's role allows: the log actions every execution role needs, and the
/// reporting and settling of the task whose token it is handed.
fn worker_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
                "states:SendTaskHeartbeat",
                "states:SendTaskSuccess",
            ],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// What the machines' role allows: the invocation a callback `Task` hands its token over
/// with.
fn machine_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["lambda:InvokeFunction"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create a role named `name` trusting `trust`, and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, trust: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    })
}

/// Put the inline document `document` on the role `name`.
async fn put_role_policy(router: &MultiServiceRouter, name: &str, document: &str) {
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "heartbeat-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
}

/// The one-state definition a row's machine runs: a callback `Task` that hands the worker
/// the token out of the context object, the spans it works and whether it heartbeats, under
/// the window the rule is about.
fn definition(row: &Row, function: &str) -> String {
    json!({
        "StartAt": "AwaitWorker",
        "States": {
            "AwaitWorker": {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "FunctionName": function,
                    "Payload": {
                        "token.$": "$$.Task.Token",
                        "before": WORKS_BEFORE_SECONDS,
                        "after": WORKS_AFTER_SECONDS,
                        "heartbeats": row.heartbeats,
                        "output": settlement_output(),
                    },
                },
                "HeartbeatSeconds": HEARTBEAT_SECONDS,
                "End": true,
            },
        },
    })
    .to_string()
}

/// Create one state machine and answer its ARN.
async fn create_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &str,
    role_arn: &str,
) -> String {
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({ "name": name, "definition": definition, "roleArn": role_arn }),
    )
    .await;
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateStateMachine({name:?}) must answer an ARN, got {created}")
        })
        .to_string()
}

/// Deploy the worker's stack, wait for [`CREATE_COMPLETE`] and answer the function's
/// physical id.
///
/// The inline source is archived into a deployment package by CloudFormation, which is how
/// a function whose handler is written out in a suite comes to exist at all.
async fn deploy_worker(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Worker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": WORKER_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports the function created,
    // which no fixed sleep can name: this looks until it reports it.
    let giving_up_at = Instant::now() + PATIENCE;
    let mut reported = stack_status(router, STACK).await;
    while reported.0 != CREATE_COMPLETE && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, STACK).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
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
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    let physical = physical_ids_of(&resources);
    let function = physical.get(LOGICAL_ID).cloned().unwrap_or_else(|| {
        panic!("setup: the stack must report a physical id for {LOGICAL_ID}, got {physical:?}")
    });
    assert_eq!(
        function, FUNCTION,
        "setup: the stack must name the function {FUNCTION:?}, got {function:?}"
    );
    function
}

/// Wait until Lambda reports `name` [`FUNCTION_ACTIVE`], which is the state an invocation
/// needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("/2015-03-31/functions/{name}/configuration"))
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("lambda"))
            .body(Bytes::new())
            .expect("a well-formed request");
        let response = router.dispatch(request).await;
        let configuration: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} {FUNCTION_ACTIVE}, last reported {last}");
}

/// `execution` once this service reports it in a terminal status, or the last reading taken
/// before the deadline.
///
/// What a callback task's bounds decide is owed from the instants the clock makes them due,
/// which no fixed sleep can name: this looks for the CONDITION — a status that is no longer
/// [`RUNNING`] — and stops at the look where it holds.
async fn awaited_terminal(router: &MultiServiceRouter, case: &str, execution: &str) -> Value {
    let giving_up_at = Instant::now() + PATIENCE;
    loop {
        let reported = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
        if reported["status"].as_str() != Some(RUNNING) || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// The types of the events `execution`'s history carries, in order.
async fn history_types(router: &MultiServiceRouter, case: &str, execution: &str) -> Vec<String> {
    let history = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution }),
    )
    .await;
    history["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["type"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Both rows: on the assembled emulator, the worker that reports progress inside the window
/// settles its task and the one that reports none is closed by the window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heartbeating_lambda_worker_settles_its_callback_task() {
    let router = build_router();

    let worker_role = create_role(&router, WORKER_ROLE, &lambda_trust_policy()).await;
    put_role_policy(&router, WORKER_ROLE, &worker_document()).await;
    let machine_role = create_role(&router, MACHINE_ROLE, &machine_trust_policy()).await;
    put_role_policy(&router, MACHINE_ROLE, &machine_document()).await;
    let function = deploy_worker(&router, &worker_role).await;
    wait_until_active(&router, &function).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let machine = create_machine(
            &router,
            case,
            &format!("xsvc-heartbeat-window-{index}"),
            &definition(row, &function),
            &machine_role,
        )
        .await;
        let started = sfn(
            &router,
            case,
            "StartExecution",
            &json!({
                "stateMachineArn": machine,
                "name": "worker-run",
                "input": "{}",
            }),
        )
        .await;
        let execution = started["executionArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartExecution must answer an ARN, got {started}")
            })
            .to_string();

        let settled = awaited_terminal(&router, case, &execution).await;
        assert_eq!(
            settled["status"].as_str(),
            Some(row.status),
            "case {case:?}: expected {}, got {settled}",
            row.status
        );
        let types = history_types(&router, case, &execution).await;
        let timed_out = types.iter().any(|event| event == EVENT_TASK_TIMED_OUT);
        if row.status == SUCCEEDED {
            let carried: Value = settled["output"]
                .as_str()
                .map(|text| {
                    serde_json::from_str(text)
                        .unwrap_or_else(|why| panic!("case {case:?}: the output is JSON: {why}"))
                })
                .unwrap_or_else(|| panic!("case {case:?}: expected an output in {settled}"));
            assert_eq!(
                carried,
                settlement_output(),
                "case {case:?}: expected the document the worker settled with, got {carried}"
            );
            assert!(
                !timed_out,
                "case {case:?}: no {EVENT_TASK_TIMED_OUT} may be recorded for a task whose window \
                 was reset in time, got {types:?}"
            );
        } else {
            assert_eq!(
                settled["error"].as_str(),
                Some(TIMEOUT_ERROR),
                "case {case:?}: expected {TIMEOUT_ERROR}, got {settled}"
            );
            assert!(
                timed_out,
                "case {case:?}: expected a {EVENT_TASK_TIMED_OUT} event for the closed task, got \
                 {types:?}"
            );
        }
    }
}
