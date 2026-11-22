// SPDX-License-Identifier: CC0-1.0

//! What a stage whose methods are RUN by an `AWS_PROXY` Lambda integration delivers into the
//! group its own `accessLogSettings` name.
//!
//! The rule under test, one level above the recorded case:
//!
//! > With the account's CloudWatch role installed, a deployed stage carrying
//! > `accessLogSettings` — a `destinationArn` naming a CloudWatch Logs log group and "a single
//! > line format of the access logs of data, as specified by selected $context variables"
//! > (`AccessLogSettings$format`) — delivers exactly ONE record per request it served into
//! > THAT group, with every `$context` reference in the format resolved from THAT request and
//! > the stage that served it. WHICH integration answered decides none of it: a method run by
//! > a Lambda function through an `AWS_PROXY` integration is logged like any other, and
//! > `$context.status` is the status that run actually returned.
//!
//! Each reference carries what it is published to carry: `$context.requestId` the id the
//! answer was reported under, `$context.httpMethod` the verb the request was served under,
//! `$context.stage` the stage's own name, `$context.resourcePath` the un-prefixed resource
//! path of the matched method, and `$context.path` the stage-qualified path the client asked
//! for.
//!
//! The sibling suites span the RENDERING of a format and the two paths one record names on a
//! `MOCK` method that reaches no backend (`stage_access_log_record_rendering`,
//! `stage_access_log_request_path`), and the destinations and outcomes of a proxy-integrated
//! stage answering one verb with one status (`stage_access_logging`); what this suite adds is
//! the two halves neither of them crosses — the STATUS a function's run chose, and the
//! stage-qualified `$context.path` beside the matched template, on a method that RUNS.
//!
//! Only a cross-service suite can pin this: the api, its stage and the logging configuration
//! are API Gateway's, the run that answers is AWS Lambda's, and the group each record lands in
//! is CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs` mounts them.
//! Every record is read back through CloudWatch Logs' own `FilterLogEvents`, so a record that
//! never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the VERB the method is served under,
//! the shape of the resource the request matched (one segment, nested, parameterised), whether
//! a QUERY STRING was sent, the STATUS the function returned (a success, a created, a
//! not-found), WHICH stage served the request, and the REPETITION of one request.

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{
    apigw, lambda, member, package, wait_until_active, FUNCTION_TIMEOUT, HANDLER, REGION, ROLE,
};
use crate::stage_access_log_absent_destination::destination_arn;
use crate::stage_access_logging::{
    create_log_group, delivered, emulator, install_account_role, invoke,
};

/// The two stages the api is deployed to, each pointed at its own destination, so WHICH stage
/// served a request is a row rather than a constant of the suite.
const TEST: &str = "test";
const PROD: &str = "prod";

/// The destination each of those stages names.
const TEST_GROUP: &str = "/apigw/proxy-access/test";
const PROD_GROUP: &str = "/apigw/proxy-access/prod";

/// The function every method of the api proxies to.
const FUNCTION: &str = "apigw-access-log-context";

/// The format every stage below renders its records by: the `$context` references the rule
/// names, beside a literal member, so one record answers every half of it. It selects
/// `$context.requestId`, which `AccessLogSettings$format` requires of a format at all ("the
/// format must include at least `$context.requestId`").
const FORMAT: &str = concat!(
    r#"{"marker":"proxy-access-log","requestId":"$context.requestId","#,
    r#""httpMethod":"$context.httpMethod","path":"$context.path","#,
    r#""status":"$context.status","stage":"$context.stage","#,
    r#""resourcePath":"$context.resourcePath"}"#
);

/// The handler behind every method: the query string says which status the run answers with,
/// so one package serves every row and the status in each record is one a RUN returned.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    query = event.get("queryStringParameters") or {}
    status = int(query.get("status") or "200")
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"answered": status}),
    }
"#;

/// One row: the request made, and what the record it leaves has to say about it.
struct Row {
    case: &'static str,
    /// The stage the request is addressed to, which `$context.stage` reports.
    stage: &'static str,
    /// The destination THAT stage names, which is where this row's record lands.
    group: &'static str,
    /// The verb the request is sent under, which `$context.httpMethod` reports.
    verb: &'static str,
    /// What the client asks for within the stage, query string included.
    asked_for: &'static str,
    /// The `$context.path` the record has to carry: the stage and the path asked for, with no
    /// query string in it.
    path: &'static str,
    /// The `$context.resourcePath` the record has to carry: the template that matched.
    resource_path: &'static str,
    /// The status the function's run answers with, which `$context.status` reports in decimal.
    status: u16,
}

/// The rows: the verbs, the resource shapes, the statuses a run returned, the second stage and
/// a repeat of one request.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a GET the function answers 200, on a one-segment resource",
            stage: TEST,
            group: TEST_GROUP,
            verb: "GET",
            asked_for: "/ping",
            path: "/test/ping",
            resource_path: "/ping",
            status: 200,
        },
        Row {
            case: "the same request again, which is a second record",
            stage: TEST,
            group: TEST_GROUP,
            verb: "GET",
            asked_for: "/ping",
            path: "/test/ping",
            resource_path: "/ping",
            status: 200,
        },
        Row {
            case: "a POST whose run answers 201, with a query string the path excludes",
            stage: TEST,
            group: TEST_GROUP,
            verb: "POST",
            asked_for: "/ping?status=201",
            path: "/test/ping",
            resource_path: "/ping",
            status: 201,
        },
        Row {
            case: "a DELETE whose run answers 404, which the stage still served",
            stage: TEST,
            group: TEST_GROUP,
            verb: "DELETE",
            asked_for: "/ping?status=404",
            path: "/test/ping",
            resource_path: "/ping",
            status: 404,
        },
        Row {
            case: "a nested resource",
            stage: TEST,
            group: TEST_GROUP,
            verb: "GET",
            asked_for: "/deep/ping",
            path: "/test/deep/ping",
            resource_path: "/deep/ping",
            status: 200,
        },
        Row {
            case: "a parameterised resource, where the template and the path asked for differ",
            stage: TEST,
            group: TEST_GROUP,
            verb: "GET",
            asked_for: "/items/42",
            path: "/test/items/42",
            resource_path: "/items/{id}",
            status: 200,
        },
        Row {
            case: "the other stage of the same api, with its own destination",
            stage: PROD,
            group: PROD_GROUP,
            verb: "GET",
            asked_for: "/items/other-value",
            path: "/prod/items/other-value",
            resource_path: "/items/{id}",
            status: 200,
        },
    ]
}

/// `CreateResource` of `path_part` under `parent`, answering the created resource's id.
///
/// `pub(crate)` because the sibling suite that takes ONE proxy-integrated stage through a
/// sequence of logging configurations ([`crate::stage_access_log_reconfiguration`]) builds its
/// resources the same way: one helper, so two suites cannot wire a resource tree differently
/// while believing they agree.
pub(crate) async fn child(
    router: &MultiServiceRouter,
    api: &str,
    parent: &str,
    path_part: &str,
) -> String {
    let created = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    member("CreateResource", &created, "id")
}

/// One open method on `resource`, served by `function_arn` through an `AWS_PROXY` integration —
/// "for integrating the method request with the Lambda function-invoking action with the client
/// request passed through as-is" (`IntegrationType`).
///
/// `pub(crate)` for the reason [`child`] gives: the sibling suite serves its methods through the
/// same integration, and one helper is what keeps the two from wiring it differently.
pub(crate) async fn proxy_method(
    router: &MultiServiceRouter,
    api: &str,
    resource: &str,
    verb: &str,
    function_arn: &str,
) {
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
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
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

/// Point `stage`'s access logging at `group`, rendered by [`FORMAT`], as an operator configures
/// it.
async fn configure_access_logging(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    group: &str,
) {
    apigw(
        router,
        "UpdateStage accessLogSettings",
        "PATCH",
        &format!("/restapis/{api}/stages/{stage}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/accessLogSettings/destinationArn",
                    // The group itself, which is one of the two published forms of a
                    // log-group ARN; the sibling suite that spans both owns the reading.
                    "value": destination_arn(group, false),
                },
                { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
            ],
        })),
        200,
    )
    .await;
}

/// The whole setup: the function, an api whose methods all proxy to it, the grant that admits
/// the invocation, and a deployment released to both stages. Answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
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
        "setup CreateFunction: expected HTTP 201 from Lambda, got {} with {:?}",
        created.status,
        created.text()
    );
    let function_arn = member("CreateFunction", &created.json(), "FunctionArn");

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "proxy-access-logged-api" })),
        201,
    )
    .await;
    let root = member("CreateRestApi", &api, "rootResourceId");
    let api = member("CreateRestApi", &api, "id");

    let ping = child(router, &api, &root, "ping").await;
    for verb in ["GET", "POST", "DELETE"] {
        proxy_method(router, &api, &ping, verb, &function_arn).await;
    }
    let deep = child(router, &api, &root, "deep").await;
    let deep_ping = child(router, &api, &deep, "ping").await;
    proxy_method(router, &api, &deep_ping, "GET", &function_arn).await;
    let items = child(router, &api, &root, "items").await;
    let item = child(router, &api, &items, "{id}").await;
    proxy_method(router, &api, &item, "GET", &function_arn).await;

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
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
        "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
        granted.status,
        granted.text()
    );

    for stage in [TEST, PROD] {
        apigw(
            router,
            "CreateDeployment",
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
            201,
        )
        .await;
    }
    wait_until_active(router, FUNCTION).await;
    api
}

/// Every request a proxy-integrated stage serves leaves exactly one record in the destination
/// THAT stage names, with each `$context` reference of its format resolved from that request.
#[tokio::test]
async fn a_proxy_integrated_stage_logs_one_resolved_record_per_request_it_served() {
    let router = emulator();
    let api = deployed(&router).await;
    install_account_role(&router).await;
    // Each destination is a log group of the ACCOUNT's, created before a stage is pointed at
    // it: API Gateway delivers into a group that is there and brings none into existence.
    create_log_group(&router, TEST_GROUP).await;
    create_log_group(&router, PROD_GROUP).await;
    configure_access_logging(&router, &api, TEST, TEST_GROUP).await;
    configure_access_logging(&router, &api, PROD, PROD_GROUP).await;

    let mut counted: Vec<(&str, usize)> = vec![(TEST_GROUP, 0), (PROD_GROUP, 0)];
    for row in rows() {
        let case = row.case;
        let answered = invoke(&router, &api, row.stage, row.verb, row.asked_for).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from the stage, got {} with {:?}",
            row.status,
            answered.status,
            answered.text()
        );
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected a non-empty request id, got {request_id:?}"
        );

        // One request is one record, and only in the destination the stage it reached names.
        for entry in &mut counted {
            if entry.0 == row.group {
                entry.1 += 1;
            }
        }
        for (group, expected) in &counted {
            let records = delivered(&router, group).await.unwrap_or_default();
            assert_eq!(
                records.len(),
                *expected,
                "case {case:?}: expected {expected} record(s) in {group:?}, got {records:?}"
            );
        }

        let records = delivered(&router, row.group)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: no group {:?} at all", row.group));
        let record = records
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: no record in {:?}", row.group))
            .clone();
        let rendered: Value = serde_json::from_str(&record).unwrap_or_else(|error| {
            panic!("case {case:?}: the record {record:?} is not the JSON the format is: {error}")
        });
        assert_eq!(
            rendered,
            json!({
                "marker": "proxy-access-log",
                "requestId": request_id,
                "httpMethod": row.verb,
                "path": row.path,
                "status": row.status.to_string(),
                "stage": row.stage,
                "resourcePath": row.resource_path,
            }),
            "case {case:?}: expected the stage's format resolved against this request, got \
             {record:?}"
        );
    }
}
