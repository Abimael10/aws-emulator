// SPDX-License-Identifier: CC0-1.0

//! WHICH PATH a stage's access-log record names for the request it answered, and how that
//! path differs from the resource template the request matched.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `$context.path` is the path the API's OWN endpoint carries for the request — the stage
//! > segment followed by the path within the API the client asked for ("for a non-proxy
//! > request URL of
//! > `https://{rest-api-id}.execute-api.{region}.amazonaws.com/{stage-name}/root/child`, the
//! > `$context.path` value is `/{stage-name}/root/child`"). It is the path ASKED FOR, so a
//! > parameterised resource's record carries the value the caller sent where
//! > `$context.resourcePath` carries the template, and a query string is no part of it. It
//! > carries NOTHING of the endpoint the client happened to reach this emulator by: a client
//! > that cannot resolve the API's own invoke host reaches the same stage through the
//! > reserved data-plane path prefix, and both read one value for one request.
//!
//! The same path is what a PROXY integration's event reports under `requestContext.path`, so
//! the two cannot name one request's path two ways.
//!
//! Only a cross-service suite can pin this: the api, its stage and the logging configuration
//! are API Gateway's, the role the account logs under is AWS IAM's, and the group a record
//! lands in is CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs`
//! mounts them. Every record is read back through CloudWatch Logs' own `FilterLogEvents`, so
//! a record that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the ADDRESSING (the api's own invoke
//! host and the reserved data-plane prefix), the depth of the path (the stage root, one
//! segment, a nested one), whether the matched resource is PARAMETERISED (where the path and
//! the template differ), whether the request carried a QUERY STRING, and WHICH STAGE served
//! it.

use serde_json::{json, Value};

use crate::account_logging_role::{create_role, replace_role, role_arn, trusting};
use crate::execute_api_data_plane::{invoke_on_host, mock_method, mock_resource};
use crate::rest_api_lifecycle::{
    create_api, create_resource, gateway_with_iam_and_logs, string_member, Gateway,
};
use crate::stage_access_logging::{create_log_group, delivered};

/// The Region every ARN below names and the api's own invoke host is addressed in.
const REGION: &str = "us-east-1";

/// The two stages the api is deployed to, so WHICH stage served a request is a row rather
/// than a constant of the suite.
const LIVE: &str = "live";
const PROD: &str = "prod";

/// The emulator's own endpoint, as a client on this machine reaches it: a loopback address,
/// which names no service to the front door, so such a client addresses the data plane by the
/// reserved path prefix below instead of by host.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved path prefix that names this data plane ahead of the api id, which is the
/// addressing a client that cannot resolve the api's own invoke host uses. No part of it is
/// part of the path the api serves, so no record may carry it.
const DATA_PLANE_PREFIX: &str = "/_aws/execute-api";

/// The role the account writes its logs as, and the principal its trust policy has to admit.
const ROLE_NAME: &str = "apigw-access-log-path";
const ROLE_PATH: &str = "/service-role/";
const SERVICE_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The format every stage below renders its records by: the two paths beside the stage, the
/// verb and the status, so one record answers both halves of the rule.
const FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","httpMethod":"$context.httpMethod","#,
    r#""path":"$context.path","resourcePath":"$context.resourcePath","#,
    r#""stage":"$context.stage","status":"$context.status"}"#
);

/// How a row reaches the stage.
#[derive(Clone, Copy, Debug)]
enum Addressing {
    /// The api's own invoke host, `{api-id}.execute-api.{region}.amazonaws.com`, whose whole
    /// path space is this api's invoke path.
    InvokeHost,
    /// The emulator's own endpoint, with the api named in the path ahead of the stage.
    DataPlanePrefix,
}

/// One row: how the request was addressed, what it asked for, and what the record has to
/// name.
struct Row {
    case: &'static str,
    addressing: Addressing,
    stage: &'static str,
    /// The group this row's record lands in — one per row, so the count asserted is this
    /// row's own.
    group: &'static str,
    /// What the client asked for within the stage, query string included.
    asked_for: &'static str,
    /// The `$context.path` the record has to carry: the stage and the path asked for.
    path: &'static str,
    /// The `$context.resourcePath` the record has to carry: the template that matched.
    resource_path: &'static str,
}

/// The rows: each addressing crossed with the shapes of a path, the parameterised resource,
/// a query string and the second stage.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one segment, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: LIVE,
            group: "/apigw/path/host-one-segment",
            asked_for: "/ping",
            path: "/live/ping",
            resource_path: "/ping",
        },
        Row {
            case: "one segment, through the reserved data-plane prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: LIVE,
            group: "/apigw/path/prefix-one-segment",
            asked_for: "/ping",
            path: "/live/ping",
            resource_path: "/ping",
        },
        Row {
            case: "a nested resource, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: LIVE,
            group: "/apigw/path/host-nested",
            asked_for: "/shop/items",
            path: "/live/shop/items",
            resource_path: "/shop/items",
        },
        Row {
            case: "a nested resource, through the reserved data-plane prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: LIVE,
            group: "/apigw/path/prefix-nested",
            asked_for: "/shop/items",
            path: "/live/shop/items",
            resource_path: "/shop/items",
        },
        Row {
            case: "a parameterised resource, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: LIVE,
            group: "/apigw/path/host-parameter",
            asked_for: "/things/7",
            path: "/live/things/7",
            resource_path: "/things/{id}",
        },
        Row {
            case: "a parameterised resource, through the reserved data-plane prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: LIVE,
            group: "/apigw/path/prefix-parameter",
            asked_for: "/things/other-value",
            path: "/live/things/other-value",
            resource_path: "/things/{id}",
        },
        Row {
            case: "a query string the path does not carry, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: LIVE,
            group: "/apigw/path/host-query",
            asked_for: "/ping?id=7&id=8",
            path: "/live/ping",
            resource_path: "/ping",
        },
        Row {
            case: "a query string the path does not carry, through the reserved prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: LIVE,
            group: "/apigw/path/prefix-query",
            asked_for: "/ping?id=7",
            path: "/live/ping",
            resource_path: "/ping",
        },
        Row {
            case: "the stage root, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: LIVE,
            group: "/apigw/path/host-root",
            asked_for: "/",
            path: "/live/",
            resource_path: "/",
        },
        Row {
            case: "the stage root, through the reserved data-plane prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: LIVE,
            group: "/apigw/path/prefix-root",
            asked_for: "/",
            path: "/live/",
            resource_path: "/",
        },
        Row {
            case: "the other stage of the same api, on the api's own invoke host",
            addressing: Addressing::InvokeHost,
            stage: PROD,
            group: "/apigw/path/host-other-stage",
            asked_for: "/shop/items",
            path: "/prod/shop/items",
            resource_path: "/shop/items",
        },
        Row {
            case: "the other stage of the same api, through the reserved data-plane prefix",
            addressing: Addressing::DataPlanePrefix,
            stage: PROD,
            group: "/apigw/path/prefix-other-stage",
            asked_for: "/things/42",
            path: "/prod/things/42",
            resource_path: "/things/{id}",
        },
    ]
}

/// The ARN of the log group `name`, as a stage's `destinationArn` names one.
fn destination_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{name}",
        account = engine::emulator_account_id(),
    )
}

/// The api's own invoke host, `{api-id}.execute-api.{region}.amazonaws.com`.
fn invoke_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// Install the account's CloudWatch role through AWS IAM's own `CreateRole` and API Gateway's
/// own `UpdateAccount`: without it there is nothing for API Gateway to write the account's
/// logs as.
async fn install_account_role(gateway: &Gateway) {
    create_role(gateway, ROLE_NAME, ROLE_PATH, &trusting(SERVICE_PRINCIPAL)).await;
    let installed = replace_role(gateway, &role_arn(ROLE_PATH, ROLE_NAME)).await;
    assert_eq!(
        installed.status, 200,
        "setup UpdateAccount /cloudwatchRoleArn: expected 200, got {} with {:?}",
        installed.status, installed.body
    );
}

/// Point `stage`'s access logging at `group`, rendered by [`FORMAT`], as a client configures
/// it.
async fn configure_access_logging(
    gateway: &Gateway,
    api: &str,
    stage: &str,
    case: &str,
    group: &str,
) {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": "/accessLogSettings/destinationArn",
                        "value": destination_arn(group),
                    },
                    { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
                ],
            })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateStage to store the logging configuration, got {} with \
         {:?}",
        answer.status, answer.body
    );
}

/// `CreateDeployment` of `api` to `stage`.
async fn deploy(gateway: &Gateway, api: &str, stage: &str) {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": stage })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "setup CreateDeployment({stage}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
}

/// The whole setup: an api whose root, a one-segment resource, a nested one and a
/// parameterised one each carry a `MOCK` `GET` — the one integration type that needs no
/// backend, so what is logged is the stage's own doing — deployed to both stages. Answers the
/// api id.
async fn deployed(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "access-log-request-path").await;
    mock_method(gateway, &api, &root, "GET", "200", json_body(r#"{"at":"root"}"#)).await;
    mock_resource(
        gateway,
        &api,
        &root,
        "ping",
        "GET",
        "200",
        json_body(r#"{"at":"ping"}"#),
    )
    .await;
    let shop = create_resource(gateway, &api, &root, "shop").await;
    let shop = string_member(&shop, "id", "CreateResource(shop)");
    mock_resource(
        gateway,
        &api,
        &shop,
        "items",
        "GET",
        "200",
        json_body(r#"{"at":"items"}"#),
    )
    .await;
    let things = create_resource(gateway, &api, &root, "things").await;
    let things = string_member(&things, "id", "CreateResource(things)");
    mock_resource(
        gateway,
        &api,
        &things,
        "{id}",
        "GET",
        "200",
        json_body(r#"{"at":"one thing"}"#),
    )
    .await;
    for stage in [LIVE, PROD] {
        deploy(gateway, &api, stage).await;
    }
    api
}

/// A response-template map answering `body` to `application/json` callers.
fn json_body(body: &str) -> Value {
    json!({ "application/json": body })
}

/// The ONE record `group` holds, as a client reading the destination back sees it.
async fn only_record(gateway: &Gateway, case: &str, group: &str) -> Value {
    let records = delivered(&gateway.router, group).await.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected API Gateway to have delivered into {group:?}, but \
             CloudWatch Logs reports no such group at all"
        )
    });
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected exactly one record in {group:?}, got {records:?}"
    );
    assert!(
        !records[0].contains(DATA_PLANE_PREFIX),
        "case {case:?}: expected no part of the emulator's own addressing in the record, got \
         {:?}",
        records[0]
    );
    serde_json::from_str(&records[0]).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the rendered record to be the configured JSON document, \
             got {:?} ({error})",
            records[0]
        )
    })
}

/// The member `name` of a rendered record, as text.
fn member(case: &str, record: &Value, name: &str) -> String {
    record
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the record to carry {name:?}, got {record:?}")
        })
        .to_string()
}

/// RULE: the record names the path the API's own endpoint carries — the stage and the path
/// asked for — whatever addressing the client reached this emulator by, with the matched
/// template reported separately.
#[tokio::test]
async fn the_record_names_the_stage_qualified_path_the_request_asked_for() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed(&gateway).await;

    for row in rows() {
        let case = row.case;
        // The destination is a log group of the account's: it is created before the stage is
        // pointed at it, because API Gateway delivers into a group that is there.
        create_log_group(&gateway.router, row.group).await;
        configure_access_logging(&gateway, &api, row.stage, case, row.group).await;

        let (host, path) = match row.addressing {
            Addressing::InvokeHost => (
                invoke_host(&api),
                format!("/{}{}", row.stage, row.asked_for),
            ),
            Addressing::DataPlanePrefix => (
                OWN_ENDPOINT.to_string(),
                format!(
                    "{DATA_PLANE_PREFIX}/{api}/{stage}{asked}",
                    stage = row.stage,
                    asked = row.asked_for
                ),
            ),
        };
        let answered = invoke_on_host(&gateway, &host, "GET", &path, &[]).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the stage to serve {path:?}, got {} with {:?}",
            answered.status, answered.body
        );

        let record = only_record(&gateway, case, row.group).await;
        let got = member(case, &record, "path");
        assert_eq!(
            got, row.path,
            "case {case:?}: expected the record's path {:?}, got {got:?}",
            row.path
        );
        let got = member(case, &record, "resourcePath");
        assert_eq!(
            got, row.resource_path,
            "case {case:?}: expected the record's resourcePath {:?}, got {got:?}",
            row.resource_path
        );
        let got = member(case, &record, "stage");
        assert_eq!(
            got, row.stage,
            "case {case:?}: expected the record's stage {:?}, got {got:?}",
            row.stage
        );
        let got = member(case, &record, "status");
        assert_eq!(
            got, "200",
            "case {case:?}: expected the record's status \"200\", got {got:?}"
        );
        let got = member(case, &record, "httpMethod");
        assert_eq!(
            got, "GET",
            "case {case:?}: expected the record's httpMethod \"GET\", got {got:?}"
        );
        let got = member(case, &record, "requestId");
        assert!(
            !got.is_empty(),
            "case {case:?}: expected the record to name the request it is about, got {got:?}"
        );
    }
}

/// RULE, read through the other consumer of those same values: a MAPPING TEMPLATE of the
/// call renders from the one `$context` map the access-log record is rendered from, so a
/// template and a record cannot name one request's path two ways — through either
/// addressing. (The `requestContext.path` a PROXY integration's event carries is that same
/// path, built from the same reading.)
#[tokio::test]
async fn a_mapping_template_of_the_call_renders_that_same_path() {
    let gateway = gateway_with_iam_and_logs();
    let (api, root) = create_api(&gateway, "access-log-template-path").await;
    // A `MOCK` integration whose response template is the request's own `$context`: the one
    // integration type that needs no backend, used here to read back what the serving path
    // resolved for the call.
    let echoed = r#"{"path":"$context.path","resourcePath":"$context.resourcePath"}"#;
    mock_resource(&gateway, &api, &root, "echo", "GET", "200", json_body(echoed)).await;
    deploy(&gateway, &api, LIVE).await;

    for (case, host, path) in [
        (
            "on the api's own invoke host",
            invoke_host(&api),
            format!("/{LIVE}/echo"),
        ),
        (
            "through the reserved data-plane prefix",
            OWN_ENDPOINT.to_string(),
            format!("{DATA_PLANE_PREFIX}/{api}/{LIVE}/echo"),
        ),
    ] {
        let answered = invoke_on_host(&gateway, &host, "GET", &path, &[]).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the stage to serve {path:?}, got {} with {:?}",
            answered.status, answered.body
        );
        let reported: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected the mapped response to be JSON, got {:?} ({error})",
                answered.body
            )
        });
        let got = member(case, &reported, "path");
        assert_eq!(
            got,
            format!("/{LIVE}/echo"),
            "case {case:?}: expected the stage-qualified path \"/{LIVE}/echo\", got {got:?}"
        );
        let got = member(case, &reported, "resourcePath");
        assert_eq!(
            got, "/echo",
            "case {case:?}: expected the matched template \"/echo\", got {got:?}"
        );
    }
}
