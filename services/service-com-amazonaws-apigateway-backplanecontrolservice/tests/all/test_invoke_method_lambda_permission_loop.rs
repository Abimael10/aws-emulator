// SPDX-License-Identifier: CC0-1.0

//! The LOOP a client follows to verify its Lambda permission wiring: simulate the method, add
//! the grant the refusal calls for, simulate the identical method again, and read the run it
//! made out of the function's log group.
//!
//! The rule under test, one level above the recorded case:
//!
//! > API Gateway decides whether a proxy integration's function is invoked from that FUNCTION's
//! > resource-based policy AS IT STANDS AT THE CALL, judged on `lambda:InvokeFunction` with the
//! > invoking method's ARN as the `aws:SourceArn` — the method under the stage AWS reserves for
//! > a test invocation. So one `TestInvokeMethod` request is answered the api's
//! > `API_CONFIGURATION_ERROR` gateway response (`500` with
//! > `{"message":"Internal server error"}`, the configuration error named in the execution log)
//! > while no statement of the policy admits that call, and the IDENTICAL request is answered
//! > the handler's own response once one does — and goes back to the refusal when that
//! > statement is removed. Every invocation that HAPPENED leaves the handler's printed output
//! > in the function's own log group, which `FilterLogEvents` answers for a `filterPattern` the
//! > line matches, and an invocation that did not happen leaves nothing there.
//!
//! The rows differ along what the rule generalises over: the `SourceArn` the added statement is
//! narrowed to — the api's methods under every stage for this verb and path (the grant the CLI
//! makes), no source at all, every segment wildcarded, another verb of the same path, and
//! another path of the same verb — which is the axis the decision turns on. Each row drives the
//! whole loop over ONE api and ONE function: refused, granted, refused again after the removal,
//! with a marker of its own per phase so the log read cannot see another phase's line.
//!
//! Only a cross-service suite can pin this: the api, the method and the simulation are API
//! Gateway's, the function and its policy are AWS Lambda's, and the log of a run that HAPPENED
//! is CloudWatch Logs' — all three over ONE engine, as `core/router/src/lib.rs` mounts them.
//! The wire helpers are [`crate::test_invoke_method_lambda_permission`]'s, reused here rather
//! than written twice.
//!
//! Where this sits beside its siblings: [`crate::test_invoke_method_lambda_permission`] varies
//! the grant a FRESH emulator is wired with and counts the runs of the function,
//! [`crate::test_invoke_stage_grant_differential`] crosses the method's own gate with the
//! function's policy, and [`crate::test_invoke_method_stage_segment_grants`] holds the deployed
//! and the simulated caller against each other. What is added here is the client's own
//! round trip — the grant forms a console or CLI user adds, the REMOVAL that takes the
//! authorization away again, and the handler's printed line read back through a `filterPattern`.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::test_invoke_method_lambda_permission::{
    accepted, emulator, lambda, logs, member, package, wait_until_active, INTERNAL_SERVER_ERROR,
    INVALID_PERMISSIONS, REGION,
};

/// The function the method proxies to, the group its runs log to, the role it names and the
/// module-qualified handler its package carries.
const FUNCTION: &str = "permission-loop-echo";
const FUNCTION_GROUP: &str = "/aws/lambda/permission-loop-echo";
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";
const HANDLER: &str = "lambda_function.lambda_handler";

/// The verb the method is put on, the path part it sits on, and the stage the api is deployed
/// to — the three segments every `SourceArn` below is spelled from.
const VERB: &str = "GET";
const RESOURCE_PATH: &str = "echo";
const STAGE: &str = "test";

/// How long this suite waits for a finished run's log to reach its group, and how often it
/// looks. Generous — the suite is one of thousands a run may be executing at once, so the
/// deadline leaves room for a loaded host — and bounded, so a group that never takes the line
/// reports it rather than holding the suite: the wait is for a CONDITION, and no assertion
/// here is on how long anything took.
const WAIT: Duration = Duration::from_secs(60);
const LOOK: Duration = Duration::from_millis(100);

/// What the handler prints before the marker it was called with, so the printed line is told
/// from the marker travelling in the response body.
const PRINTED_PREFIX: &str = "handled ";

/// The sentence the execution log of an invocation that was MADE carries.
const COMPLETED: &str = "Successfully completed execution";

/// The error CloudWatch Logs refuses a read of a group it holds no record of with
/// (`ResourceNotFoundException`, the modelled 400 of `FilterLogEvents`), which is the only
/// refusal a read taken before any run of the function can be answered.
const GROUP_ABSENT: &str = "ResourceNotFoundException";

/// The handler behind the method: it answers the proxy envelope carrying the marker the request
/// named, and PRINTS that marker, which is what a client greps the function's log group for.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    marker = (event.get("queryStringParameters") or {}).get("marker", "no-marker")
    print("handled " + marker)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"marker": marker}),
    }
"#;

/// The function, the api, its one `GET` method proxying to that function, and a deployment
/// released to the stage — the wiring the client has in place before it checks its permissions.
/// Answers the api id and the id of the resource the method sits on.
async fn wired(router: &MultiServiceRouter) -> (String, String) {
    let (status, function) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 10,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected HTTP 201, got {status} with {function}"
    );
    let function_arn = member("CreateFunction", &function, "FunctionArn");

    let api = accepted(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "permission-loop-api" })),
        201,
    )
    .await;
    let root = member("CreateRestApi", &api, "rootResourceId");
    let api = member("CreateRestApi", &api, "id");
    let resource = accepted(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    accepted(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    accepted(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/{VERB}/integration"),
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
    accepted(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    wait_until_active(router, FUNCTION).await;
    (api, resource)
}

/// Add the statement `statement_id` admitting `apigateway.amazonaws.com`
/// `lambda:InvokeFunction` on the function, narrowed to `source_arn` where the row names one.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    statement_id: &str,
    source_arn: Option<&str>,
) {
    let mut request = json!({
        "StatementId": statement_id,
        "Action": "lambda:InvokeFunction",
        "Principal": "apigateway.amazonaws.com",
    });
    if let Some(source_arn) = source_arn {
        request["SourceArn"] = json!(source_arn);
    }
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission({case:?}): expected HTTP 201, got {status} with {granted}"
    );
}

/// Remove the statement `statement_id` from the function's policy again, as a client taking the
/// authorization away does.
async fn revoke(router: &MultiServiceRouter, case: &str, statement_id: &str) {
    let (status, removed) = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{FUNCTION}/policy/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "setup RemovePermission({case:?}): expected HTTP 204, got {status} with {removed}"
    );
}

/// One simulated invocation of the method, naming `marker` in the query string the handler
/// reads it from: the simulated `status`, the simulated `body` parsed, and the execution `log`.
async fn simulate(
    router: &MultiServiceRouter,
    case: &str,
    api: &str,
    resource: &str,
    marker: &str,
) -> (i64, Option<Value>, String) {
    let simulated = accepted(
        router,
        &format!("TestInvokeMethod({case})"),
        "POST",
        &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
        Some(json!({
            "pathWithQueryString": format!("/{RESOURCE_PATH}?marker={marker}"),
        })),
        200,
    )
    .await;
    let status = simulated
        .get("status")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: no numeric status in {simulated}"));
    let body = simulated
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let log = simulated
        .get("log")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, serde_json::from_str::<Value>(&body).ok(), log)
}

/// The messages `FilterLogEvents` answers for one read of the function's group under
/// `filterPattern` `marker`.
///
/// The group exists only once a run has written to it, so the ONE refusal this read may be
/// answered is the absent group, which holds no event and so is no message. Any OTHER refusal
/// is a read that did not happen rather than a group with nothing in it, and is reported here:
/// without that, a Logs reader answering nothing at all would satisfy every assertion that
/// expects no event.
async fn matched_once(router: &MultiServiceRouter, marker: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": FUNCTION_GROUP, "filterPattern": marker }),
    )
    .await;
    if status != 200 {
        assert!(
            (status, body.to_string().contains(GROUP_ABSENT)) == (400, true),
            "case {marker:?}: expected FilterLogEvents to answer the group's events or its \
             absence, got {status} with {body}"
        );
        return Vec::new();
    }
    body.get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The same read, repeated until the group answers at least one message for `marker` or the
/// deadline passes: a finished run's log reaches its group on CloudWatch Logs' own clock, so the
/// wait is for that CONDITION.
async fn matched(router: &MultiServiceRouter, marker: &str) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let messages = matched_once(router, marker).await;
        if !messages.is_empty() || Instant::now() >= deadline {
            return messages;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// One row: the `SourceArn` the statement a client adds is narrowed to, and whether it admits
/// the SIMULATED invocation of the method.
struct Row {
    case: &'static str,
    /// The resource half of the statement's `SourceArn`, with `{api}` standing for the api
    /// under test — `None` for a statement narrowed to no source at all.
    source: Option<&'static str>,
    /// Whether the simulation is then made, which decides the reported response, the execution
    /// log and whether the handler's printed line reaches the group.
    admits: bool,
}

/// The rows: the grant forms a client adds, each covering — or failing to cover — the method the
/// simulation names.
const ROWS: &[Row] = &[
    Row {
        case: "the grant the CLI makes: every stage of the api, this verb and this path",
        source: Some("{api}/*/GET/echo"),
        admits: true,
    },
    Row {
        case: "a grant narrowed to no source at all",
        source: None,
        admits: true,
    },
    Row {
        case: "a grant wildcarding every segment of the api's methods",
        source: Some("{api}/*/*/*"),
        admits: true,
    },
    Row {
        case: "a grant naming another verb of the same path",
        source: Some("{api}/*/POST/echo"),
        admits: false,
    },
    Row {
        case: "a grant naming another path of the same verb",
        source: Some("{api}/*/GET/other"),
        admits: false,
    },
];

/// The `SourceArn` a row's statement carries: an `execute-api` ARN of this account and Region
/// over the api under test.
fn source_arn(source: &str, api: &str) -> String {
    format!(
        "arn:aws:execute-api:{REGION}:{account}:{resource_half}",
        account = engine::emulator_account_id(),
        resource_half = source.replace("{api}", api),
    )
}

/// What a REFUSED simulation reports, whichever phase of the loop refused it: the api's
/// configuration-error gateway response, the configuration error named in the execution log, and
/// no line of the handler's in the function's group.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    phase: &str,
    marker: &str,
    reported: (i64, Option<Value>, String),
) {
    let (status, body, log) = reported;
    assert_eq!(
        (status, body.clone()),
        (500, Some(json!({ "message": INTERNAL_SERVER_ERROR }))),
        "case {case:?} ({phase}): expected the api's configuration-error refusal, got {status} \
         with {body:?}"
    );
    assert!(
        log.contains(INVALID_PERMISSIONS),
        "case {case:?} ({phase}): expected the execution log to name the configuration error \
         {INVALID_PERMISSIONS:?}, got {log:?}"
    );
    let printed = matched_once(router, marker).await;
    assert!(
        printed.is_empty(),
        "case {case:?} ({phase}): expected no line of the handler's in {FUNCTION_GROUP:?} for \
         {marker:?}, got {printed:?}"
    );
}

/// The whole client loop, per grant form: the simulation refused while the function's policy
/// admits it nothing, the IDENTICAL simulation once the statement is added, and the simulation
/// again once that statement is removed.
#[tokio::test]
async fn the_statement_a_client_adds_decides_the_next_simulation_and_its_removal_the_one_after() {
    // ONE emulator, ONE api and ONE function for every row: each row adds its own statement and
    // removes it again, so the policy a row starts from is the empty one the client began with.
    let router = emulator();
    let (api, resource) = wired(&router).await;
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let statement = format!("apigw-invoke-{index}");

        // 1. Before the grant: nothing in the policy admits the call, whatever the row goes on
        // to add.
        let marker = format!("marker-{index}-before");
        let reported = simulate(&router, case, &api, &resource, &marker).await;
        assert_refused(&router, case, "before the grant", &marker, reported).await;

        // 2. The statement the client adds, and the IDENTICAL simulation after it.
        grant(
            &router,
            case,
            &statement,
            row.source.map(|source| source_arn(source, &api)).as_deref(),
        )
        .await;
        let marker = format!("marker-{index}-granted");
        let reported = simulate(&router, case, &api, &resource, &marker).await;
        if row.admits {
            let (status, body, log) = reported;
            assert_eq!(
                (status, body.clone()),
                (200, Some(json!({ "marker": marker }))),
                "case {case:?} (after the grant): expected the handler's own 200 and body, got \
                 {status} with {body:?}"
            );
            assert!(
                log.contains(COMPLETED),
                "case {case:?} (after the grant): expected the execution log to report the \
                 invocation, got {log:?}"
            );
            // What a status alone cannot show: the function RAN, and its printed line is in its
            // own group, answered for the pattern a client greps it with.
            let printed = matched(&router, &marker).await;
            assert!(
                printed
                    .iter()
                    .any(|message| message.contains(&format!("{PRINTED_PREFIX}{marker}"))),
                "case {case:?} (after the grant): expected {FUNCTION_GROUP:?} to hold the \
                 handler's printed line {:?}, got {printed:?}",
                format!("{PRINTED_PREFIX}{marker}")
            );
            assert!(
                printed.iter().all(|message| message.contains(&marker)),
                "case {case:?} (after the grant): expected every event answered for the pattern \
                 {marker:?} to carry it, got {printed:?}"
            );
        } else {
            assert_refused(&router, case, "after the grant", &marker, reported).await;
        }

        // 3. The removal: the same call is decided by the policy as it stands NOW, so it is
        // refused again and the handler does not run.
        revoke(&router, case, &statement).await;
        let marker = format!("marker-{index}-revoked");
        let reported = simulate(&router, case, &api, &resource, &marker).await;
        assert_refused(&router, case, "after the removal", &marker, reported).await;
    }
}
