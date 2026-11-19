// SPDX-License-Identifier: CC0-1.0

//! WHICH `methodSettings` entry gates the execution log of a request a deployed REST stage
//! served: the entry keyed by the METHOD the request reached, and only then the stage-wide
//! one.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `MethodSetting$loggingLevel` "specifies the logging level for this method, which affects
//! > the log entries pushed to Amazon CloudWatch Logs", with "valid values are `OFF`, `ERROR`,
//! > and `INFO`" — so the setting in force for a request is the stage's `methodSettings` entry
//! > keyed by the method it reached (`{resource-path}/{http-method}`, the resource path's own
//! > `/` written as the JSON Pointer escape a patch addressed it with) where the stage carries
//! > one, and the stage-wide `*/*` entry otherwise. That one level decides what API Gateway
//! > writes into the group it publishes for the stage
//! > (`API-Gateway-Execution-Logs_{api-id}/{stage}`): `INFO` traces every request the stage
//! > answered, `ERROR` traces only one it answered WITH an error, and `OFF` — or no entry in
//! > force at all — traces none. A per-method entry therefore overrides the stage-wide entry
//! > for its own method and leaves every other method of the stage under the stage-wide one.
//!
//! Only a cross-service suite can pin this: the api, its stage's method settings and the
//! emission of the trace are API Gateway's, the proxied function is AWS Lambda's, and the
//! group the trace lands in is CloudWatch Logs' — all three on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every assertion reads the outcome back through
//! CloudWatch Logs' own reader (`FilterLogEvents`), so a record that did not reach that store
//! cannot pass.
//!
//! The rows differ along what the rule generalises over: which method of the stage carries an
//! entry of its own (one, two, none), the level that entry carries, the level the stage-wide
//! entry carries (including none at all), the verb the request is served by, and — in the
//! second test — the STATUS the stage answered with, which is what separates `ERROR` from
//! `INFO`. The recorded case is one row: the stage at `ERROR` with one method raised to
//! `INFO`.
//!
//! The wire harness is this crate's existing one: the router (API Gateway, AWS Lambda and
//! CloudWatch Logs on one engine), the invoke and the log reads are
//! [`crate::stage_access_logging`]'s, and the control-plane and Lambda legs are
//! [`crate::proxy_binary_media_types`]'s. Only the api, its three methods and the gating are
//! stood up here.

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{
    apigw, lambda, member, package, wait_until_active, FUNCTION_TIMEOUT, HANDLER, REGION, ROLE,
};
use crate::stage_access_logging::{delivered, emulator, install_account_role, invoke, traced_line};

/// The stage every row deploys to.
const STAGE: &str = "s";

/// The path part of the one resource every method below is put on, and the resource path it
/// makes: `/{RESOURCE_PART}`.
const RESOURCE_PART: &str = "echo";

/// The `methodSettings` key that names every method of the stage.
const EVERY_METHOD_KEY: &str = "*/*";

/// The verbs the one resource carries, each its own method with its own integration: three, so
/// a row can raise one, leave one under the stage-wide entry and still have a third to show
/// the fallback is not the raised one.
const VERBS: [&str; 3] = ["GET", "POST", "DELETE"];

/// The function every method proxies to.
const FUNCTION: &str = "method-level-logging";

/// The handler behind every method: the request's own query string says which status it is to
/// answer, so one package serves every leg — a served request and one the stage answers with
/// an error alike.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    query = event.get("queryStringParameters") or {}
    status = int(query.get("status") or "200")
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"verb": event.get("httpMethod"), "status": status}),
    }
"#;

/// Create the function every integration names, waiting for Lambda to report it `Active`.
/// Answers its ARN.
async fn given_function(router: &MultiServiceRouter) -> String {
    let created = lambda(
        router,
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
        "setup CreateFunction: expected HTTP 201, got {} with {:?}",
        created.status,
        created.text()
    );
    let arn = member("CreateFunction", &created.json(), "FunctionArn");
    wait_until_active(router, FUNCTION).await;
    arn
}

/// One api carrying `/{RESOURCE_PART}` with a method per [`VERBS`], each an `AWS_PROXY`
/// integration at `function_arn`, the grant that admits those invocations, and a deployment
/// released to [`STAGE`]. Answers the api id.
///
/// `label` separates one row's api, and the grant's own statement, from another's.
async fn deployed(router: &MultiServiceRouter, function_arn: &str, label: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": format!("method-level-logging-{label}") })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let resources = apigw(
        router,
        "GetResources",
        "GET",
        &format!("/restapis/{api}/resources"),
        None,
        200,
    )
    .await;
    let root = resources
        .get("item")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|item| member("GetResources", item, "id"))
        .expect("the api's root resource");
    let created = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PART })),
        201,
    )
    .await;
    let resource = member("CreateResource", &created, "id");
    for verb in VERBS {
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/{verb}"),
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/{verb}/integration"),
            Some(json!({
                "type": "AWS_PROXY",
                "httpMethod": "POST",
                "uri": format!(
                    "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                     {function_arn}/invocations"
                ),
            })),
            201,
        )
        .await;
    }
    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits the invocations API Gateway makes for this api as its own service principal. It
    // covers every method of it, so what a leg below shows is the LOGGING and never a refused
    // invocation.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": format!("apigw-invoke-{label}"),
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/*/*/*",
                account = engine::emulator_account_id(),
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission({label}): expected HTTP 201, got {} with {:?}",
        granted.status,
        granted.text()
    );
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// The `methodSettings` key API Gateway reports the per-method setting of `verb` on
/// `/{RESOURCE_PART}` under: the method path, which is the resource path without its leading
/// separator joined to the HTTP method.
fn method_key(verb: &str) -> String {
    format!("{RESOURCE_PART}/{verb}")
}

/// The `UpdateStage` patch path that addresses a setting of that same method, behind the
/// pointer's own leading separator: inside a pointer the resource path is one reference token,
/// so its leading separator is the escape `~1`.
fn method_patch_path(verb: &str) -> String {
    format!("~1{RESOURCE_PART}/{verb}")
}

/// Gate the stage's execution logs: the stage-wide `loggingLevel` where `stage_wide` names one,
/// and a per-method one for each `(verb, level)` of `per_method`.
///
/// Also asserts WHICH entries the stage then reports, because the key a patch stored is the key
/// the gating is resolved by: a stage reporting another set of keys is gating other methods
/// than the ones the caller named.
async fn gate(
    router: &MultiServiceRouter,
    api: &str,
    case: &str,
    stage_wide: Option<&str>,
    per_method: &[(&str, &str)],
) {
    let mut operations: Vec<Value> = Vec::new();
    let mut expected: Vec<String> = Vec::new();
    if let Some(level) = stage_wide {
        operations.push(json!({
            "op": "replace",
            "path": format!("/{EVERY_METHOD_KEY}/logging/loglevel"),
            "value": level,
        }));
        expected.push(EVERY_METHOD_KEY.to_string());
    }
    for (verb, level) in per_method {
        operations.push(json!({
            "op": "replace",
            "path": format!("/{}/logging/loglevel", method_patch_path(verb)),
            "value": level,
        }));
        expected.push(method_key(verb));
    }
    if !operations.is_empty() {
        apigw(
            router,
            "UpdateStage methodSettings",
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": operations })),
            200,
        )
        .await;
    }
    expected.sort();

    let read = apigw(
        router,
        "GetStage",
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    let mut reported: Vec<String> = read
        .get("methodSettings")
        .and_then(Value::as_object)
        .map(|settings| settings.keys().cloned().collect())
        .unwrap_or_default();
    reported.sort();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected GetStage to report the methodSettings keys {expected:?}, got \
         {reported:?} in {read:?}"
    );
    for (verb, level) in per_method {
        let reported = &read["methodSettings"][method_key(verb)]["loggingLevel"];
        assert_eq!(
            reported,
            &json!(level),
            "case {case:?}: expected the entry of {verb} to carry loggingLevel {level:?}, got \
             {reported:?} in {read:?}"
        );
    }
}

/// Drive ONE request through the stage, served by `verb` and answered with `status`, and assert
/// what the stage's execution log then holds about it: the trace the level in force calls for,
/// or nothing at all.
///
/// `at_info` is whether the level in force for that method is `INFO`, which is what decides
/// WHICH lines a written trace carries: `INFO` writes the informational steps of serving the
/// request beside the entries that report a failure, and `ERROR` writes only the latter.
async fn one_leg(
    router: &MultiServiceRouter,
    api: &str,
    row_case: &str,
    verb: &str,
    status: u16,
    traced: bool,
    at_info: bool,
) {
    let case = &format!("{row_case}: {verb} answered {status}");
    let answered = invoke(
        router,
        api,
        STAGE,
        verb,
        &format!("/{RESOURCE_PART}?status={status}"),
    )
    .await;
    assert_eq!(
        (answered.status, answered.json()),
        (status, json!({ "verb": verb, "status": status })),
        "case {case:?}: expected the function's own answer, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let request_id = answered
        .request_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));

    // The records of THIS request, read back through CloudWatch Logs' own reader: every record
    // of a stage's execution log names the request it is about, which is what separates this
    // request's trace from the traces of the requests before it in the group. A stage that has
    // traced nothing yet has no group of its own either, which is as much "nothing was written"
    // as an empty trace in a group is.
    let group = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
    let records = delivered(router, &group).await.unwrap_or_default();
    let prefix = traced_line(&request_id, "");
    let trace: Vec<String> = records
        .iter()
        .filter(|record| record.starts_with(&prefix))
        .cloned()
        .collect();
    if !traced {
        assert!(
            trace.is_empty(),
            "case {case:?}: expected no trace of this request in {group:?}, got {trace:?}"
        );
        return;
    }
    // The line that OPENS a request's log is in every trace, whichever level wrote it: it is
    // what names the request the following records are about.
    let opens = traced_line(&request_id, &format!("Execution log for request {request_id}"));
    assert!(
        trace.contains(&opens),
        "case {case:?}: expected the record {opens:?} in this request's trace, got {trace:?}"
    );
    // The INFORMATIONAL steps of serving the request, which only a trace written at `INFO`
    // carries: a trace written at `ERROR` reports the failure and nothing beside it.
    for line in [
        traced_line(
            &request_id,
            &format!("HTTP Method: {verb}, Resource Path: /{RESOURCE_PART}"),
        ),
        traced_line(&request_id, &format!("Method completed with status: {status}")),
    ] {
        assert_eq!(
            trace.contains(&line),
            at_info,
            "case {case:?}: expected the record {line:?} in this request's trace={at_info}, \
             got {trace:?}"
        );
    }
}

/// One row of the precedence table: which entries the stage carries, and which of its methods
/// then leave a trace of a request they SERVED.
struct Row {
    case: &'static str,
    /// The `loggingLevel` of the stage-wide `*/*` entry, or `None` for a stage carrying none.
    stage_wide: Option<&'static str>,
    /// The per-method entries: the verb whose own key is patched, and the level it carries.
    per_method: &'static [(&'static str, &'static str)],
    /// Whether a request each verb answers `200` leaves a trace. A row's POSITIVE legs come
    /// first, so a later negative one is asserted against a group the stage has already
    /// delivered into rather than against one that does not exist yet.
    traced: &'static [(&'static str, bool)],
}

/// The rows: the recorded shape, the same override the other way about, a per-method entry with
/// no stage-wide entry behind it, two methods raised over a stage-wide `OFF`, and the
/// stage-wide entry alone.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the stage at ERROR with one method raised to INFO",
            stage_wide: Some("ERROR"),
            per_method: &[("POST", "INFO")],
            traced: &[("POST", true), ("GET", false), ("DELETE", false)],
        },
        Row {
            case: "the stage at INFO with one method turned OFF",
            stage_wide: Some("INFO"),
            per_method: &[("POST", "OFF")],
            traced: &[("GET", true), ("DELETE", true), ("POST", false)],
        },
        Row {
            case: "one method at INFO with no stage-wide entry at all",
            stage_wide: None,
            per_method: &[("GET", "INFO")],
            traced: &[("GET", true), ("POST", false), ("DELETE", false)],
        },
        Row {
            case: "two methods with their own levels over a stage-wide OFF",
            stage_wide: Some("OFF"),
            per_method: &[("GET", "INFO"), ("POST", "ERROR")],
            traced: &[("GET", true), ("POST", false), ("DELETE", false)],
        },
        Row {
            case: "the stage-wide entry alone, at INFO",
            stage_wide: Some("INFO"),
            per_method: &[],
            traced: &[("GET", true), ("POST", true), ("DELETE", true)],
        },
    ]
}

/// The entry keyed by the METHOD a request reached is the one that gates its execution log,
/// and the stage-wide entry gates exactly the methods carrying none of their own.
#[tokio::test]
async fn the_entry_of_the_matched_method_gates_its_execution_log() {
    let router = emulator();
    let function_arn = given_function(&router).await;
    install_account_role(&router).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // Each row stands its own api up, so the entries under test are exactly the ones this
        // row patched: a stage carried over from another row would gate by that row's keys.
        let api = deployed(&router, &function_arn, &index.to_string()).await;
        gate(&router, &api, case, row.stage_wide, row.per_method).await;
        for (verb, traced) in row.traced {
            // Every leg here is answered `200`, which only `INFO` traces at all, so a written
            // trace of one is an `INFO` trace.
            one_leg(&router, &api, case, verb, 200, *traced, true).await;
        }
    }
}

/// One leg of the status table: the verb a request is served by, the status the stage answers it
/// with, whether the level in force for THAT method traces it, and whether that level is `INFO`
/// — which is what decides which lines the trace carries.
struct Leg {
    verb: &'static str,
    status: u16,
    traced: bool,
    at_info: bool,
}

/// The legs: the level in force for a method decides which of ITS requests leave a trace —
/// `INFO` every one, `ERROR` only the ones answered with an error, `OFF` none — and each method
/// is decided by its own level rather than by another method's.
fn legs() -> Vec<Leg> {
    vec![
        Leg { verb: "GET", status: 200, traced: true, at_info: true },
        Leg { verb: "GET", status: 503, traced: true, at_info: true },
        Leg { verb: "POST", status: 200, traced: false, at_info: false },
        Leg { verb: "POST", status: 500, traced: true, at_info: false },
        Leg { verb: "DELETE", status: 200, traced: false, at_info: false },
        Leg { verb: "DELETE", status: 500, traced: false, at_info: false },
    ]
}

/// A method's own `loggingLevel` decides which of its requests leave a trace: `INFO` traces
/// every one, `ERROR` only those the stage answered with an error, and a method left to a
/// stage-wide `OFF` traces none however it was answered.
#[tokio::test]
async fn a_methods_own_level_decides_which_of_its_requests_leave_a_trace() {
    let router = emulator();
    let function_arn = given_function(&router).await;
    install_account_role(&router).await;
    let api = deployed(&router, &function_arn, "statuses").await;
    let case = "GET at INFO and POST at ERROR over a stage-wide OFF";
    gate(
        &router,
        &api,
        case,
        Some("OFF"),
        &[("GET", "INFO"), ("POST", "ERROR")],
    )
    .await;

    for leg in legs() {
        one_leg(
            &router,
            &api,
            case,
            leg.verb,
            leg.status,
            leg.traced,
            leg.at_info,
        )
        .await;
    }
}
