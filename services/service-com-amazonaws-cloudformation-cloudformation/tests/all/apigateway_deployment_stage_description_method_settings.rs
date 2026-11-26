// SPDX-License-Identifier: CC0-1.0

//! The per-method settings a deployment's `StageDescription` declares, end to end: the stack
//! deploys them onto the stage the `AWS::ApiGateway::Deployment` created, and API Gateway
//! reports each one under the METHOD PATH the declaration named it for.
//!
//! RULE under test: **a declared per-method setting is addressed by the method path
//! `{resource_path}/{http_method}` — the declaration's own `ResourcePath` with its INITIAL
//! separator dropped (the method path a key spells does not carry it) joined to its
//! `HttpMethod`, each half travelling as one reference token of the patch pointer, so a
//! separator INSIDE the path name rides as the escape `PatchOperation$path` requires of one —
//! so `Stage$methodSettings` keys the setting by the path the declaration states and not by a
//! re-spelling of it, whichever of the two writers of those settings (a stage's own
//! `MethodSettings` or the `MethodSettings` of a deployment's `StageDescription`) declared
//! it.**
//!
//! Why it matters: a client reading the per-method throttle or metrics configuration off a
//! CloudFormation-provisioned stage looks the method up by `{resource_path}/{http_method}`. A
//! stage that keyed the entry under another spelling answers nothing there, so an idempotent
//! re-apply or a drift check concludes the setting it declared was never applied.
//!
//! The rows differ along what the rule generalises over: the DEPTH of the resource path (the
//! root, a child, a nested one), whether the declaration states its inner separators plainly
//! or already encoded, the wildcard that stands for every method of the stage, and which
//! setting of the method the entry configures.
//!
//! The sibling suite `crate::apigateway_stage_method_settings` holds the same rule for the
//! other writer, an `AWS::ApiGateway::Stage`'s own `MethodSettings`.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::stack_lifecycle::{create_stack, router_with_engine, xml_text};
use crate::stack_resource_provisioning::physical_id;

/// The stage the deployment creates and configures.
const STAGE: &str = "described";

/// The throttle one row's entry states, which is read back under the row's key.
const BURST_LIMIT: i64 = 42;

/// One row: the `MethodSettings` entry a `StageDescription` declares, and the
/// `methodSettings` key API Gateway must report it under.
struct Row {
    case: &'static str,
    /// `MethodSetting$ResourcePath`, exactly as the declaration states it.
    resource_path: &'static str,
    /// `MethodSetting$HttpMethod`.
    http_method: &'static str,
    /// The method path the stage must key the entry by.
    key: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a child resource",
        resource_path: "/items",
        http_method: "GET",
        key: "items/GET",
    },
    Row {
        case: "a child resource stated with no initial separator",
        resource_path: "orders",
        http_method: "POST",
        key: "orders/POST",
    },
    Row {
        case: "a nested resource path, stated plainly",
        resource_path: "/items/{id}",
        http_method: "PUT",
        key: "items~1{id}/PUT",
    },
    Row {
        case: "a nested resource path, stated encoded as the type documents",
        resource_path: "/~1orders~1{id}",
        http_method: "DELETE",
        key: "orders~1{id}/DELETE",
    },
    Row {
        // No recording states what a declaration of the ROOT resource is keyed by; this row
        // pins the rule the recorded child case states, applied to it: the root carries no
        // path name behind its separator, so the resource half is empty.
        case: "the root resource",
        resource_path: "/",
        http_method: "HEAD",
        key: "/HEAD",
    },
    Row {
        case: "every method of the stage",
        resource_path: "/*",
        http_method: "*",
        key: "*/*",
    },
];

/// One `MethodSettings` entry as a row declares it: the two settings of a method this rule is
/// read back through.
fn method_setting(row: &Row) -> Value {
    json!({
        "ResourcePath": row.resource_path,
        "HttpMethod": row.http_method,
        "MetricsEnabled": true,
        "ThrottlingBurstLimit": BURST_LIMIT
    })
}

/// The stack this suite deploys: one api with a resource and a MOCK method on it, and the
/// deployment whose `StageDescription` creates the stage and carries one `MethodSettings`
/// entry per row.
fn template(stack: &str) -> String {
    let settings: Vec<Value> = ROWS.iter().map(method_setting).collect();
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "Items": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": "items"
                }
            },
            "GetItems": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Items" },
                    "HttpMethod": "GET",
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": {
                        "Type": "MOCK",
                        "RequestTemplates": { "application/json": "{\"statusCode\":200}" },
                        "IntegrationResponses": [{ "StatusCode": "200" }]
                    }
                }
            },
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["GetItems"],
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "StageName": STAGE,
                    "StageDescription": { "MethodSettings": settings }
                }
            }
        }
    })
    .to_string()
}

/// Deploy the stack and assert the create completed.
async fn deploy(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    document: &str,
) {
    let (status, body) = create_stack(router, stack, document, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status} {body}"
    );
    settle_the_store(router, engine).await;
    let described = poll_to_terminal(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the declared method settings to deploy, got {described}"
    );
}

#[tokio::test]
async fn a_stage_descriptions_method_setting_is_reported_under_its_method_path() {
    let case = "a deployment's StageDescription";
    let stack = "apigateway-stage-description-method-settings";
    let (router, engine) = router_with_engine();
    deploy(&router, &engine, case, stack, &template(stack)).await;
    let api_id = physical_id(&router, stack, "Api").await;

    let (status, stage) = apigateway(
        &router,
        "GET",
        &format!("/restapis/{api_id}/stages/{STAGE}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected API Gateway to report the stage, got {status} {stage}"
    );

    let settings = stage["methodSettings"].clone();
    for row in ROWS {
        let held = &settings[row.key];
        assert!(
            held.is_object(),
            "case {:?}: expected the settings under the method path {:?}, got {settings}",
            row.case,
            row.key
        );
        assert_eq!(
            held["metricsEnabled"],
            json!(true),
            "case {:?}: expected metricsEnabled under {:?}, got {held}",
            row.case,
            row.key
        );
        assert_eq!(
            held["throttlingBurstLimit"],
            json!(BURST_LIMIT),
            "case {:?}: expected the declared burst limit under {:?}, got {held}",
            row.case,
            row.key
        );
    }

    // And the stage carries exactly those entries: no key is a re-spelling of a path the
    // declaration never stated.
    let mut reported: Vec<String> = settings
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    reported.sort();
    let mut expected: Vec<String> = ROWS.iter().map(|row| row.key.to_string()).collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the method paths {expected:?}, got {reported:?}"
    );
}
