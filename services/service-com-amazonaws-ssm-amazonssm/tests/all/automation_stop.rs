// SPDX-License-Identifier: CC0-1.0

//! Stopping an Automation execution, and the members every Automation read carries.
//!
//! Two rules, both end to end through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"):
//!
//! 1. `StopAutomationExecution` is APPLIED to the execution it names: the execution and the
//!    step it stands at reach a cancelled terminal state with their own end instants, no
//!    step after that one is ever entered, and the reads report that — so the standard
//!    stop-then-wait-for-a-terminal-status pattern completes.
//! 2. An Automation read always carries the execution's parameter, output, target and
//!    resolved-target members and each step's own two parameter maps and blocked flag,
//!    empty where the execution has nothing for them; and a step's `Inputs` reports each
//!    fully-resolved value as the JSON literal it is.
//!
//! An automation is `Pending` until the service picks it up and a step that waits waits out
//! its own span, so the cases that look PAST either instant move the clock through the
//! service's own state ([`pick_up`], [`finish_waits`]) rather than sleeping for it: what
//! they assert is the stop and the projection, not how long a span is.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for unless a case says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, to pin that an execution is stoppable only through the Region it
/// started in.
const OTHER_REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// A well-formed `AutomationExecutionId` (the modelled length is exactly 36) no execution
/// was ever minted under.
const UNKNOWN_EXECUTION_ID: &str = "99999999-8888-7777-6666-555555555555";

/// The sentence Systems Manager refuses an execution-scoped Automation call it holds no
/// execution for with: FIXED text, with the id the request named nowhere in it.
const EXECUTION_NOT_FOUND: &str = "The input automation execution does not exist.";

/// The sentence it refuses a status update it cannot make with, as the modelled
/// `InvalidAutomationStatusUpdateException` words it.
const STATUS_UPDATE_NOT_VALID: &str = "The specified update status operation isn't valid.";

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case reads the store through and moves the clock with.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for `region`, returning `(status, body)`.
async fn call_in_region(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{region}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Call `operation` signed for [`REGION`].
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, operation, body).await
}

/// The wire error code of a failure body (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// Create an Automation runbook of `name` with `content`, asserting it succeeded.
async fn runbook(router: &MultiServiceRouter, name: &str, content: &Value) {
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook {name} must be created first, got {status} with {created:?}"
    );
}

/// Start an automation, asserting it succeeded, and answering the id it minted.
async fn started(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, response) = call(router, "StartAutomationExecution", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {response:?}"
    );
    text(&response, "AutomationExecutionId")
}

/// Read one execution back, asserting the read succeeded.
async fn read(router: &MultiServiceRouter, case: &str, execution_id: &str) -> Value {
    let (status, response) = call(
        router,
        "GetAutomationExecution",
        &json!({"AutomationExecutionId": execution_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetAutomationExecution, got {status} with \
         {response:?}"
    );
    response["AutomationExecution"].clone()
}

/// Stop one execution, answering `(status, body)`. `stop_type` is the `Type` the request
/// names, or `None` for a request that names none.
async fn stop(
    router: &MultiServiceRouter,
    execution_id: &str,
    stop_type: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({"AutomationExecutionId": execution_id});
    if let Some(stop_type) = stop_type {
        body["Type"] = json!(stop_type);
    }
    call(router, "StopAutomationExecution", &body).await
}

/// One step of an execution, by name.
fn step(execution: &Value, name: &str) -> Value {
    execution["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|step| text(step, "StepName") == name)
        .unwrap_or(Value::Null)
}

/// An instant well in the past, which is what the clock is moved to below.
fn long_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(3_600)
}

/// Let the clock reach the pickup every execution of [`REGION`] is waiting for, without
/// touching the waits its steps are in.
///
/// The service owes a started automation ONE transition, a span after the start
/// (`automation::PICKUP_DELAY`); a case that asserts what the stop does to a RUNNING
/// execution moves the clock to it through the store rather than sleeping for it.
fn pick_up(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held == REGION && execution.runnable_at.is_some() {
            execution.runnable_at = Some(long_ago());
        }
    }
}

/// Let the clock reach the end of every wait a step of [`REGION`]'s executions is in.
fn finish_waits(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    for ((held, _), execution) in store.automation_executions.iter_mut() {
        if held != REGION {
            continue;
        }
        for step in &mut execution.steps {
            if step.due_at.is_some() {
                step.due_at = Some(long_ago());
            }
        }
    }
}

/// A runbook whose FIRST step is `action` with `inputs` and whose second step would run
/// after it, so a stop at the first step is visible as the second step never being entered.
fn two_step_runbook(action: &str, inputs: Value) -> Value {
    json!({
        "schemaVersion": "0.3",
        "description": "Stands at one step.",
        "mainSteps": [
            {"name": "stand", "action": action, "inputs": inputs},
            {"name": "after", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}},
        ],
    })
}

/// A wait no case of this suite lets the clock reach, so the execution stands at the step
/// for as long as the case needs — the step is left running until [`finish_waits`] moves
/// the clock to the end of its span.
const A_LONG_WAIT: &str = "PT5M";

/// Create a runbook standing at `action` and start it in `mode` — the `ExecutionMode` the
/// start names, or none for the default — answering the id it minted. NOTHING has picked
/// the execution up yet.
async fn started_standing_at(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    inputs: Value,
    mode: Option<&str>,
) -> String {
    // A document `Name` admits `[a-zA-Z0-9_\-.]` only
    // (`smithy_query(query="operation",operation="CreateDocument")`), so the case names the
    // runbook through that set.
    let name = format!(
        "Stop-{}-{}",
        action.replace(':', "-"),
        case.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    );
    runbook(router, &name, &two_step_runbook(action, inputs)).await;
    let mut start = json!({"DocumentName": name});
    if let Some(mode) = mode {
        start["Mode"] = json!(mode);
    }
    started(router, case, &start).await
}

/// The same, with the pickup the clock owes materialised, so the execution is standing at
/// the step rather than waiting to be picked up at all.
async fn standing_at_in_mode(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    action: &str,
    inputs: Value,
    mode: Option<&str>,
) -> String {
    let id = started_standing_at(router, case, action, inputs, mode).await;
    pick_up(engine);
    // The read is what materialises the pickup the clock now owes, so the execution is
    // standing at its first step before the stop arrives.
    let execution = read(router, case, &id).await;
    let standing = text(&execution, "AutomationExecutionStatus");
    assert!(
        ["InProgress", "Waiting"].contains(&standing.as_str()),
        "case {case:?}: expected the execution to be running before the stop, got \
         {standing:?} in {execution}"
    );
    id
}

/// The same, in the mode a start that names none runs in.
async fn standing_at(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    action: &str,
    inputs: Value,
) -> String {
    standing_at_in_mode(router, engine, case, action, inputs, None).await
}

/// RULE: a CANCELLING stop is APPLIED to the execution it names — the execution reaches the
/// cancelled terminal status with its own end instant, the step it was standing at is ended
/// `Cancelled` with it and with its own end instant, and no step after that one is ever
/// entered.
///
/// The rows differ along whether the request names the type at all, along the step the
/// automation stands at (one the clock would have ended by itself, and two only a caller's
/// signal would ever have carried on) and along whether the service had picked the execution
/// up before the stop arrived at all. What the OTHER stop type does is the next suite's.
#[tokio::test]
async fn a_stop_ends_the_execution_and_the_step_it_stands_at() {
    let (router, engine) = service();

    // (case, the `Type` the request names, the action the automation stands at, its inputs,
    // whether the service had picked the execution up)
    let rows: Vec<(&str, Option<&str>, &str, Value, bool)> = vec![
        (
            "no type named, so the default",
            None,
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            true,
        ),
        (
            "Cancel at a step the clock would end",
            Some("Cancel"),
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            true,
        ),
        (
            "Cancel at a step waiting for a signal",
            Some("Cancel"),
            "aws:pause",
            json!({}),
            true,
        ),
        (
            "Cancel at a step waiting for an approval",
            Some("Cancel"),
            "aws:approve",
            json!({"Approvers": ["arn:aws:iam::000000000000:user/emulator"]}),
            true,
        ),
        (
            "a stop that reaches the execution before the service picked it up",
            None,
            "aws:sleep",
            json!({"Duration": A_LONG_WAIT}),
            false,
        ),
    ];

    for (case, stop_type, action, inputs, picked_up) in rows {
        let id = if picked_up {
            standing_at(&router, &engine, case, action, inputs).await
        } else {
            started_standing_at(&router, case, action, inputs, None).await
        };

        let (status, answered) = stop(&router, &id, stop_type).await;
        assert_eq!(
            (status, &answered),
            (200, &json!({})),
            "case {case:?}: expected an empty 200 for a stop of a running automation, got \
             {status} with {answered}"
        );

        // The stop is APPLIED: the status a poller waits for is terminal on the very next
        // read, and stays so however many times it is read.
        for round in ["the first read after the stop", "a later read"] {
            let execution = read(&router, case, &id).await;
            let stood_at = step(&execution, "stand");
            assert_eq!(
                (
                    text(&execution, "AutomationExecutionStatus"),
                    text(&stood_at, "StepStatus"),
                    text(&step(&execution, "after"), "StepStatus"),
                ),
                (
                    "Cancelled".to_string(),
                    "Cancelled".to_string(),
                    "Pending".to_string(),
                ),
                "case {case:?} / {round:?}: expected the execution and the step it stood at \
                 cancelled with nothing after them entered, got {execution}"
            );
            assert!(
                !execution["ExecutionEndTime"].is_null()
                    && !stood_at["ExecutionEndTime"].is_null(),
                "case {case:?} / {round:?}: expected an end instant on the execution and on \
                 the step it stood at, got {execution}"
            );
        }
    }
}

/// RULE: the two stop types differ in the OUTCOME the automation is left with — a stop that
/// asks for it to be COMPLETED ends it as completed rather than cancelled — and a
/// completion is a request only an `Interactive` automation admits, because only such an
/// automation is waiting for its caller to run the rest of its steps. A completion of an
/// automation running in any other mode is the modelled invalid status update, worded with
/// the execution it named and the mode that automation is running in.
#[tokio::test]
async fn a_completion_is_only_supported_for_an_interactive_automation() {
    let (router, engine) = service();

    // An automation running itself has nothing for a caller to complete.
    let case = "a completion of an automation running itself";
    let running = standing_at(
        &router,
        &engine,
        case,
        "aws:sleep",
        json!({"Duration": A_LONG_WAIT}),
    )
    .await;
    let (status, refused) = stop(&router, &running, Some("Complete")).await;
    assert_eq!(
        (
            status,
            error_code(&refused).as_str(),
            error_message(&refused).as_str()
        ),
        (
            400,
            "InvalidAutomationStatusUpdateException",
            format!(
                "Failed to complete automation {running}, automation is running in Auto mode. \
                 Completion request is only supported in Interactive mode."
            )
            .as_str()
        ),
        "case {case:?}: expected the modelled refusal naming the execution and its mode, got \
         {status} with {refused}"
    );
    assert_eq!(
        text(
            &read(&router, case, &running).await,
            "AutomationExecutionStatus"
        ),
        "InProgress",
        "case {case:?}: a refused completion must leave the automation running"
    );

    // An INTERACTIVE automation waits for its caller before each step, so completing it is
    // saying the rest will not be run: the automation ends completed, with the step it was
    // waiting at never run and nothing after it entered.
    let case = "a completion of an interactive automation";
    let interactive = standing_at_in_mode(
        &router,
        &engine,
        case,
        "aws:sleep",
        json!({"Duration": A_LONG_WAIT}),
        Some("Interactive"),
    )
    .await;
    let (status, answered) = stop(&router, &interactive, Some("Complete")).await;
    assert_eq!(
        (status, &answered),
        (200, &json!({})),
        "case {case:?}: expected an empty 200, got {status} with {answered}"
    );
    let completed = read(&router, case, &interactive).await;
    assert_eq!(
        (
            text(&completed, "AutomationExecutionStatus"),
            text(&step(&completed, "stand"), "StepStatus"),
            text(&step(&completed, "after"), "StepStatus"),
        ),
        (
            "Success".to_string(),
            "Cancelled".to_string(),
            "Pending".to_string(),
        ),
        "case {case:?}: expected the automation completed with the step it waited at ended \
         and nothing after it entered, got {completed}"
    );
    assert!(
        !completed["ExecutionEndTime"].is_null(),
        "case {case:?}: expected the end instant of the completed automation, got {completed}"
    );

    // And the clock reaching the end of a wait does not revive what has ended.
    finish_waits(&engine);
    assert_eq!(
        text(
            &read(&router, case, &interactive).await,
            "AutomationExecutionStatus"
        ),
        "Success",
        "case {case:?}: a completed automation stays completed"
    );
}

/// RULE: a stop of an execution this endpoint is not running names nothing to stop — an id
/// no execution was minted under and the endpoint of another Region are the modelled miss,
/// and an execution that has already reached a terminal status is the modelled invalid
/// status update. Each refusal carries the fixed sentence AWS words its class with, and
/// changes nothing.
#[tokio::test]
async fn a_stop_of_an_execution_that_is_running_nothing_is_refused() {
    let (router, engine) = service();

    // An execution that has already ended, each terminal status reached a different way:
    // one stopped, and one that ran its steps out.
    let stopped = standing_at(
        &router,
        &engine,
        "an execution already stopped",
        "aws:sleep",
        json!({"Duration": A_LONG_WAIT}),
    )
    .await;
    let (status, _) = stop(&router, &stopped, None).await;
    assert_eq!(status, 200, "the first stop must be applied");

    runbook(
        &router,
        "Stop-Succeeding",
        &json!({
            "schemaVersion": "0.3",
            "mainSteps": [{"name": "only", "action": "aws:sleep", "inputs": {"Duration": "PT0S"}}],
        }),
    )
    .await;
    let succeeded = started(
        &router,
        "an execution that succeeded",
        &json!({"DocumentName": "Stop-Succeeding"}),
    )
    .await;
    pick_up(&engine);
    assert_eq!(
        text(
            &read(&router, "an execution that succeeded", &succeeded).await,
            "AutomationExecutionStatus"
        ),
        "Success",
        "the execution whose only step ran out must have succeeded first"
    );

    // (case, the id to stop, the Region to stop it through, the code and message expected)
    let rows: Vec<(&str, &str, &str, &str, &str)> = vec![
        (
            "an id nothing minted",
            UNKNOWN_EXECUTION_ID,
            REGION,
            "AutomationExecutionNotFoundException",
            EXECUTION_NOT_FOUND,
        ),
        (
            "another Region's endpoint",
            stopped.as_str(),
            OTHER_REGION,
            "AutomationExecutionNotFoundException",
            EXECUTION_NOT_FOUND,
        ),
        (
            "an execution already stopped",
            stopped.as_str(),
            REGION,
            "InvalidAutomationStatusUpdateException",
            STATUS_UPDATE_NOT_VALID,
        ),
        (
            "an execution that succeeded",
            succeeded.as_str(),
            REGION,
            "InvalidAutomationStatusUpdateException",
            STATUS_UPDATE_NOT_VALID,
        ),
    ];

    for (case, execution_id, region, expected_code, expected_message) in rows {
        let (status, refused) = call_in_region(
            &router,
            region,
            "StopAutomationExecution",
            &json!({"AutomationExecutionId": execution_id}),
        )
        .await;
        let message = error_message(&refused);
        assert_eq!(
            (status, error_code(&refused).as_str(), message.as_str()),
            (400, expected_code, expected_message),
            "case {case:?}: expected the modelled refusal and its fixed sentence, got \
             {status} with {refused}"
        );
        assert!(
            !message.contains(execution_id),
            "case {case:?}: the fixed sentence must not repeat the id the request sent, got \
             {message:?}"
        );
    }

    // The statuses the refusals were measured against are untouched.
    for (case, id, expected) in [
        ("an execution already stopped", &stopped, "Cancelled"),
        ("an execution that succeeded", &succeeded, "Success"),
    ] {
        assert_eq!(
            text(
                &read(&router, case, id).await,
                "AutomationExecutionStatus"
            ),
            expected,
            "case {case:?}: a refused stop must change nothing"
        );
    }
}

/// RULE: every Automation read carries the execution's parameter, output, target and
/// resolved-target members and each step's own two parameter maps and blocked flag — empty
/// where the execution has nothing for them — so a client that indexes any of them finds it
/// on every read.
///
/// The rows differ along what the start supplied: nothing at all, parameters, and a
/// rate-controlled start whose targets resolve to values.
#[tokio::test]
async fn every_automation_read_carries_the_collection_members() {
    let (router, engine) = service();
    runbook(
        &router,
        "Projecting",
        &json!({
            "schemaVersion": "0.3",
            "parameters": {"Path": {"type": "String", "default": "left"}},
            "mainSteps": [
                {"name": "stand", "action": "aws:sleep", "inputs": {"Duration": A_LONG_WAIT}},
            ],
        }),
    )
    .await;

    // (case, the start, the Parameters / Targets / ResolvedTargets the read must carry)
    let rows: Vec<(&str, Value, Value, Value, Value)> = vec![
        (
            "a start that supplied nothing",
            json!({"DocumentName": "Projecting"}),
            json!({}),
            json!([]),
            json!({"ParameterValues": [], "Truncated": false}),
        ),
        (
            "a start that supplied parameters",
            json!({"DocumentName": "Projecting", "Parameters": {"Path": ["right"]}}),
            json!({"Path": ["right"]}),
            json!([]),
            json!({"ParameterValues": [], "Truncated": false}),
        ),
        (
            "a rate-controlled start whose targets resolve",
            json!({
                "DocumentName": "Projecting",
                "TargetParameterName": "Path",
                "Targets": [{"Key": "ParameterValues", "Values": ["left", "right"]}],
            }),
            json!({}),
            json!([{"Key": "ParameterValues", "Values": ["left", "right"]}]),
            json!({"ParameterValues": ["left", "right"], "Truncated": false}),
        ),
    ];

    for (case, start, parameters, targets, resolved_targets) in rows {
        let id = started(&router, case, &start).await;
        pick_up(&engine);
        let execution = read(&router, case, &id).await;
        assert_eq!(
            (
                &execution["Parameters"],
                &execution["Outputs"],
                &execution["Targets"],
                &execution["ResolvedTargets"],
            ),
            (&parameters, &json!({}), &targets, &resolved_targets),
            "case {case:?}: expected the always-carried members of an execution, got \
             {execution}"
        );
    }
}

/// RULE: every step execution a read reports carries its own two parameter maps and its
/// blocked flag, and reports each fully-resolved input as the JSON literal it is — a string
/// inside its own quotes, and every other kind as the JSON it was declared as. Both reads
/// that project a step execution report it the same way.
#[tokio::test]
async fn every_step_execution_carries_its_defaults_and_json_inputs() {
    let (router, engine) = service();
    // One input per kind of value a step's `inputs` can declare, so the rendering rule is
    // pinned over the whole set rather than over the one string the recorded step carried.
    // `aws:sleep` reads its own `Duration` and nothing else, so the rest are carried
    // through to the report untouched.
    let declared = json!({
        "Duration": A_LONG_WAIT,
        "Referenced": "{{ Path }}",
        "Count": 5,
        "Flag": true,
        "Names": ["a", "b"],
        "Mapping": {"k": "v"},
    });
    runbook(
        &router,
        "Projecting-Inputs",
        &json!({
            "schemaVersion": "0.3",
            "parameters": {"Path": {"type": "String", "default": "left"}},
            "mainSteps": [{"name": "stand", "action": "aws:sleep", "inputs": declared}],
        }),
    )
    .await;
    let case = "a step standing at its wait";
    let id = started(
        &router,
        case,
        &json!({"DocumentName": "Projecting-Inputs", "Parameters": {"Path": ["right"]}}),
    )
    .await;
    pick_up(&engine);

    // The step as each of the two reads that project one reports it.
    let through_the_get = step(&read(&router, case, &id).await, "stand");
    let (status, report) = call(
        &router,
        "DescribeAutomationStepExecutions",
        &json!({"AutomationExecutionId": &id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAutomationStepExecutions, got {status} \
         with {report}"
    );
    let through_the_report = report["StepExecutions"]
        .as_array()
        .and_then(|steps| steps.first())
        .cloned()
        .unwrap_or(Value::Null);

    for (reader, reported) in [
        ("GetAutomationExecution", &through_the_get),
        ("DescribeAutomationStepExecutions", &through_the_report),
    ] {
        assert_eq!(
            (
                &reported["Outputs"],
                &reported["OverriddenParameters"],
                &reported["IsStepBlocked"],
            ),
            (&json!({}), &json!({}), &json!(false)),
            "case {reader:?}: expected the always-carried members of a step execution, got \
             {reported}"
        );
        // (the input, the JSON literal its resolved value renders as)
        let expected: Vec<(&str, &str)> = vec![
            ("Duration", r#""PT5M""#),
            // The reference resolves to the value the start supplied, still a string.
            ("Referenced", r#""right""#),
            ("Count", "5"),
            ("Flag", "true"),
            ("Names", r#"["a","b"]"#),
            ("Mapping", r#"{"k":"v"}"#),
        ];
        for (input, literal) in expected {
            assert_eq!(
                reported["Inputs"][input],
                json!(literal),
                "case {reader:?} / {input:?}: expected the resolved value as the JSON \
                 literal {literal:?}, got {reported}"
            );
        }
    }
}
