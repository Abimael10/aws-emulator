// SPDX-License-Identifier: CC0-1.0

//! A managed workflow's CUSTOM step reporting its outcome from INSIDE the function it
//! invoked, with the whole emulator assembled.
//!
//! RULE: the AWS SDK a managed Lambda runtime puts on a handler's import path reaches every
//! service this emulator serves whose requests the runtime's call seam binds — the request is
//! built from that service's own model, signed as the function's execution role and dispatched
//! in process — so what the handler reads back is that service's own answer, and a token the
//! seam binds no requests for is refused as an unknown service before any request is built.
//! Transfer Family is one such service: a custom workflow step hands its Lambda `Target` an
//! `ExecutionId`, a `WorkflowId` and a `Token`, and "you must include those with their callback
//! as well as providing a status" (`SendWorkflowStepState`), so a step target that cannot reach
//! the service can never report the outcome its execution is waiting for.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the service the
//! handler calls (a create, a read of what it created, a listing, and the callback itself),
//! which `CustomStepStatus` the callback reports (`SUCCESS` and `FAILURE` — the enum's two
//! values), and whether what the callback names resolves (a live token, a replay of one already
//! consumed, an execution the workflow is not running, a workflow this account does not hold).
//! Each row asserts the answer the SERVICE gave: its status, its error code and its message.
//! What the calls left behind is then read back through that service's own API, so a callback
//! that answered without recording anything fails here.
//!
//! Two crates have to agree for any of this to hold — Lambda runs the handler and supplies the
//! SDK, Transfer Family owns the workflow run the callback resolves against — so the suite
//! lives in this crate: neither crate's own test target can see both halves.
//!
//! The run a token belongs to is STARTED by a file landing on a server whose
//! `WorkflowDetails.OnUpload` names the workflow, over the SFTP connector plane the transfer
//! crate's own suites drive; this suite PLANTS the record that writer produces — with the
//! service's own minted execution id and callback token, through the state the handlers read —
//! because what is under test here is the callback leg that crosses from a running function
//! into the service.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::InMemoryEngine;
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_transfer_transferservice::state as transfer_state;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The stack that deploys the step target, the execution role it runs as, and the function
/// itself.
const STACK: &str = "xsvc-transfer-custom-step";
const EXEC_ROLE: &str = "xsvc-transfer-custom-step-exec";
const FUNCTION: &str = "xsvc-transfer-custom-step-target";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is which calls a handler can make and not how fast an
/// interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack completing, a function
/// reaching `Active` — and how many looks to take. Every span waited for is a service's own, so
/// nothing here sleeps for a fixed one, and the deadline is generous so a loaded host cannot
/// decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// `CustomStepStatus`'s two values (`smithy_query(query="shape",shape="CustomStepStatus")` ->
/// `[FAILURE, SUCCESS]`).
const SUCCESS: &str = "SUCCESS";
const FAILURE: &str = "FAILURE";

/// The `ExecutionStatus` values a finished run reports: a custom step that reported `SUCCESS`
/// leaves nothing to wait for, and one that reported `FAILURE` is the "exception encountered"
/// (`smithy_query(query="shape",shape="ExecutionStatus")` ->
/// `[IN_PROGRESS, COMPLETED, EXCEPTION, HANDLING_EXCEPTION]`).
const COMPLETED: &str = "COMPLETED";
const EXCEPTION: &str = "EXCEPTION";
const IN_PROGRESS: &str = "IN_PROGRESS";

/// The error codes this suite asserts, each the one its operation models
/// (`smithy_query(query="operation",operation="SendWorkflowStepState")`).
const ACCESS_DENIED: &str = "AccessDeniedException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The fixed text the service answers a callback that resolves to no running execution with —
/// the whole message, with the identifier travelling in the error's `Resource` member rather
/// than interpolated into it.
const UNKNOWN_EXECUTION: &str = "Unknown execution";

/// A `WorkflowId` satisfying the member's modelled `len 19..19`, pattern
/// `^w-([a-z0-9]{17})$`, that no `CreateWorkflow` of this suite minted.
const ABSENT_WORKFLOW: &str = "w-00000000000000000";

/// An `ExecutionId` satisfying the member's modelled `len 36..36` UUID pattern that no run of
/// this suite's workflow answers to.
const ABSENT_EXECUTION: &str = "ffffffff-0000-4000-8000-ffffffffffff";

/// The handler every row runs: `import boto3` at MODULE scope and the client built there too —
/// so a runtime that supplied no Transfer Family client would fail the initialization phase
/// rather than one call — then each call the event names, reporting exactly what the SDK gave
/// it: the answer document and its HTTP status, or the service's own code, message and status.
const TARGET_HANDLER: &str = r#"
import boto3
import botocore.exceptions

CLIENT = boto3.client("transfer")


def lambda_handler(event, context):
    made = []
    for call in event["calls"]:
        method = getattr(CLIENT, call["method"])
        try:
            answered = method(**call.get("parameters", {}))
        except botocore.exceptions.ClientError as refused:
            made.append({
                "ok": False,
                "code": refused.response["Error"]["Code"],
                "message": refused.response["Error"].get("Message"),
                "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            })
        except Exception as raised:
            made.append({"ok": False, "raised": type(raised).__name__, "detail": str(raised)})
        else:
            status = answered.pop("ResponseMetadata", {}).get("HTTPStatusCode")
            made.append({"ok": True, "status": status, "answered": answered})
    return {"made": made}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the execution it reports on is the one this suite planted.
fn build_emulator() -> (Arc<InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    (engine, router)
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and where each handler reads its Region from.
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
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::copy_from_slice(body))
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

/// One awsJson1_1 call to Transfer Family, as the account's own credential: the way this suite
/// reads back what a handler's callback left behind.
async fn transfer(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("transfer.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TransferService.{operation}"))
        .header("authorization", authorization("transfer"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parsed)
}

/// The template the suite deploys: the execution role, granted the callback action a step target
/// makes, and the target function. The inline source is archived into the deployment package by
/// CloudFormation, which is how a function whose handler is written here comes to exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ExecRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": EXEC_ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "custom-step-grants",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "transfer:SendWorkflowStepState",
                                        "transfer:CreateWorkflow",
                                        "transfer:DescribeWorkflow",
                                        "transfer:ListWorkflows",
                                    ],
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "Target": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": TARGET_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the stack and wait for `CREATE_COMPLETE`, answering the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let mut reported = String::new();
    let mut described = String::new();
    for _ in 0..LOOKS {
        let (status, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if reported == CREATE_COMPLETE || !reported.ends_with("_IN_PROGRESS") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    let failures = if reported == CREATE_COMPLETE {
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
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
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
    physical_ids_of(&resources)
}

/// Wait until Lambda reports the target function `Active` — the state an invocation needs — and
/// answer the `FunctionArn` that service published for it, which is what the custom step names
/// as its `Target`.
async fn active_function_arn(router: &MultiServiceRouter) -> String {
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
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}")
}

/// One pending custom step of a started run of `workflow_id`, as the upload writer records it:
/// the service's own minted `ExecutionId` and `Token`, with the step's outcome not yet reported.
fn plant_pending_execution(engine: &Arc<InMemoryEngine>, workflow_id: &str) -> (String, String) {
    let state = transfer_state::service_state(&**engine)
        .expect("the transfer service state is reachable on this engine");
    let mut guard = state.lock();
    let execution_id = guard.mint_execution_id();
    let token = guard.mint_callback_token();
    let mut step_states = BTreeMap::new();
    step_states.insert(token.clone(), None);
    guard.workflow_executions.insert(
        (workflow_id.to_string(), execution_id.clone()),
        transfer_state::WorkflowExecution {
            workflow_id: workflow_id.to_string(),
            execution_id: execution_id.clone(),
            initial_file_location: None,
            service_metadata: None,
            step_states,
        },
    );
    (execution_id, token)
}

/// Invoke the step target with `calls` and answer what it reported for each, in order.
async fn invoke(router: &MultiServiceRouter, case: &str, calls: &Value) -> Vec<Value> {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        json!({"calls": calls}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let raised = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        raised, None,
        "case {case:?}: the handler must run to completion, got {raised:?} with {payload}"
    );
    payload["made"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected one report per call, got {payload}"))
}

/// The `Status` Transfer Family reports for one run of `workflow_id`, read through that
/// service's own listing.
async fn listed_status(
    router: &MultiServiceRouter,
    workflow_id: &str,
    execution_id: &str,
) -> (u16, Value) {
    let (status, body) = transfer(
        router,
        "ListExecutions",
        &json!({"WorkflowId": workflow_id}),
    )
    .await;
    let reported = body["Executions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|execution| execution["ExecutionId"].as_str() == Some(execution_id))
        .map(|execution| execution["Status"].clone())
        .unwrap_or(Value::Null);
    (status, reported)
}

/// One callback the handler makes, as the event names it.
fn callback(workflow_id: &str, execution_id: &str, token: &str, status: &str) -> Value {
    json!({
        "method": "send_workflow_step_state",
        "parameters": {
            "WorkflowId": workflow_id,
            "ExecutionId": execution_id,
            "Token": token,
            "Status": status,
        },
    })
}

/// RULE: a step target reaches Transfer Family through the SDK its runtime supplies, so the
/// callback it makes is dispatched into that service and answered by it — the first callback for
/// a token a started execution still holds as pending is accepted with an empty body and records
/// the reported status on that step, a replay of it resolves to no running execution, and a read
/// of the run answers alike before and after that refused replay.
#[tokio::test]
async fn a_custom_step_target_reports_its_outcome_through_the_runtimes_transfer_client() {
    let (engine, router) = build_emulator();
    let physical = deploy(&router).await;
    let reported = physical.get("Target").cloned().unwrap_or_default();
    assert_eq!(
        reported, FUNCTION,
        "setup: the stack must report the step target as {FUNCTION:?}, got {reported:?}"
    );
    let target_arn = active_function_arn(&router).await;
    assert!(
        target_arn.starts_with("arn:aws:lambda:"),
        "setup: Lambda must publish an ARN for {FUNCTION:?}, got {target_arn:?}"
    );

    // The workflow whose CUSTOM step names that function, created through the service's own API.
    let (status, created) = transfer(
        &router,
        "CreateWorkflow",
        &json!({
            "Description": "the step whose target reports its own outcome",
            "Steps": [{
                "Type": "CUSTOM",
                "CustomStepDetails": {
                    "Name": "pause",
                    "Target": &target_arn,
                    "TimeoutSeconds": 300,
                    "SourceFileLocation": "${original.file}",
                },
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateWorkflow must be accepted, got {status} with {created}"
    );
    let workflow_id = created["WorkflowId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !workflow_id.is_empty(),
        "setup: CreateWorkflow must answer a WorkflowId, got {created}"
    );

    // Two started runs of it, each holding one custom step pending on its own token: one row
    // reports `SUCCESS` and the other `FAILURE`, the two values the status member models.
    let (succeeding, success_token) = plant_pending_execution(&engine, &workflow_id);
    let (failing, failure_token) = plant_pending_execution(&engine, &workflow_id);

    // Before any callback, both runs are in progress — which is what makes the rows below about
    // a token the execution is still holding as pending.
    for (case, execution_id) in [
        ("the run whose step will report SUCCESS", &succeeding),
        ("the run whose step will report FAILURE", &failing),
    ] {
        let (status, reported) = listed_status(&router, &workflow_id, execution_id).await;
        assert_eq!(
            (status, reported.clone()),
            (200, json!(IN_PROGRESS)),
            "case {case:?}: expected the planted run to be listed {IN_PROGRESS}, got {status} \
             with {reported}"
        );
    }

    // ---- One invocation, one call per row: what a handler's own Transfer Family client reaches.
    //
    // The rows differ along the operation called and along what the callback names; the two live
    // callbacks come last, so every refusal above them is judged against a run still pending.
    let handler_made_workflow = "the handler's own CreateWorkflow";
    let calls = json!([
        {
            "method": "create_workflow",
            "parameters": {
                "Description": handler_made_workflow,
                "Steps": [{"Type": "DELETE", "DeleteStepDetails": {"Name": "drop"}}],
            },
        },
        {
            "method": "describe_workflow",
            "parameters": {"WorkflowId": &workflow_id},
        },
        {"method": "list_workflows", "parameters": {}},
        callback(ABSENT_WORKFLOW, &succeeding, &success_token, SUCCESS),
        callback(&workflow_id, ABSENT_EXECUTION, &success_token, SUCCESS),
        callback(&workflow_id, &succeeding, &failure_token, SUCCESS),
        callback(&workflow_id, &succeeding, &success_token, SUCCESS),
        callback(&workflow_id, &failing, &failure_token, FAILURE),
    ]);
    let made = invoke(&router, "the step target's own calls", &calls).await;
    assert_eq!(
        made.len(),
        8,
        "case \"the step target's own calls\": expected one report per call, got {made:?}"
    );

    // A create the handler made, answered by the service that owns it.
    let case = "a create the handler made";
    assert_eq!(
        made[0]["ok"],
        json!(true),
        "case {case:?}: expected the call to be answered, got {}",
        made[0]
    );
    let minted = made[0]["answered"]["WorkflowId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !minted.is_empty() && minted != workflow_id,
        "case {case:?}: expected a workflow of its own, got {}",
        made[0]
    );

    // A read of the workflow this suite created, with the step's own `Target` in it.
    let case = "a read of the workflow whose step invoked it";
    assert_eq!(
        made[1]["ok"],
        json!(true),
        "case {case:?}: expected the call to be answered, got {}",
        made[1]
    );
    let read_target = made[1]["answered"]["Workflow"]["Steps"][0]["CustomStepDetails"]["Target"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        read_target, target_arn,
        "case {case:?}: expected the step to name the function the service invoked, got {}",
        made[1]
    );

    // A listing, which enumerates both the suite's workflow and the handler's own.
    let case = "a listing of the account's workflows";
    assert_eq!(
        made[2]["ok"],
        json!(true),
        "case {case:?}: expected the call to be answered, got {}",
        made[2]
    );
    let listed: Vec<&str> = made[2]["answered"]["Workflows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["WorkflowId"].as_str())
        .collect();
    for wanted in [&workflow_id, &minted] {
        assert!(
            listed.contains(&wanted.as_str()),
            "case {case:?}: expected the listing to enumerate {wanted:?}, got {listed:?}"
        );
    }

    // A callback naming a workflow this account does not hold: the resource a permission for the
    // action names does not resolve, so the service refuses it as unauthorized.
    let case = "a callback naming a workflow the account does not hold";
    assert_eq!(
        (
            made[3]["ok"].clone(),
            made[3]["code"].clone(),
            made[3]["status"].clone()
        ),
        (json!(false), json!(ACCESS_DENIED), json!(403)),
        "case {case:?}: expected the service's own authorization refusal, got {}",
        made[3]
    );

    // A callback naming an execution the resolved workflow is not running.
    let case = "a callback naming an execution the workflow is not running";
    assert_eq!(
        (
            made[4]["ok"].clone(),
            made[4]["code"].clone(),
            made[4]["status"].clone(),
            made[4]["message"].clone()
        ),
        (
            json!(false),
            json!(NOT_FOUND),
            json!(transfer_state::RESOURCE_NOT_FOUND_HTTP_STATUS),
            json!(UNKNOWN_EXECUTION)
        ),
        "case {case:?}: expected the service's not-found with its fixed text, got {}",
        made[4]
    );

    // A token that addresses no pending step OF THIS execution: it is the other run's token, so
    // the callback is malformed rather than resolving to a step of the execution it named.
    let case = "a callback carrying another run's token";
    assert_eq!(
        made[5]["ok"],
        json!(false),
        "case {case:?}: expected the callback to be refused, got {}",
        made[5]
    );
    assert_eq!(
        made[5]["status"],
        json!(400),
        "case {case:?}: expected the client fault the service classes it as, got {}",
        made[5]
    );

    // The FIRST callback for a token the execution is still holding as pending: accepted, with
    // the empty body the operation's output models.
    for (case, at, status) in [
        ("the first callback reporting SUCCESS", 6, SUCCESS),
        ("the first callback reporting FAILURE", 7, FAILURE),
    ] {
        assert_eq!(
            (made[at]["ok"].clone(), made[at]["status"].clone()),
            (json!(true), json!(200)),
            "case {case:?}: expected the callback reporting {status} to be accepted, got {}",
            made[at]
        );
        assert_eq!(
            made[at]["answered"],
            json!({}),
            "case {case:?}: expected an empty answer document, got {}",
            made[at]
        );
    }

    // What the callbacks RECORDED, read back through the service's own listing: the status each
    // step reported decides the run's own.
    for (case, execution_id, expected) in [
        ("the run whose step reported SUCCESS", &succeeding, COMPLETED),
        ("the run whose step reported FAILURE", &failing, EXCEPTION),
    ] {
        let (status, reported) = listed_status(&router, &workflow_id, execution_id).await;
        assert_eq!(
            (status, reported.clone()),
            (200, json!(expected)),
            "case {case:?}: expected the run to be listed {expected}, got {status} with \
             {reported}"
        );
    }

    // ---- The REPLAY of a consumed token, and what a reader sees across it.
    let case = "a replay of an already reported token";
    let described_before = transfer(
        &router,
        "DescribeExecution",
        &json!({"WorkflowId": &workflow_id, "ExecutionId": &succeeding}),
    )
    .await;
    let replayed = invoke(
        &router,
        case,
        &json!([callback(&workflow_id, &succeeding, &success_token, SUCCESS)]),
    )
    .await;
    assert_eq!(
        (
            replayed[0]["ok"].clone(),
            replayed[0]["code"].clone(),
            replayed[0]["status"].clone(),
            replayed[0]["message"].clone()
        ),
        (
            json!(false),
            json!(NOT_FOUND),
            json!(transfer_state::RESOURCE_NOT_FOUND_HTTP_STATUS),
            json!(UNKNOWN_EXECUTION)
        ),
        "case {case:?}: expected the service's not-found with its fixed text, got {}",
        replayed[0]
    );
    let described_after = transfer(
        &router,
        "DescribeExecution",
        &json!({"WorkflowId": &workflow_id, "ExecutionId": &succeeding}),
    )
    .await;
    assert_eq!(
        described_after, described_before,
        "case {case:?}: expected DescribeExecution to answer alike across the refused replay, \
         got {described_after:?} after {described_before:?}"
    );
    let (status, reported) = listed_status(&router, &workflow_id, &succeeding).await;
    assert_eq!(
        (status, reported.clone()),
        (200, json!(COMPLETED)),
        "case {case:?}: expected the recorded outcome to stand, got {status} with {reported}"
    );

    // The workflow the handler created is still the one the service holds, read where a client
    // reads it: a call that answered without doing the work fails here.
    let (status, described) = transfer(
        &router,
        "DescribeWorkflow",
        &json!({"WorkflowId": &minted}),
    )
    .await;
    assert_eq!(
        status, 200,
        "after the rows: the workflow the handler created must be readable, got {status} with \
         {described}"
    );
    assert_eq!(
        described["Workflow"]["Description"],
        json!(handler_made_workflow),
        "after the rows: expected the description the handler's create carried, got {described}"
    );
    // And the stack that deployed the target still holds both resources the callback stood on:
    // the role the function runs as and the function itself.
    let (status, resources) = query(
        &router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    let held = physical_ids_of(&resources);
    assert_eq!(
        (
            status,
            held.get("ExecRole").cloned(),
            held.get("Target").cloned()
        ),
        (
            200,
            Some(EXEC_ROLE.to_string()),
            Some(FUNCTION.to_string())
        ),
        "after the rows: expected the stack to still hold its resources, got {resources}"
    );
}
