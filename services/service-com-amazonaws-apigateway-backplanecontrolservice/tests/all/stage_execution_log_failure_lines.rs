// SPDX-License-Identifier: CC0-1.0

//! What a stage's EXECUTION LOG says about a request its backend answered badly, and WHICH of
//! a trace's lines each `loggingLevel` writes.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A method whose integration is `AWS_PROXY` is answered from the function's own return
//! > value, so a return value API Gateway cannot read as a proxy response is a CONFIGURATION
//! > ERROR of the integration: the request is answered `502` with the fixed
//! > `{"message":"Internal server error"}` body, and — because that answer is built from the
//! > backend's own answer rather than through a gateway response of the api, so nothing in it
//! > says what was wrong — the stage's execution log carries that error in API Gateway's own
//! > words for the class, `Execution failed due to configuration error: Malformed Lambda proxy
//! > response`. Which lines of a request's trace are written is the `loggingLevel` in force
//! > for the method it reached: the two documented levels are nested, so `ERROR` writes the
//! > entries that report the FAILURE and `INFO` writes those and the informational steps of
//! > serving the request beside them — the verb and path, the header names, the payload lines
//! > and the status the method completed with.
//!
//! The rows differ along what the rule generalises over: the level in force (`ERROR` and
//! `INFO`), and what the function returned (a document carrying no `statusCode`, a document
//! that is not an object at all, and a well-formed proxy response, which is the control that
//! earns no configuration error).
//!
//! Only a cross-service suite can pin this: the api, its stage and the logging configuration
//! are API Gateway's, the role the account logs under is AWS IAM's, the function behind the
//! method is AWS Lambda's, and the group the trace lands in is CloudWatch Logs' — all four on
//! ONE engine, as `core/router/src/lib.rs` mounts them. The trace is read back through
//! CloudWatch Logs' own `FilterLogEvents`, so what is asserted is what a client reading the
//! group holds.

use base64::Engine as _;
use serde_json::json;

use crate::account_logging_role::{create_role, replace_role, role_arn, trusting};
use crate::proxy_binary_media_types::{
    lambda, member, package, wait_until_active, FUNCTION_TIMEOUT, HANDLER, REGION, ROLE,
};
use crate::rest_api_lifecycle::{
    create_api, gateway_with_iam_logs_and_lambda, resource_under, Gateway,
};
use crate::stage_access_logging::{delivered, invoke, traced_line};

/// The stage each api is deployed to, and the resource its one method sits on.
const STAGE: &str = "live";
const RESOURCE: &str = "answer";

/// The function every integration names.
const FUNCTION: &str = "badly-answering-handler";

/// The account's CloudWatch role, created through IAM's own `CreateRole` under the path AWS's
/// managed push-to-logs role is created under: API Gateway writes a stage's execution log as
/// this role, so execution logging cannot be turned on before it is installed.
const CLOUDWATCH_ROLE: &str = "apigw-execution-failure-lines";
const CLOUDWATCH_ROLE_PATH: &str = "/service-role/";

/// The status and body API Gateway answers a proxy response it cannot read with. The body
/// is the envelope API Gateway serializes ITSELF — a space after the member separator —
/// rather than the compact body a gateway response's default template renders, because no
/// gateway response type carries this status.
const BAD_GATEWAY: (u16, &str) = (502, r#"{"message": "Internal server error"}"#);

/// The sentence API Gateway states that class of configuration error in, as the execution log
/// of the request carries it.
const MALFORMED: &str = "Execution failed due to configuration error: Malformed Lambda proxy \
                         response";

/// The ONE handler every row runs: the `shape` query parameter says what it returns, so one
/// deployment package serves every row and the rows differ in the ANSWER rather than in which
/// function answered.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    shape = (event.get("queryStringParameters") or {}).get("shape") or "well-formed"
    if shape == "no-status":
        return {"body": json.dumps({"answered": True})}
    if shape == "not-an-object":
        return "just a string"
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"answered": True}),
    }
"#;

/// One row: the level the stage's execution logging is on at, what the function returns, and
/// what the client and the trace then hold.
struct Row {
    case: &'static str,
    /// The `loggingLevel` written at the stage-wide `*/*` scope.
    level: &'static str,
    /// The `shape` the handler is asked for.
    shape: &'static str,
    /// The status the stage answers.
    status: u16,
    /// Whether the trace carries the configuration error of the malformed answer.
    malformed: bool,
    /// Whether the trace carries the informational steps `INFO` writes.
    informational: bool,
}

/// The rows: each malformed shape at `ERROR` and at `INFO`, and the well-formed control at
/// each level.
const ROWS: &[Row] = &[
    Row {
        case: "a document with no statusCode, at ERROR",
        level: "ERROR",
        shape: "no-status",
        status: BAD_GATEWAY.0,
        malformed: true,
        informational: false,
    },
    Row {
        case: "a document with no statusCode, at INFO",
        level: "INFO",
        shape: "no-status",
        status: BAD_GATEWAY.0,
        malformed: true,
        informational: true,
    },
    Row {
        case: "a return value that is not an object at all, at ERROR",
        level: "ERROR",
        shape: "not-an-object",
        status: BAD_GATEWAY.0,
        malformed: true,
        informational: false,
    },
    Row {
        case: "a return value that is not an object at all, at INFO",
        level: "INFO",
        shape: "not-an-object",
        status: BAD_GATEWAY.0,
        malformed: true,
        informational: true,
    },
    Row {
        case: "a well-formed proxy response, at INFO",
        level: "INFO",
        shape: "well-formed",
        status: 200,
        malformed: false,
        informational: true,
    },
    Row {
        case: "a well-formed proxy response, at ERROR: no error, so no trace at all",
        level: "ERROR",
        shape: "well-formed",
        status: 200,
        malformed: false,
        informational: false,
    },
];

/// Install the account's CloudWatch role, which is what every row needs before its stage may
/// be configured to log executions at all.
async fn install_account_role(gateway: &Gateway) {
    create_role(
        gateway,
        CLOUDWATCH_ROLE,
        CLOUDWATCH_ROLE_PATH,
        &trusting("apigateway.amazonaws.com"),
    )
    .await;
    let installed = replace_role(gateway, &role_arn(CLOUDWATCH_ROLE_PATH, CLOUDWATCH_ROLE)).await;
    assert_eq!(
        installed.status, 200,
        "setup UpdateAccount /cloudwatchRoleArn: expected 200, got {} with {:?}",
        installed.status, installed.body
    );
}

/// Create the one function every row's integration names, answering its ARN.
async fn given_function(gateway: &Gateway) -> String {
    let created = lambda(
        &gateway.router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup CreateFunction: expected 201, got {} with {:?}",
        created.status,
        created.text()
    );
    let arn = member("CreateFunction", &created.json(), "FunctionArn");
    wait_until_active(&gateway.router, FUNCTION).await;
    arn
}

/// An api whose one `GET` method proxies to the function, granted, deployed to the stage and
/// configured to log executions at `level`. Answers the api id.
async fn deployed(gateway: &Gateway, name: &str, function_arn: &str, level: &str) -> String {
    let (api, root) = create_api(gateway, name).await;
    let resource = resource_under(gateway, &api, &root, RESOURCE).await;
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    let put = gateway
        .call(
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
        )
        .await;
    assert_eq!(
        put.status, 201,
        "setup PutMethod: expected 201, got {} with {:?}",
        put.status, put.body
    );
    let integration = gateway
        .call(
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "AWS_PROXY",
                "httpMethod": "POST",
                "uri": format!(
                    "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                     {function_arn}/invocations"
                ),
            })),
        )
        .await;
    assert_eq!(
        integration.status, 201,
        "setup PutIntegration: expected 201, got {} with {:?}",
        integration.status, integration.body
    );
    let granted = lambda(
        &gateway.router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": format!("apigateway-invoke-{name}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/*/*",
                account = engine::emulator_account_id(),
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected 201, got {} with {:?}",
        granted.status,
        granted.text()
    );
    let deployment = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
        )
        .await;
    assert_eq!(
        deployment.status, 201,
        "setup CreateDeployment: expected 201, got {} with {:?}",
        deployment.status, deployment.body
    );
    let logging = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/*/*/logging/loglevel", "value": level },
                ],
            })),
        )
        .await;
    assert_eq!(
        logging.status, 200,
        "setup UpdateStage loggingLevel={level}: expected 200, got {} with {:?}",
        logging.status, logging.body
    );
    api
}

/// RULE: a proxy answer API Gateway cannot read earns the client a `502` and the stage's
/// execution log the configuration error that answer was, and the level in force decides which
/// of the trace's lines are written.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits an invocation this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_answer_this_api_cannot_read_is_traced_as_a_configuration_error() {
    let gateway = gateway_with_iam_logs_and_lambda();
    install_account_role(&gateway).await;
    let function_arn = given_function(&gateway).await;

    for (rank, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One api per row, so the trace each row is judged by is its own.
        let api = deployed(
            &gateway,
            &format!("execution-failure-lines-{rank}"),
            &function_arn,
            row.level,
        )
        .await;
        let answered = invoke(
            &gateway.router,
            &api,
            STAGE,
            "GET",
            &format!("/{RESOURCE}?shape={}", row.shape),
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {case:?}: expected HTTP {}, got {} with {:?}",
            row.status,
            answered.status,
            answered.text()
        );
        if row.status == BAD_GATEWAY.0 {
            assert_eq!(
                answered.text(),
                BAD_GATEWAY.1,
                "case {case:?}: expected the fixed body of this class, got {:?}",
                answered.text()
            );
        }
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the answer named no request id"));

        let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
        let records = delivered(&gateway.router, &group).await.unwrap_or_default();
        let prefix = traced_line(&request_id, "");
        let trace: Vec<String> = records
            .iter()
            .filter(|record| record.starts_with(&prefix))
            .cloned()
            .collect();

        let malformed = traced_line(&request_id, MALFORMED);
        assert_eq!(
            trace.contains(&malformed),
            row.malformed,
            "case {case:?}: expected the record {malformed:?} in this request's trace={}, got \
             {trace:?}",
            row.malformed
        );
        for line in [
            traced_line(&request_id, &format!("HTTP Method: GET, Resource Path: /{RESOURCE}")),
            traced_line(
                &request_id,
                &format!("Method completed with status: {}", row.status),
            ),
        ] {
            assert_eq!(
                trace.contains(&line),
                row.informational,
                "case {case:?}: expected the record {line:?} in this request's trace={}, got \
                 {trace:?}",
                row.informational
            );
        }
    }
}
