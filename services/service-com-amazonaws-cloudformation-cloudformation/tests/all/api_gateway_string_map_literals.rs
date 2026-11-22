// SPDX-License-Identifier: CC0-1.0

//! What a template's NUMBER and BOOLEAN literals become where the API Gateway call that
//! creates the resource takes a map of string to STRING, read back through API Gateway itself.
//!
//! The rule every row here holds the service to: **a property value a template states as a
//! JSON number or boolean literal travels to the service that owns the resource as the TEXT
//! that literal spells, wherever that service's own member takes a string** — "all property
//! values in a template are strings", so `Variables: {build: 7}` configures the stage variable
//! `build` with `"7"` and the stack deploys, rather than being refused as a number where a
//! string was expected. The caller-keyed maps of these types that API Gateway takes as strings
//! are a stage's `variables`, an integration's `requestTemplates`, an integration response's
//! `responseTemplates` and the `requestModels`/`responseModels` a content type is read with
//! (`Stage$variables`, `Integration$requestTemplates`,
//! `IntegrationResponse$responseTemplates`, `Method$requestModels`,
//! `MethodResponse$responseModels`, each `MapOfStringToString` — `smithy_query(query="shape",
//! shape="Stage,Method,MethodResponse,Integration,IntegrationResponse")`).
//!
//! The rows differ along what the rule generalises over: which literal the template states (an
//! integer, a negative integer, a decimal, both booleans, and a string, which is already its
//! own text), how many entries the map carries, and which of those maps carries it — a stage's
//! own `Variables`, the `Variables` of the stage a deployment's `StageDescription` creates, and
//! an integration's `RequestTemplates`.
//!
//! The counter-half is asserted too: the two maps API Gateway takes as a map of string to
//! BOOLEAN (`Method$requestParameters`, `MethodResponse$responseParameters`) are NOT rendered
//! as text — a boolean literal stated for one of those travels as the boolean it is, which is
//! what makes the method's parameter required.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::stack_resource_provisioning::physical_id;
use crate::stack_lifecycle::{create_stack, router_with_engine, xml_text};

/// The api, one resource under its root and the MOCK method on it that a deployment can
/// snapshot, with `rest` merged into the template's `Resources`.
fn api_template(name: &str, method_extra: Value, rest: Value) -> String {
    let mut method = json!({
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
    });
    for (property, value) in method_extra.as_object().cloned().unwrap_or_default() {
        method["Properties"][property] = value;
    }
    let mut resources = json!({
        "Api": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": name,
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
        "Method": method
    });
    for (logical_id, declaration) in rest.as_object().cloned().unwrap_or_default() {
        resources[logical_id] = declaration;
    }
    json!({ "Resources": resources }).to_string()
}

/// The deployment of that api, and a stage of its own whose `Variables` is `variables`.
fn deployment_and_stage(stage: &str, variables: Value) -> Value {
    json!({
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["Method"],
            "Properties": { "RestApiId": { "Ref": "Api" } }
        },
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": stage,
                "Variables": variables
            }
        }
    })
}

/// One row of the table.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The stage whose variables are read back.
    stage: &'static str,
    /// The variables API Gateway must report for that stage: every value as its own text.
    variables: Vec<(&'static str, &'static str)>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a stage whose variables are stated as number and boolean literals",
            stack: "maplit-stage-variables",
            template: api_template(
                "maplit-stage-variables-api",
                json!({}),
                deployment_and_stage(
                    "v1",
                    json!({
                        "build": 7,
                        "offset": -3,
                        "ratio": 1.5,
                        "enabled": true,
                        "verbose": false,
                        "release": "2024-01"
                    }),
                ),
            ),
            stage: "v1",
            variables: vec![
                ("build", "7"),
                ("offset", "-3"),
                ("ratio", "1.5"),
                ("enabled", "true"),
                ("verbose", "false"),
                ("release", "2024-01"),
            ],
        },
        Row {
            case: "one number literal, in the variables of the stage a deployment's \
                   StageDescription creates",
            stack: "maplit-stage-description",
            template: api_template(
                "maplit-stage-description-api",
                json!({}),
                json!({
                    "Deployment": {
                        "Type": "AWS::ApiGateway::Deployment",
                        "DependsOn": ["Method"],
                        "Properties": {
                            "RestApiId": { "Ref": "Api" },
                            "StageName": "described",
                            "StageDescription": { "Variables": { "build": 42 } }
                        }
                    }
                }),
            ),
            stage: "described",
            variables: vec![("build", "42")],
        },
    ]
}

/// `GetStage` (`GET /restapis/{restApiId}/stages/{stageName}`).
async fn get_stage(router: &MultiServiceRouter, api_id: &str, stage: &str) -> (u16, Value) {
    apigateway(router, "GET", &format!("/restapis/{api_id}/stages/{stage}")).await
}

/// Deploy a row's template and assert the stack completed.
async fn deploy(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    template: &str,
) {
    let (code, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    let described = poll_to_terminal(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: a literal where the call takes a string must travel as its text, not \
         fail the stack, got {described}"
    );
}

#[tokio::test]
async fn a_literal_where_api_gateway_takes_a_string_travels_as_its_text() {
    let (router, engine) = router_with_engine();
    for row in rows() {
        let case = row.case;
        deploy(&router, &engine, case, row.stack, &row.template).await;
        let api_id = physical_id(&router, row.stack, "Api").await;
        let (code, document) = get_stage(&router, &api_id, row.stage).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected GetStage({api_id}, {:?}) to answer, got {code} with \
             {document}",
            row.stage
        );
        let reported = document.get("variables").cloned().unwrap_or(Value::Null);
        for (name, text) in &row.variables {
            assert_eq!(
                reported.get(*name).and_then(Value::as_str),
                Some(*text),
                "case {case:?}: expected the stage variable {name:?} to carry {text:?}, got \
                 {reported}"
            );
        }
    }
}

/// RULE (the counter-half): the maps API Gateway takes as a map of string to BOOLEAN are not
/// text — a boolean literal stated for a method's `RequestParameters` travels as the boolean
/// it is, which is what API Gateway reports back as the parameter's requiredness.
#[tokio::test]
async fn a_boolean_where_api_gateway_takes_a_boolean_travels_as_the_boolean() {
    let case = "a method's request parameters, which say whether a parameter is required";
    let (router, engine) = router_with_engine();
    let stack = "maplit-request-parameters";
    let template = api_template(
        "maplit-request-parameters-api",
        json!({
            "RequestParameters": {
                "method.request.querystring.page": true,
                "method.request.header.x-trace": false
            }
        }),
        json!({}),
    );
    deploy(&router, &engine, case, stack, &template).await;

    let api_id = physical_id(&router, stack, "Api").await;
    let resource_id = physical_id(&router, stack, "Items").await;
    let (code, document) = apigateway(
        &router,
        "GET",
        &format!("/restapis/{api_id}/resources/{resource_id}/methods/GET"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetMethod to answer, got {code} with {document}"
    );
    let reported = document
        .get("requestParameters")
        .cloned()
        .unwrap_or(Value::Null);
    for (parameter, required) in [
        ("method.request.querystring.page", true),
        ("method.request.header.x-trace", false),
    ] {
        assert_eq!(
            reported.get(parameter).and_then(Value::as_bool),
            Some(required),
            "case {case:?}: expected {parameter:?} to be reported as {required}, got {reported}"
        );
    }
}
