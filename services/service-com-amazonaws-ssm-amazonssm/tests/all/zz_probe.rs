// SPDX-License-Identifier: CC0-1.0

//! A PROBE of the whole Automation stop surface, in the ORDER a caller drives it: create the
//! runbook, start it, ask for a completion it cannot have, cancel it, and read the execution
//! back.
//!
//! `tests/all/automation_stop.rs` states each rule over a table of inputs; this suite is the
//! one case that drives the WHOLE sequence through the front door in order, with no clock
//! moved through the store at any point — so what it pins is that the sequence a caller
//! actually makes (a start followed straight away by a stop, with no wait in between)
//! answers the execution a stop leaves behind, every member of it at once.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for.
const REGION: &str = "us-west-2";

/// The runbook this suite stops: one step that waits far longer than the sequence takes, so
/// the automation is still standing at it when the stop arrives.
const A_LONG_WAIT: &str = "PT5M";

/// A router with just this service mounted.
fn service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this test is about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
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

/// A string member of a body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// The whole sequence, in order, with nothing waited for in between.
#[tokio::test]
async fn a_start_followed_by_a_stop_answers_the_execution_the_stop_left() {
    let router = service();

    let content = json!({
        "schemaVersion": "0.3",
        "description": "Waits.",
        "mainSteps": [{"name": "wait", "action": "aws:sleep", "inputs": {"Duration": A_LONG_WAIT}}],
    });
    let (status, created) = call(
        &router,
        "CreateDocument",
        &json!({
            "Name": "Stopped-Runbook",
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook must be created first, got {status} with {created}"
    );

    let (status, started) = call(
        &router,
        "StartAutomationExecution",
        &json!({"DocumentName": "Stopped-Runbook"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the start must be served, got {status} with {started}"
    );
    let id = text(&started, "AutomationExecutionId");

    // An automation running itself has nothing for a caller to complete.
    let (status, refused) = call(
        &router,
        "StopAutomationExecution",
        &json!({"AutomationExecutionId": &id, "Type": "Complete"}),
    )
    .await;
    assert_eq!(
        (status, text(&refused, "Message")),
        (
            400,
            format!(
                "Failed to complete automation {id}, automation is running in Auto mode. \
                 Completion request is only supported in Interactive mode."
            )
        ),
        "expected the completion to be refused for an automation running itself, got {status} \
         with {refused}"
    );

    // The cancellation it does admit.
    let (status, stopped) = call(
        &router,
        "StopAutomationExecution",
        &json!({"AutomationExecutionId": &id}),
    )
    .await;
    assert_eq!(
        (status, &stopped),
        (200, &json!({})),
        "expected an empty 200 for the cancellation, got {status} with {stopped}"
    );

    // The execution the stop left behind: cancelled, ended, with the step it was stopped at
    // reported as it stood and every collection member of an automation read on the wire.
    let (status, read) = call(
        &router,
        "GetAutomationExecution",
        &json!({"AutomationExecutionId": &id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the read must be served, got {status} with {read}"
    );
    let execution = read["AutomationExecution"].clone();
    assert_eq!(
        (
            text(&execution, "AutomationExecutionStatus"),
            &execution["Parameters"],
            &execution["Outputs"],
            &execution["Targets"],
            &execution["ResolvedTargets"],
        ),
        (
            "Cancelled".to_string(),
            &json!({}),
            &json!({}),
            &json!([]),
            &json!({"ParameterValues": [], "Truncated": false}),
        ),
        "expected the cancelled execution and every collection member of it, got {execution}"
    );
    assert!(
        !execution["ExecutionEndTime"].is_null(),
        "expected the end instant of the cancelled execution, got {execution}"
    );

    let steps = execution["StepExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let [stood_at] = steps.as_slice() else {
        panic!("expected the runbook's one step execution, got {execution}");
    };
    assert_eq!(
        (
            text(stood_at, "StepStatus"),
            &stood_at["Inputs"],
            &stood_at["Outputs"],
            &stood_at["OverriddenParameters"],
            &stood_at["IsStepBlocked"],
        ),
        (
            "Cancelled".to_string(),
            // A step's `Inputs` reports each resolved value as the JSON literal it is.
            &json!({"Duration": "\"PT5M\""}),
            &json!({}),
            &json!({}),
            &json!(false),
        ),
        "expected the step the automation was stopped at as it stood, got {stood_at}"
    );
    assert!(
        !stood_at["ExecutionStartTime"].is_null() && !stood_at["ExecutionEndTime"].is_null(),
        "expected the window the step was stopped in, got {stood_at}"
    );
}
