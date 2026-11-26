// SPDX-License-Identifier: CC0-1.0

//! What a FAILED `arn:aws:states:::apigateway:invoke` task tells the definition: the cause
//! it carries, driven across Step Functions (which runs the state and makes the request),
//! IAM and STS (which hold the execution role) and API Gateway (whose `execute-api` data
//! plane serves the deployed stage), mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a `Task` state whose unit of work is one request on a deployed api stage fails,
//! for an answer outside 2xx, with the BODY that answer carried as its cause and nothing
//! around it — the status, the reason phrase and the headers of the answer are what the
//! task's RESULT reports for a served call, and the error name already carries the status —
//! so a `Catch` path parses the document the method's own backend produced straight out of
//! `$.Cause`.
//!
//! The rows differ along what the rule generalises over: the status the stage answers (two
//! client errors, a conflict, a server error and an unavailable), and the FORM the answer's
//! body takes — a JSON document, a document with a nested member, a JSON array, a sentence
//! that is no document at all, and no body whatsoever.
//!
//! Which `ApiEndpoint`, stage, verb and authorization a task addresses, and the one answer
//! whose cause is Step Functions' own sentence rather than the endpoint's body (an
//! authorization refusal), are `crate::sfn_api_gateway_invoke_task`'s question; this suite
//! asks only what a non-2xx answer REACHES the definition as. Its harness — the four
//! services on one engine and the API Gateway control-plane caller — is that suite's, since
//! `tests/all/main.rs` is one crate root.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_api_gateway_invoke_task::{apigw, emulator, invoke_host, member, mock_method};
use crate::sfn_task_target_authorization::{
    logs_only, machine, ran_with_input, role, sfn, string, task_failure,
};

/// The `Resource` every state machine below names.
const INVOKE_RESOURCE: &str = "arn:aws:states:::apigateway:invoke";

/// The stage every api below is deployed to.
const STAGE: &str = "live";

/// The path whose method answers a 2xx, and the document that method's success branch
/// renders — the request's own context, which is what the shared stand-up configures
/// (`crate::sfn_api_gateway_invoke_task`'s `mock_method`). That answer's FACTS are reported
/// around the body in the task's result, which is what the cause of a failed call must not
/// carry.
const SERVED_PATH: &str = "served";

/// The members a served call's RESULT reports an answer by, none of which belongs in the
/// cause of a failed one: `StatusCode`, `StatusText`, `Headers` and `ResponseBody`, the
/// four the optimized integration names an endpoint's answer under
/// (`crate::sfn_api_gateway_invoke_task`'s served rows assert the result carries them).
const RESULT_MEMBERS: &[&str] = &["StatusCode", "StatusText", "Headers", "ResponseBody"];

/// One row: the path whose `MOCK` method answers `status` with `body`, and the case it
/// stands for. `body` is `None` for a method that answers no body at all.
struct Row {
    case: &'static str,
    path: &'static str,
    status: u16,
    body: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a client error whose body is a document with a nested member",
        path: "rejected",
        status: 400,
        body: Some(CAUGHT_BODY),
    },
    Row {
        case: "a not-found whose body is a flat document",
        path: "absent",
        status: 404,
        body: Some(r#"{"fault":"no-such-order"}"#),
    },
    Row {
        case: "a conflict whose body is a JSON array",
        path: "conflicting",
        status: 409,
        body: Some(r#"[{"fault":"already-placed"}]"#),
    },
    Row {
        case: "a server error whose body is a sentence rather than a document",
        path: "broken",
        status: 500,
        body: Some("the order service is broken"),
    },
    Row {
        case: "an unavailable answer carrying no body at all",
        path: "unavailable",
        status: 503,
        body: None,
    },
];

/// The document the 400 method renders, which the `Catch` row parses a member out of.
const CAUGHT_BODY: &str = r#"{"fault":"bad-request","detail":{"field":"quantity"}}"#;

/// The error name the 400 answer gives the task.
const CAUGHT_ERROR: &str = "ApiGateway.400";

/// An api carrying one method per row plus the served one, deployed to [`STAGE`]. Answers
/// the api's id.
///
/// Each method is stood up through the ONE stand-up of a `MOCK` method that answers a given
/// status (`crate::sfn_api_gateway_invoke_task`'s `mock_method`), so what a row's stage
/// answers is configured exactly as the sibling suite's is — a `GET` whose integration
/// response for that status renders the row's own body, or carries no template at all, which
/// is a `MOCK` answering no body.
async fn deployed_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = apigw(
        router,
        name,
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    let api = member(name, &created, "id");
    let root = member(name, &created, "rootResourceId");
    for row in ROWS {
        mock_method(
            router,
            &api,
            &root,
            row.path,
            &["GET"],
            row.status,
            row.body,
        )
        .await;
    }
    mock_method(router, &api, &root, SERVED_PATH, &["GET"], 200, None).await;
    apigw(
        router,
        name,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// The parameters a task addressing `path` of the stage carries.
fn parameters(host: &str, path: &str) -> Value {
    json!({
        "ApiEndpoint": host,
        "Method": "GET",
        "Stage": STAGE,
        "Path": path,
    })
}

/// The role every state machine below runs as: the log writes a state machine makes, and
/// nothing about any api, since the methods are open.
async fn invoking_role(router: &MultiServiceRouter, name: &str) -> String {
    role(
        router,
        name,
        &json!({ "Version": "2012-10-17", "Statement": [logs_only()] }),
    )
    .await
}

/// The cause an execution carries, as the text a definition reads — empty where the
/// execution carries none, which is what an answer with no body leaves.
fn carried_cause(described: &Value) -> String {
    described
        .get("cause")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: a non-2xx answer fails the task with the body that answer carried as its whole
/// cause, under the error name its status gives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_invoke_carries_the_answered_body_alone_as_its_cause() {
    let router = emulator();
    let api = deployed_api(&router, "sfn-apigw-cause").await;
    let host = invoke_host(&api);
    let role_arn = invoking_role(&router, "ApiGatewayCauseRole").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let machine_arn = machine(
            &router,
            &format!("sfn-apigw-cause-{index}"),
            INVOKE_RESOURCE,
            &parameters(&host, row.path),
            &role_arn,
        )
        .await;
        let (execution_arn, described) = ran_with_input(&router, &machine_arn, None, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the answer outside 2xx to fail the task, got {described:?}"
        );
        let expected_error = format!("ApiGateway.{}", row.status);
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(expected_error.as_str()),
            "case {case:?}: expected the error name {expected_error:?}, got {described:?}"
        );
        let expected_cause = row.body.unwrap_or_default();
        let carried = carried_cause(&described);
        assert_eq!(
            carried, expected_cause,
            "case {case:?}: expected the cause {expected_cause:?}, got {carried:?}"
        );
        for named in RESULT_MEMBERS {
            assert!(
                !carried.contains(named),
                "case {case:?}: expected the result member {named:?} to stay out of the cause, \
                 got {carried:?}"
            );
        }
        // The same name and cause on the task's own event, which is what a `Catch` is
        // matched against and what a reader of the history sees.
        let details = task_failure(&router, &execution_arn, case).await;
        assert_eq!(
            details.get("error").and_then(Value::as_str),
            Some(expected_error.as_str()),
            "case {case:?}: expected the task event to name the same error, got {details:?}"
        );
        let on_event = details
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            on_event, expected_cause,
            "case {case:?}: expected the task event to carry the same cause, got {on_event:?}"
        );
    }
}

/// RULE, read from the definition's own side: a `Catch` on the error name holds the
/// backend's document in `$.Cause`, so a path into that document resolves — while the
/// members an answer is REPORTED by are what a served call's result carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_catch_parses_the_backend_document_out_of_the_cause() {
    let router = emulator();
    let api = deployed_api(&router, "sfn-apigw-caught-cause").await;
    let host = invoke_host(&api);
    let role_arn = invoking_role(&router, "ApiGatewayCaughtCauseRole").await;
    let caught_row = ROWS
        .iter()
        .find(|row| row.path == "rejected")
        .expect("the row whose method answers the caught status");

    let definition = json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": INVOKE_RESOURCE,
                "Parameters": parameters(&host, caught_row.path),
                "Catch": [{
                    "ErrorEquals": [CAUGHT_ERROR],
                    "Next": "Caught",
                    "ResultPath": "$.caught",
                }],
                "End": true,
            },
            "Caught": { "Type": "Pass", "End": true },
        },
    })
    .to_string();
    let created = sfn(
        &router,
        "CreateStateMachine",
        &json!({
            "name": "sfn-apigw-caught-cause",
            "definition": definition,
            "roleArn": role_arn,
        }),
    )
    .await;
    let machine_arn = string(&created, "stateMachineArn");
    let case = "a Catch that parses the cause";
    let (_execution, described) = ran_with_input(&router, &machine_arn, Some("{}"), case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the Catch to route the failure, got {described:?}"
    );
    let output = string(&described, "output");
    let caught: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the caught failure, got {output:?} ({error})")
    });
    assert_eq!(
        caught["caught"]["Error"].as_str(),
        Some(CAUGHT_ERROR),
        "case {case:?}: expected the error the Catch matched, got {caught:?}"
    );
    let cause = caught["caught"]["Cause"].as_str().unwrap_or_default();
    assert_eq!(
        cause, CAUGHT_BODY,
        "case {case:?}: expected the backend's own document as the cause, got {cause:?}"
    );
    let document: Value = serde_json::from_str(cause).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a cause a Catch path can parse, got {cause:?} ({error})")
    });
    assert_eq!(
        document["detail"]["field"].as_str(),
        Some("quantity"),
        "case {case:?}: expected a path into the backend's document to resolve, got {document:?}"
    );

    // The members an answer is reported BY are the result of a served call, which is what
    // the cause above carries none of.
    let served = machine(
        &router,
        "sfn-apigw-served-result",
        INVOKE_RESOURCE,
        &parameters(&host, SERVED_PATH),
        &role_arn,
    )
    .await;
    let served_case = "the same api's method that answers a 2xx";
    let (_arn, answered) = ran_with_input(&router, &served, None, served_case).await;
    assert_eq!(
        answered.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {served_case:?}: expected the served call to succeed, got {answered:?}"
    );
    let result: Value = serde_json::from_str(&string(&answered, "output")).unwrap_or_else(|error| {
        panic!("case {served_case:?}: expected the invoke's result ({error})")
    });
    assert_eq!(
        result.get("StatusCode").and_then(Value::as_u64),
        Some(200),
        "case {served_case:?}: expected the status the stage answered, got {result:?}"
    );
    assert_eq!(
        result.get("StatusText").and_then(Value::as_str),
        Some("OK"),
        "case {served_case:?}: expected the reason phrase of that status, got {result:?}"
    );
    // The document that method's success branch renders is the request's own context, so the
    // body in the result is one only a call that really reached the stage can carry.
    let expected_body = json!({ "method": "GET", "stage": STAGE });
    assert_eq!(
        result.get("ResponseBody"),
        Some(&expected_body),
        "case {served_case:?}: expected the document the method rendered, got {result:?}"
    );
    let carried = carried_cause(&answered);
    assert!(
        carried.is_empty(),
        "case {served_case:?}: expected a served call to carry no cause, got {carried:?}"
    );
}
