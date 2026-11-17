// SPDX-License-Identifier: CC0-1.0

//! What a SWF `ScheduleLambdaFunction` decision DOES, over the four crates that hold the
//! answer: SWF owns the workflow execution and its history, IAM holds the role the execution
//! attaches, CloudFormation archives the handler into a deployment package, and Lambda runs
//! the function and answers for the invocation.
//!
//! RULE: a Lambda task of a workflow execution that carries a `lambdaRole` is really invoked
//! — SWF records `LambdaFunctionStarted` naming the scheduling event, and then the one
//! terminal event the invocation produced: `LambdaFunctionCompleted` whose `result` is what
//! the function RETURNED, or `LambdaFunctionFailed` whose `reason`/`details` are what the
//! function (or Lambda) reported instead — and a decision task carries that outcome to the
//! decider, which is what lets the run carry on. An execution with NO role has none to
//! assume, so its task never starts: `StartLambdaFunctionFailed` with cause
//! `ASSUME_ROLE_FAILED`, and no start and no result ever.
//!
//! Only a cross-service suite can pin it: the history is SWF's, the function and its result
//! are Lambda's, and neither crate on its own sees the other side of the hand-over.
//!
//! The rows span what the rule generalises over: the input the task carries (and so the
//! result the handler computes from it), the form the function is named in (its bare name,
//! its full ARN), whether the handler returns or raises, whether the function exists at all,
//! and whether the execution carries a role. No expected result is written out here: each is
//! computed by the test from the input it sent.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// An access key id no record is held for: it resolves to the account's own identity, which
/// no identity-based document bounds, so the fixtures and the calls are made with it.
const ACCOUNT_KEY: &str = "EMULATORSETUPKEY0001";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `X-Amz-Target` prefix of SWF's `awsJson1_0` wire protocol.
const SWF_TARGET_PREFIX: &str = "SimpleWorkflowService";

/// The function every row's task names, the stack that deploys it, and the handler module
/// CloudFormation archives the inline source under.
const FUNCTION: &str = "swf-lam-sum";
const STACK: &str = "swf-lam-sum-stack";
const HANDLER: &str = "index.handler";

/// A function name no `CreateFunction` was made for, for the row whose task names a function
/// that does not exist.
const ABSENT_FUNCTION: &str = "swf-lam-absent";

/// The runtime the handler runs on.
const RUNTIME: &str = "python3.12";

/// The handler: it SUMS the numbers it is handed, and raises [`RAISED_ERROR`] when asked to,
/// so a row's expected result is computed from the row's own input and a row's expected
/// failure is the class this source raises.
const SOURCE: &str = "def handler(event, context):\n    \
     if event.get('fail'):\n        \
     raise ValueError('the handler was asked to fail')\n    \
     return {'sum': sum(event['numbers'])}\n";

/// The error class [`SOURCE`] raises, which is what a failed task's `reason` reports.
const RAISED_ERROR: &str = "ValueError";

/// The `State` a function serves invocations in, and the status its stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The timeout the function is created with. Nothing here tests a function's own timeout, so
/// it is given generous room: the run shares this process with every other test of this
/// target and must not be stopped by a limit this suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `startToCloseTimeout` every row's task carries: the LONGEST one SWF gives a Lambda
/// task ("any integer from 1-900"). Nothing here tests a task's own timeout, so it is given
/// all the room there is — this target runs thousands of tests beside this one, and a result
/// must not turn into a timeout because the host was busy. The timeout VALUES themselves are
/// exercised, at both published bounds, by the SWF crate's own suite.
const TASK_START_TO_CLOSE: &str = "900";

/// How long this suite looks for work the clock owes (a stack's completion, a function's
/// provisioning window, the invocation SWF makes off the response path) and how often. The
/// deadline bounds a hang; nothing here sleeps for a fixed span on the emulator's behalf.
const WAIT: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(50);

/// The domain and workflow type every run of this suite belongs to, and the task list its
/// decision tasks wait in.
const DOMAIN: &str = "swf-lam-domain";
const WORKFLOW_TYPE: (&str, &str) = ("swf-lam-wf", "1.0");
const DECIDER_LIST: &str = "swf-lam-deciders";

/// The terminal events a Lambda task can reach once it has started, as SWF's `EventType`
/// names them.
const COMPLETED: &str = "LambdaFunctionCompleted";
const FAILED: &str = "LambdaFunctionFailed";
const TIMED_OUT: &str = "LambdaFunctionTimedOut";
/// The event a task that never started records instead, and its one modelled cause.
const START_FAILED: &str = "StartLambdaFunctionFailed";
const ASSUME_ROLE_FAILED: &str = "ASSUME_ROLE_FAILED";

/// The emulator as it is ASSEMBLED, on one engine: SWF, Lambda, IAM and CloudFormation all
/// reach the same store.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`: the pair that routes the
/// request and names the principal it speaks for.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call that must be served, as its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// What one restJson1 or awsJson call answered.
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One Lambda `restJson1` call.
async fn lambda_call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split([':', '#'])
                    .next_back()
                    .unwrap_or(value)
                    .to_string()
            }),
        body: answered.to_vec(),
    }
}

/// One SWF `awsJson1_0` call, as its status and parsed body.
async fn swf_call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("swf.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{SWF_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("swf"))
        .body(Bytes::from(body.to_string().into_bytes()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, parsed)
}

/// One SWF call that must be served, as its parsed body.
async fn swf(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = swf_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> u32 {
    u32::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(u32::MAX)
}

/// Create the IAM role `name` trusting `service`, and answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    service: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": service},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    element(&created, "Arn")
}

/// Grant `role` the invoking action on `function_arn` through an inline policy — the wiring a
/// workflow execution's `lambdaRole` carries so SWF may invoke the function under it.
async fn granted_invoke(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    function_arn: &str,
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "lambda:InvokeFunction",
            "Resource": function_arn,
        }],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "invoke-the-task-function"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// Deploy [`FUNCTION`] as a stack whose inline source CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist — and answer its ARN once it is active.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Summer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": SOURCE},
                },
            },
        },
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;

    // The stack's completion and the function's own provisioning window are owed by the
    // clock, which no fixed sleep names: this looks until each holds.
    let mut last = String::new();
    for _ in 0..looks() {
        last = query(
            router,
            case,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        if element(&last, "StackStatus") == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&last, "StackStatus"),
        CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {last}"
    );

    let mut configuration = Value::Null;
    for _ in 0..looks() {
        configuration = lambda_call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an active function reports its ARN, got {configuration}")
                })
                .to_string();
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// What one row asks of the Lambda task, and what must become of it.
struct Row {
    case: &'static str,
    /// Whether the execution is started with the `lambdaRole`, which is what decides whether
    /// there is a role to assume at all.
    with_role: bool,
    /// Whether the task names the function by its ARN rather than by its bare name.
    by_arn: bool,
    /// Whether the task names a function that does not exist.
    absent: bool,
    /// The numbers the handler is handed, which is what it sums.
    numbers: Vec<i64>,
    /// Whether the input asks the handler to raise.
    fail: bool,
}

fn rows() -> Vec<Row> {
    let row = |case: &'static str, numbers: Vec<i64>| Row {
        case,
        with_role: true,
        by_arn: false,
        absent: false,
        numbers,
        fail: false,
    };
    vec![
        row("three numbers", vec![1, 2, 3]),
        row("one number", vec![7]),
        row("no numbers at all", vec![]),
        row("many larger numbers", vec![10, 20, 30, 40, 50]),
        Row {
            by_arn: true,
            ..row("the function named by its full ARN", vec![4, 5])
        },
        Row {
            fail: true,
            ..row("a handler that raises", vec![1])
        },
        Row {
            absent: true,
            ..row("a task naming a function that does not exist", vec![1])
        },
        Row {
            with_role: false,
            ..row("an execution with no lambdaRole", vec![1, 2, 3])
        },
    ]
}

/// The input one row's task carries: the event the handler is invoked with.
fn input_of(row: &Row) -> String {
    json!({ "numbers": row.numbers, "fail": row.fail }).to_string()
}

#[tokio::test]
async fn a_scheduled_lambda_task_runs_its_function_and_reports_what_it_returned() {
    let router = build_router();
    let case = "fixture";
    let execution_role = given_role(&router, case, "swf-lam-exec", "lambda.amazonaws.com").await;
    let function_arn = given_function(&router, case, &execution_role).await;
    // The role a workflow execution attaches: trusted by SWF, which assumes it to invoke the
    // task's function, and granted the invoking action on that function.
    let lambda_role = given_role(&router, case, "swf-lam-invoke", "swf.amazonaws.com").await;
    granted_invoke(&router, case, "swf-lam-invoke", &function_arn).await;

    swf(
        &router,
        case,
        "RegisterDomain",
        &json!({ "name": DOMAIN, "workflowExecutionRetentionPeriodInDays": "1" }),
    )
    .await;
    swf(
        &router,
        case,
        "RegisterWorkflowType",
        &json!({
            "domain": DOMAIN,
            "name": WORKFLOW_TYPE.0,
            "version": WORKFLOW_TYPE.1,
            "defaultTaskList": { "name": DECIDER_LIST },
            "defaultTaskStartToCloseTimeout": "60",
            "defaultExecutionStartToCloseTimeout": "600",
            "defaultChildPolicy": "TERMINATE",
        }),
    )
    .await;
    // What Lambda itself answers for an invocation of a function that does not exist: the row
    // whose task names one must report that same refusal rather than a code chosen here.
    let absent_refusal = lambda_call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{ABSENT_FUNCTION}/invocations"),
        Some(&json!({})),
    )
    .await;
    assert!(
        absent_refusal.status >= 400 && absent_refusal.error_type.is_some(),
        "case {case:?}: Lambda must refuse an invocation of a function it does not hold, got \
         {} with {}",
        absent_refusal.status,
        absent_refusal.text()
    );
    let absent_code = absent_refusal.error_type.unwrap_or_default();

    for (index, row) in rows().into_iter().enumerate() {
        let workflow_id = format!("swf-lam-run-{index}");
        let mut start = json!({
            "domain": DOMAIN,
            "workflowId": workflow_id,
            "workflowType": { "name": WORKFLOW_TYPE.0, "version": WORKFLOW_TYPE.1 },
        });
        if row.with_role {
            start["lambdaRole"] = json!(lambda_role);
        }
        let run_id = swf(&router, row.case, "StartWorkflowExecution", &start).await["runId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: the start must mint a runId", row.case))
            .to_string();

        let task = swf(
            &router,
            row.case,
            "PollForDecisionTask",
            &json!({ "domain": DOMAIN, "taskList": { "name": DECIDER_LIST } }),
        )
        .await;
        let token = task["taskToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: a decision task must be waiting", row.case))
            .to_string();

        let name = match (row.absent, row.by_arn) {
            (true, _) => ABSENT_FUNCTION.to_string(),
            (false, true) => function_arn.clone(),
            (false, false) => FUNCTION.to_string(),
        };
        let input = input_of(&row);
        swf(
            &router,
            row.case,
            "RespondDecisionTaskCompleted",
            &json!({
                "taskToken": token,
                "decisions": [{
                    "decisionType": "ScheduleLambdaFunction",
                    "scheduleLambdaFunctionDecisionAttributes": {
                        "id": format!("lam-{index}"),
                        "name": name,
                        "input": input,
                        "startToCloseTimeout": TASK_START_TO_CLOSE,
                    },
                }],
            }),
        )
        .await;

        let events = settled(&router, &row, &workflow_id, &run_id).await;
        let types: Vec<String> = events
            .iter()
            .map(|event| event_type(event).to_string())
            .collect();
        let scheduled = one_of(&row, &events, "LambdaFunctionScheduled");
        let scheduled_event_id = event_id(&scheduled);
        assert_eq!(
            scheduled["lambdaFunctionScheduledEventAttributes"]["input"]
                .as_str()
                .unwrap_or_default(),
            input,
            "case {:?}: the scheduling event carries the input the decision sent, got \
             {scheduled}",
            row.case
        );

        if !row.with_role {
            // The negative control: no role to assume, so nothing was invoked — there is no
            // start event and no terminal event, only the failure to start.
            let unstarted = one_of(&row, &events, START_FAILED);
            assert_eq!(
                (
                    unstarted["startLambdaFunctionFailedEventAttributes"]["cause"]
                        .as_str()
                        .unwrap_or_default(),
                    unstarted["startLambdaFunctionFailedEventAttributes"]["scheduledEventId"]
                        .as_i64()
                        .unwrap_or_default(),
                ),
                (ASSUME_ROLE_FAILED, scheduled_event_id),
                "case {:?}: a task with no role to assume fails to start with \
                 {ASSUME_ROLE_FAILED} naming its scheduling event, got {unstarted}",
                row.case
            );
            assert!(
                !types.iter().any(|event_type| {
                    event_type == "LambdaFunctionStarted"
                        || [COMPLETED, FAILED, TIMED_OUT].contains(&event_type.as_str())
                }),
                "case {:?}: nothing may be invoked for a task with no role to assume, got \
                 {types:?}",
                row.case
            );
            assert_decision_task_follows(&router, &row, &workflow_id, &run_id, START_FAILED).await;
            continue;
        }

        // The task STARTED: the invocation was made, and the start event names the scheduling
        // one.
        let started = one_of(&row, &events, "LambdaFunctionStarted");
        assert_eq!(
            started["lambdaFunctionStartedEventAttributes"]["scheduledEventId"]
                .as_i64()
                .unwrap_or_default(),
            scheduled_event_id,
            "case {:?}: the start event names its scheduling event, got {started}",
            row.case
        );
        let started_event_id = event_id(&started);
        assert!(
            !types.iter().any(|event_type| event_type == START_FAILED),
            "case {:?}: an execution WITH a lambdaRole has one to assume, so the task must not \
             fail to start, got {types:?}",
            row.case
        );

        let expected_terminal = if row.absent || row.fail { FAILED } else { COMPLETED };
        let terminal = one_of(&row, &events, expected_terminal);
        let attributes = &terminal[attributes_member(expected_terminal)];
        assert_eq!(
            (
                attributes["scheduledEventId"].as_i64().unwrap_or_default(),
                attributes["startedEventId"].as_i64().unwrap_or_default(),
            ),
            (scheduled_event_id, started_event_id),
            "case {:?}: the {expected_terminal} names both of the task's earlier events, got \
             {terminal}",
            row.case
        );

        if expected_terminal == COMPLETED {
            // The RESULT is the function's own return value, computed by the handler from the
            // input this row sent — so the expected value is computed here the same way.
            let result: Value = serde_json::from_str(
                attributes["result"].as_str().unwrap_or_else(|| {
                    panic!(
                        "case {:?}: a completed Lambda task carries its result, got {terminal}",
                        row.case
                    )
                }),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the result must be the function's own JSON, got {terminal}: \
                     {error}",
                    row.case
                )
            });
            assert_eq!(
                result,
                json!({ "sum": row.numbers.iter().sum::<i64>() }),
                "case {:?}: the result must be what the handler returned for {input}, got \
                 {terminal}",
                row.case
            );
        } else {
            let expected_reason = if row.absent {
                absent_code.clone()
            } else {
                RAISED_ERROR.to_string()
            };
            assert_eq!(
                attributes["reason"].as_str().unwrap_or_default(),
                expected_reason,
                "case {:?}: a failed Lambda task reports what the invocation answered, got \
                 {terminal}",
                row.case
            );
            assert!(
                !attributes["details"].as_str().unwrap_or_default().is_empty(),
                "case {:?}: a failed Lambda task carries the details of the failure, got \
                 {terminal}",
                row.case
            );
        }

        assert_decision_task_follows(&router, &row, &workflow_id, &run_id, expected_terminal)
            .await;
    }
}

/// The outcome event is carried to a decider: no Lambda task is left in flight, and the next
/// poll is handed a decision task whose history holds `outcome`.
async fn assert_decision_task_follows(
    router: &MultiServiceRouter,
    row: &Row,
    workflow_id: &str,
    run_id: &str,
    outcome: &str,
) {
    let described = swf(
        router,
        row.case,
        "DescribeWorkflowExecution",
        &json!({
            "domain": DOMAIN,
            "execution": { "workflowId": workflow_id, "runId": run_id },
        }),
    )
    .await;
    assert_eq!(
        (
            described["openCounts"]["openLambdaFunctions"]
                .as_i64()
                .unwrap_or_default(),
            described["executionInfo"]["executionStatus"]
                .as_str()
                .unwrap_or_default(),
        ),
        (0, "OPEN"),
        "case {:?}: the task is out of flight and the run carries on, got {described}",
        row.case
    );
    let next = swf(
        router,
        row.case,
        "PollForDecisionTask",
        &json!({ "domain": DOMAIN, "taskList": { "name": DECIDER_LIST } }),
    )
    .await;
    assert!(
        !next["taskToken"].as_str().unwrap_or_default().is_empty(),
        "case {:?}: the outcome must be carried to a decider by a decision task, got {next}",
        row.case
    );
    let carried: Vec<String> = next["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event_type(event).to_string())
        .collect();
    assert!(
        carried.iter().any(|event_type| event_type == outcome),
        "case {:?}: the decision task handed over must carry the {outcome} event, got \
         {carried:?}",
        row.case
    );
}

/// The run's history, looked at until the Lambda task has left the Scheduled state, or a
/// generous deadline passes.
///
/// The invocation runs off the response path, so looking again is what replaces a guessed
/// wait: the completion answers before the function has.
async fn settled(
    router: &MultiServiceRouter,
    row: &Row,
    workflow_id: &str,
    run_id: &str,
) -> Vec<Value> {
    let mut events = Vec::new();
    for _ in 0..looks() {
        events = swf(
            router,
            row.case,
            "GetWorkflowExecutionHistory",
            &json!({
                "domain": DOMAIN,
                "execution": { "workflowId": workflow_id, "runId": run_id },
            }),
        )
        .await["events"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if events.iter().any(|event| {
            [COMPLETED, FAILED, TIMED_OUT, START_FAILED].contains(&event_type(event))
        }) {
            return events;
        }
        tokio::time::sleep(LOOK).await;
    }
    let types: Vec<&str> = events.iter().map(event_type).collect();
    panic!(
        "case {:?}: the Lambda task must reach one of {:?}, last saw {types:?}",
        row.case,
        [COMPLETED, FAILED, TIMED_OUT, START_FAILED]
    );
}

/// The one event of `event_type` in `events`.
fn one_of(row: &Row, events: &[Value], event_type_name: &str) -> Value {
    let matching: Vec<&Value> = events
        .iter()
        .filter(|event| event_type(event) == event_type_name)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {:?}: expected exactly one {event_type_name} event, got {:?}",
        row.case,
        events.iter().map(event_type).collect::<Vec<_>>()
    );
    matching[0].clone()
}

/// The `eventType` of one history event.
fn event_type(event: &Value) -> &str {
    event["eventType"].as_str().unwrap_or_default()
}

/// The `eventId` of one history event.
fn event_id(event: &Value) -> i64 {
    event["eventId"].as_i64().unwrap_or_default()
}

/// An `eventType` as the matching `HistoryEvent` attributes member is spelled: the same name
/// with its first letter lowercased and `EventAttributes` appended, which is how the model
/// binds every one of them.
fn attributes_member(event_type_name: &str) -> String {
    let mut characters = event_type_name.chars();
    match characters.next() {
        Some(first) => {
            first.to_lowercase().collect::<String>() + characters.as_str() + "EventAttributes"
        }
        None => String::new(),
    }
}
