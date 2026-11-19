// SPDX-License-Identifier: CC0-1.0

//! The per-method settings an `AWS::ApiGateway::Stage` declares, end to end: the stack
//! deploys the stage into Amazon API Gateway, and that service reports each setting under the
//! METHOD PATH the declaration named it for.
//!
//! RULE under test: **a `MethodSettings` entry configures the method its `ResourcePath` and
//! `HttpMethod` name, and `Stage$methodSettings` keys what it configured by that method path
//! — "method paths defined as {resource_path}/{http_method} for an individual method
//! override, or /\*/\* for overriding all methods in the stage" — the resource path
//! without its leading separator, every other separator of it literal, then the separator
//! before the HTTP method. The `~1` escape belongs to the patch POINTER that addresses the
//! entry, not to the key. The key is therefore the one API Gateway reports for that method
//! whichever way the declaration stated the path: `MethodSetting$ResourcePath` may state it
//! already pointer-encoded, its initial slash included, or with its separators plain, and
//! both state the one method.**
//!
//! Why it matters: a client reading per-method cache or logging settings off a
//! CloudFormation-provisioned stage looks the method up. A stage that keyed the entry by the
//! spelling the declaration happened to use answers nothing under the documented key, so the
//! client concludes the setting it declared is unconfigured — and so does the stage's data
//! plane, which resolves a method's settings by the same key.
//!
//! The rows differ along what the rule generalises over: the DEPTH of the resource path (the
//! root, a child, a nested one), whether the declaration states its inner separators plainly
//! or already encoded, the wildcard that stands for every method of the stage, and which
//! setting of the method the entry configures.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::stack_lifecycle::{deploy, describe_stack, output_value, router};

/// The stage every template here deploys to.
const STAGE: &str = "live";

/// One row: the `MethodSettings` entry a stage declares, and the `methodSettings` key API
/// Gateway must report it under.
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
        resource_path: "/cached",
        http_method: "GET",
        key: "cached/GET",
    },
    Row {
        case: "a nested resource path, stated plainly",
        resource_path: "/items/{id}",
        http_method: "PUT",
        key: "items/{id}/PUT",
    },
    Row {
        case: "a nested resource path, stated encoded as the type documents",
        resource_path: "/~1orders~1{id}",
        http_method: "DELETE",
        key: "orders/{id}/DELETE",
    },
    Row {
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

/// The settings every row's entry carries: the cache configuration the recorded template
/// declares, which is what a client reads back by method path.
const CACHE_TTL_SECONDS: i64 = 300;

/// One `MethodSettings` entry as a row declares it.
fn method_setting(row: &Row) -> Value {
    json!({
        "ResourcePath": row.resource_path,
        "HttpMethod": row.http_method,
        "CachingEnabled": true,
        "CacheTtlInSeconds": CACHE_TTL_SECONDS,
        "MetricsEnabled": true
    })
}

/// The stack this suite deploys: one api with a resource and a method on it, a deployment of
/// them, and the stage that serves it — carrying one `MethodSettings` entry per row.
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
            "Cached": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": "cached"
                }
            },
            "GetCached": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Cached" },
                    "HttpMethod": "GET",
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": {
                        "Type": "MOCK",
                        "RequestTemplates": { "application/json": "{\"statusCode\":200}" },
                        "IntegrationResponses": [{
                            "StatusCode": "200",
                            "ResponseTemplates": { "application/json": "{\"cached\":true}" }
                        }]
                    }
                }
            },
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["GetCached"],
                "Properties": { "RestApiId": { "Ref": "Api" } }
            },
            "Stage": {
                "Type": "AWS::ApiGateway::Stage",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "DeploymentId": { "Ref": "Deployment" },
                    "StageName": STAGE,
                    "CacheClusterEnabled": true,
                    "CacheClusterSize": "0.5",
                    "MethodSettings": settings
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } }
        }
    })
    .to_string()
}

/// The physical id of the api the stack created, as the stack published it.
async fn deployed_api(router: &MultiServiceRouter, stack: &str) -> String {
    let (status, body) = describe_stack(router, stack).await;
    assert_eq!(
        status, 200,
        "case {stack:?}: DescribeStacks answered {status} with {body}"
    );
    output_value(&body, "ApiId").unwrap_or_else(|| {
        panic!("case {stack:?}: the stack published no ApiId output in {body}")
    })
}

#[tokio::test]
async fn a_declared_method_setting_is_reported_under_its_method_path() {
    let stack = "apigateway-stage-method-settings";
    let router = router();
    deploy(&router, stack, &template(stack)).await;
    let api_id = deployed_api(&router, stack).await;

    let (status, stage) = apigateway(
        &router,
        "GET",
        &format!("/restapis/{api_id}/stages/{STAGE}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"GetStage\": expected API Gateway to report the stage, got {status} {stage}"
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
            held["cachingEnabled"],
            json!(true),
            "case {:?}: expected cachingEnabled under {:?}, got {held}",
            row.case,
            row.key
        );
        assert_eq!(
            held["cacheTtlInSeconds"],
            json!(CACHE_TTL_SECONDS),
            "case {:?}: expected the declared TTL under {:?}, got {held}",
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
    }

    // And the stage carries exactly those entries: two declarations of one method are one
    // entry, and no key is the spelling a declaration happened to state the path in.
    let mut reported: Vec<String> = settings
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    reported.sort();
    let mut expected: Vec<String> = ROWS.iter().map(|row| row.key.to_string()).collect();
    expected.sort();
    assert_eq!(
        reported, expected,
        "case \"every declared entry\": expected the method paths {expected:?}, got {reported:?}"
    );

    // The stage the stack deployed is the one that was read: its own name comes back from
    // that service, not from the stack's records.
    assert_eq!(
        stage["stageName"].as_str(),
        Some(STAGE),
        "case \"GetStage\": expected the declared stage name, got {stage}"
    );
}
