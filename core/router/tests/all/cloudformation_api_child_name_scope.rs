// SPDX-License-Identifier: CC0-1.0

//! Two declarations of one stack that state the same NAME for a child of two different
//! parents, and two that state it for one parent twice.
//!
//! The rule under test: **a physical resource name identifies one resource, but for a type
//! whose owning service addresses one of its resources only together with its PARENT, the name
//! identifies it only inside that parent — so two declarations of one stack stating the same
//! name under different parents describe two resources and CloudFormation creates both, while
//! two stating it under the same parent collide and the second fails in CloudFormation's own
//! words, `<name> already exists in stack <that stack's id>`.** CloudFormation decides it, and
//! the owning service is what holds the children afterwards, so the suite lives here rather
//! than in either crate.
//!
//! The rows differ along what the rule generalises over: the owning service's API version
//! (API Gateway's REST control plane and API Gateway v2), the kind of child (a stage, a data
//! model), and the property that names its parent (`RestApiId`, `ApiId`) — and, for each, both
//! sides of the rule: the same name under two parents, and under one parent twice. The
//! `$default` stage of an auto-deployed HTTP api is the row this matters most for: every HTTP
//! api in a template carries one, so a template with two apis states that one name twice.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The two stack statuses the two halves of the rule settle at.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The names the rows contest: the stage name every auto-deployed HTTP api is served on, the
/// stage name a REST deployment is released to, and a model's name.
const HTTP_STAGE: &str = "$default";
const REST_STAGE: &str = "prod";
const MODEL: &str = "Payload";

/// The content type a declared model is mapped to.
const JSON: &str = "application/json";

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: CloudFormation makes the creates and the
/// two API Gateway control planes hold what it created.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a call carries a credential scope so the
    // router can name the target service, and the signature itself is never checked.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One read of either API Gateway control plane over its `restJson1` wire: both sign as
/// `apigateway` and are told apart by the path they bind — `/restapis/…` for the REST control
/// plane, `/v2/…` for API Gateway v2.
async fn apigateway(router: &MultiServiceRouter, path: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::new(),
    )
    .await
}

/// A model's JSON schema, as a declaration carries it.
fn model_schema() -> Value {
    json!({
        "$schema": "http://json-schema.org/draft-04/schema#",
        "type": "object",
        "properties": { "id": { "type": "string" } }
    })
}

/// The declarations of one HTTP api and the `$default` stage served on it, under logical ids
/// suffixed with `suffix`.
fn http_api(resources: &mut Map<String, Value>, stack: &str, suffix: &str, api: &str) {
    resources.insert(
        format!("HttpApi{suffix}"),
        json!({
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": format!("{stack}-http-{suffix}"),
                "ProtocolType": "HTTP"
            }
        }),
    );
    resources.insert(
        format!("HttpStage{suffix}"),
        json!({
            "Type": "AWS::ApiGatewayV2::Stage",
            "Properties": {
                "ApiId": { "Ref": api },
                "StageName": HTTP_STAGE,
                "AutoDeploy": true
            }
        }),
    );
}

/// The declarations of one REST api, a method to deploy, the `prod` stage that deployment is
/// released to and a model named [`MODEL`], under logical ids suffixed with `suffix`.
fn rest_api(resources: &mut Map<String, Value>, stack: &str, suffix: &str, api: &str) {
    resources.insert(
        format!("RestApi{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{stack}-rest-{suffix}"),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }),
    );
    resources.insert(
        format!("RestResource{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": api },
                "ParentId": { "Fn::GetAtt": [api, "RootResourceId"] },
                "PathPart": "items"
            }
        }),
    );
    resources.insert(
        format!("RestMethod{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": api },
                "ResourceId": { "Ref": format!("RestResource{suffix}") },
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "Integration": { "Type": "MOCK" }
            }
        }),
    );
    resources.insert(
        format!("RestDeployment{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": [format!("RestMethod{suffix}")],
            "Properties": { "RestApiId": { "Ref": api } }
        }),
    );
    resources.insert(
        format!("RestStage{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": api },
                "DeploymentId": { "Ref": format!("RestDeployment{suffix}") },
                "StageName": REST_STAGE
            }
        }),
    );
    resources.insert(
        format!("Model{suffix}"),
        json!({
            "Type": "AWS::ApiGateway::Model",
            "Properties": {
                "RestApiId": { "Ref": api },
                "Name": MODEL,
                "ContentType": JSON,
                "Schema": model_schema()
            }
        }),
    );
}

/// The two logical-id suffixes the first test deploys each parent under.
const PARENTS: [&str; 2] = ["One", "Two"];

/// One row of the second test: a stack that states one child's name under ONE parent twice,
/// and the name the two declarations contest.
struct Contested {
    case: &'static str,
    /// The logical id of the declaration that fails, which is the one the deployment reaches
    /// second.
    contested: &'static str,
    template: fn(&str) -> String,
}

const CONTESTED: &[Contested] = &[
    Contested {
        case: "two $default stages of one HTTP api",
        contested: HTTP_STAGE,
        template: |stack| {
            let mut resources = Map::new();
            http_api(&mut resources, stack, "One", "HttpApiOne");
            // A SECOND stage of the api the first one is served on, under the same name.
            resources.insert(
                "HttpStageAgain".to_string(),
                json!({
                    "Type": "AWS::ApiGatewayV2::Stage",
                    "Properties": {
                        "ApiId": { "Ref": "HttpApiOne" },
                        "StageName": HTTP_STAGE,
                        "AutoDeploy": true
                    }
                }),
            );
            json!({ "Resources": Value::Object(resources) }).to_string()
        },
    },
    Contested {
        case: "two prod stages of one REST api",
        contested: REST_STAGE,
        template: |stack| {
            let mut resources = Map::new();
            rest_api(&mut resources, stack, "One", "RestApiOne");
            resources.insert(
                "RestStageAgain".to_string(),
                json!({
                    "Type": "AWS::ApiGateway::Stage",
                    "Properties": {
                        "RestApiId": { "Ref": "RestApiOne" },
                        "DeploymentId": { "Ref": "RestDeploymentOne" },
                        "StageName": REST_STAGE
                    }
                }),
            );
            json!({ "Resources": Value::Object(resources) }).to_string()
        },
    },
    Contested {
        case: "two models of one REST api under one name",
        contested: MODEL,
        template: |stack| {
            let mut resources = Map::new();
            rest_api(&mut resources, stack, "One", "RestApiOne");
            resources.insert(
                "ModelAgain".to_string(),
                json!({
                    "Type": "AWS::ApiGateway::Model",
                    "Properties": {
                        "RestApiId": { "Ref": "RestApiOne" },
                        "Name": MODEL,
                        "ContentType": "application/xml",
                        "Schema": model_schema()
                    }
                }),
            );
            json!({ "Resources": Value::Object(resources) }).to_string()
        },
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn one_name_under_two_parent_apis_is_two_children_and_both_are_created() {
    let router = emulator();
    let stack = "api-child-name-scope";

    let mut resources = Map::new();
    for suffix in PARENTS {
        http_api(&mut resources, stack, suffix, &format!("HttpApi{suffix}"));
        rest_api(&mut resources, stack, suffix, &format!("RestApi{suffix}"));
    }
    let template = json!({ "Resources": Value::Object(resources) }).to_string();

    let created = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );

    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    let failures = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let events = cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events.text(), "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "expected the stack to reach {CREATE_COMPLETE}: one name under two parents is two \
         children, got {described} with resource failures {failures:?}"
    );

    let resources =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&resources.text());

    for suffix in PARENTS {
        // Each HTTP api really holds the `$default` stage declared on it, which is what makes
        // the two declarations two stages rather than one contested name.
        let api_id = physical
            .get(&format!("HttpApi{suffix}"))
            .cloned()
            .unwrap_or_else(|| panic!("no physical id for HttpApi{suffix} in {physical:?}"));
        let case = format!("the $default stage of HTTP api {suffix}");
        let held = apigateway(&router, &format!("/v2/apis/{api_id}/stages/{HTTP_STAGE}")).await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: expected HTTP 200 from GetStage on {api_id}, got {} with {:?}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.json().get("stageName").and_then(Value::as_str),
            Some(HTTP_STAGE),
            "case {case:?}: expected the stage to report its own name, got {:?}",
            held.text()
        );
        assert_eq!(
            physical.get(&format!("HttpStage{suffix}")).map(String::as_str),
            Some(HTTP_STAGE),
            "case {case:?}: expected the stack to publish the stage's own name, got {physical:?}"
        );

        // And each REST api holds its own `prod` stage and its own model of the shared name.
        let rest_id = physical
            .get(&format!("RestApi{suffix}"))
            .cloned()
            .unwrap_or_else(|| panic!("no physical id for RestApi{suffix} in {physical:?}"));
        let case = format!("the prod stage of REST api {suffix}");
        let held = apigateway(&router, &format!("/restapis/{rest_id}/stages/{REST_STAGE}")).await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: expected HTTP 200 from GetStage on {rest_id}, got {} with {:?}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.json().get("stageName").and_then(Value::as_str),
            Some(REST_STAGE),
            "case {case:?}: expected the stage to report its own name, got {:?}",
            held.text()
        );
        let case = format!("the {MODEL} model of REST api {suffix}");
        let held = apigateway(&router, &format!("/restapis/{rest_id}/models/{MODEL}")).await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: expected HTTP 200 from GetModel on {rest_id}, got {} with {:?}",
            held.status,
            held.text()
        );
        assert_eq!(
            held.json().get("name").and_then(Value::as_str),
            Some(MODEL),
            "case {case:?}: expected the model to report its own name, got {:?}",
            held.text()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_name_under_one_parent_twice_still_collides() {
    let router = emulator();
    for (index, row) in CONTESTED.iter().enumerate() {
        let case = row.case;
        let stack = format!("api-child-name-contested-{index}");
        let template = (row.template)(&stack);
        let created = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );

        let (reached, described) = wait_for_stack_status(&router, &stack, ROLLBACK_COMPLETE).await;
        assert_eq!(
            reached, ROLLBACK_COMPLETE,
            "case {case:?}: expected the second declaration of {:?} to fail the stack, got \
             {described}",
            row.contested
        );
        let stack_id = element(&described, "StackId");
        let events = cloudformation(
            &router,
            "DescribeStackEvents",
            &[("StackName", &stack_id)],
        )
        .await;
        let reasons = elements(&events.text(), "ResourceStatusReason");
        // CloudFormation's own words, naming the contested name and the stack that holds it.
        let expected = format!("{} already exists in stack {stack_id}", row.contested);
        assert!(
            reasons.contains(&expected),
            "case {case:?}: expected a resource failure {expected:?}, got {reasons:?}"
        );
    }
}
