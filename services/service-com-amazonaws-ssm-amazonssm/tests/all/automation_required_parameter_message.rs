// SPDX-License-Identifier: CC0-1.0

//! How Automation words a required document parameter a start left out.
//!
//! RULE: a start whose `Parameters` leave a parameter the runbook declares without a value —
//! a parameter the document declares no default for — is refused
//! `InvalidAutomationExecutionParametersException` with the Automation plane's own sentence,
//! "Missing required parameter: <that parameter> in user inputs.": fixed text around the
//! name of the parameter that was omitted, which is read off the runbook's own declaration
//! rather than off the request.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body — and every runbook a row
//! starts was created through `CreateDocument` in the same suite, so the parameter the
//! sentence names comes from persisted content.
//!
//! The table's rows differ along what the rule generalises over: which of the runbook's
//! parameters is omitted, how many it declares, how many carry defaults, and the spelling of
//! the omitted name — rather than the single runbook a recorded unit happened to start.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The error class Automation refuses a parameter fault with
/// (`smithy_query(query="operation", operation="StartAutomationExecution")`:
/// `InvalidAutomationExecutionParametersException`, HTTP 400).
const PARAMETER_REFUSAL: &str = "InvalidAutomationExecutionParametersException";

/// How that refusal is worded for a parameter the start left out: fixed text around the
/// name of the one parameter that was omitted.
fn missing_required(parameter: &str) -> String {
    format!("Missing required parameter: {parameter} in user inputs.")
}

/// Create an Automation runbook of `name` declaring `parameters`, with one step that does
/// nothing in particular, asserting the create succeeded.
async fn runbook(router: &MultiServiceRouter, name: &str, parameters: Value) {
    let content = json!({
        "schemaVersion": "0.3",
        "parameters": parameters,
        "mainSteps": [{
            "name": "only",
            "action": "aws:sleep",
            "inputs": { "Duration": "PT0S" },
        }],
    });
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
        "the runbook {name} must be created first, got {status} with {created}"
    );
}

/// One row: the runbook's declared parameters, what the start supplies, and the parameter
/// the refusal must name.
struct Row {
    case: &'static str,
    name: &'static str,
    parameters: Value,
    supplied: Value,
    /// The parameter the sentence names, or `None` where the start supplies every one the
    /// runbook requires and must therefore be served.
    missing: Option<&'static str>,
}

/// The rows: one required parameter, two of them, a required one beside a defaulted one,
/// and a start that leaves nothing out.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the only required parameter, left out",
            name: "OneRequired",
            parameters: json!({ "InstanceId": { "type": "String" } }),
            supplied: json!({}),
            missing: Some("InstanceId"),
        },
        Row {
            case: "the first of two required parameters, left out",
            name: "TwoRequired",
            parameters: json!({
                "TargetName": { "type": "String" },
                "Region": { "type": "String" },
            }),
            supplied: json!({ "Region": ["us-west-2"] }),
            missing: Some("TargetName"),
        },
        Row {
            case: "the second of two required parameters, left out",
            name: "TwoRequiredAgain",
            parameters: json!({
                "TargetName": { "type": "String" },
                "Region": { "type": "String" },
            }),
            supplied: json!({ "TargetName": ["web-tier"] }),
            missing: Some("Region"),
        },
        Row {
            case: "a required parameter beside one carrying a default",
            name: "DefaultAndRequired",
            parameters: json!({
                "AutomationAssumeRole": { "type": "String", "default": "" },
                "BucketName": { "type": "String" },
            }),
            supplied: json!({}),
            missing: Some("BucketName"),
        },
        Row {
            case: "a parameter whose name carries the separators a name may hold",
            name: "OddlyNamed",
            parameters: json!({ "S3_Bucket-Name.v2": { "type": "String" } }),
            supplied: json!({}),
            missing: Some("S3_Bucket-Name.v2"),
        },
        Row {
            case: "every parameter the runbook declares a default for",
            name: "AllDefaulted",
            parameters: json!({
                "Mode": { "type": "String", "default": "Auto" },
                "Retries": { "type": "String", "default": "3" },
            }),
            supplied: json!({}),
            missing: None,
        },
        Row {
            case: "every required parameter supplied",
            name: "FullySupplied",
            parameters: json!({
                "TargetName": { "type": "String" },
                "Region": { "type": "String" },
            }),
            supplied: json!({ "TargetName": ["web-tier"], "Region": ["us-west-2"] }),
            missing: None,
        },
    ]
}

/// RULE: the refusal names the omitted parameter, in the Automation plane's own sentence.
#[tokio::test]
async fn a_required_parameter_left_out_is_named_in_the_refusal() {
    let router = router();
    for Row {
        case,
        name,
        parameters,
        supplied,
        missing,
    } in rows()
    {
        runbook(&router, name, parameters).await;
        let mut body = json!({ "DocumentName": name });
        if supplied.as_object().is_some_and(|supplied| !supplied.is_empty()) {
            body["Parameters"] = supplied;
        }
        let (status, response) = call(&router, "StartAutomationExecution", &body).await;

        let Some(missing) = missing else {
            assert_eq!(
                status, 200,
                "case {case:?}: a start that supplies every required parameter is served, got \
                 {status} with {response}"
            );
            continue;
        };
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, PARAMETER_REFUSAL),
            "case {case:?}: expected the parameter refusal, got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            missing_required(missing),
            "case {case:?}: expected the refusal to name the omitted parameter, got {response}"
        );
    }
}

/// RULE: the same sentence answers a PREVIEW of the same start, because both read the one
/// declaration of a document's parameters.
#[tokio::test]
async fn a_preview_of_the_same_start_states_the_same_sentence() {
    let router = router();
    let case = "a preview leaving the required parameter out";
    runbook(
        &router,
        "PreviewRequiring",
        json!({ "BucketName": { "type": "String" } }),
    )
    .await;

    let (status, response) = call(
        &router,
        "StartExecutionPreview",
        &json!({ "DocumentName": "PreviewRequiring" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a preview that resolves no parameter set is refused, got {status} with \
         {response}"
    );
    assert_eq!(
        error_message(&response),
        missing_required("BucketName"),
        "case {case:?}: expected the one sentence both reads of a declaration state, got \
         {response}"
    );
}
