// SPDX-License-Identifier: CC0-1.0

//! A stack that deploys its REST API AGAIN really serves what the redeployment added.
//!
//! The rule under test: **a deployment that changes a resource's declaration applies that
//! change to the resource that already exists, through the update call the owning service
//! publishes for it — so adding a method to a deployed api, which declares another DEPLOYMENT
//! (a deployment is an immutable snapshot of the api's methods) and states it as the stage's
//! `DeploymentId`, re-points that stage at the new snapshot and the stage then SERVES the
//! method that was added.** Three crates have to agree for that to hold (CloudFormation
//! decides the change and makes the call, API Gateway applies it and then serves the stage's
//! data plane), so this half of the rule lives here and drives the emulator's own assembled
//! service set.
//!
//! The CONTROL-PLANE half — that each of the five records a REST API is built out of takes a
//! changed declaration in place, and what API Gateway's own read then reports — is pinned in
//! the crate that owns it, over a table spanning those five types:
//! `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/rest_api_redeployment.rs`.
//! What is here is the part that needs the whole emulator: an INVOKE of the deployed stage.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The stack statuses this suite waits on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The emulator as it is ASSEMBLED, on one engine: the api CloudFormation changes is the api
/// API Gateway serves.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Read one API Gateway control-plane record over its restJson1 wire.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Invoke the deployed stage at the api's own invoke URL, exactly as a client reaches AWS: the
/// request's host is `{rest-api-id}.execute-api.{region}.amazonaws.com` and its path is the
/// stage and the resource path.
async fn invoke(
    router: &MultiServiceRouter,
    api_id: &str,
    verb: &str,
    path: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(verb)
        .uri(format!("/{STAGE}{path}"))
        .header(
            "host",
            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .body(Bytes::from("{}"))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// A MOCK integration that answers `status` with `body`: a mock has no backend, so the
/// `statusCode` its rendered request template names is the integration response the call maps
/// to.
fn mock_integration(status: &str, body: &str) -> Value {
    json!({
        "Type": "MOCK",
        "RequestTemplates": { "application/json": format!(r#"{{"statusCode":{status}}}"#) },
        "IntegrationResponses": [{
            "StatusCode": status,
            "ResponseTemplates": { "application/json": body }
        }]
    })
}

/// The api this suite deploys, written out one `AWS::ApiGateway::*` resource at a time: the
/// api, one resource under its root, the methods `verbs` names on it (each with a mock
/// integration answering its own status), a deployment of them under the logical id
/// `deployment`, and the stage that serves it.
///
/// `deployment` is what a template deploying the api AGAIN changes: a new snapshot is a new
/// resource, which the stage then states — so the two templates a case deploys differ in the
/// methods declared and in the deployment the stage points at, and in nothing else.
fn template(stack: &str, deployment: &str, verbs: &[(&str, &str)]) -> String {
    let mut resources = json!({
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
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": deployment },
                "StageName": STAGE
            }
        }
    });
    let declarations = resources.as_object_mut().expect("an object");
    let mut methods: Vec<String> = Vec::new();
    for (verb, status) in verbs {
        let logical_id = format!("{verb}Items");
        methods.push(logical_id.clone());
        declarations.insert(
            logical_id,
            json!({
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Items" },
                    "HttpMethod": verb,
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": status }],
                    "Integration": mock_integration(status, &format!(r#"{{"answered":"{verb}"}}"#))
                }
            }),
        );
    }
    // The deployment DEPENDS ON every method: a deployment is the snapshot taken when it is
    // created, so a method declared beside it has to exist before that snapshot is taken to be
    // in it.
    declarations.insert(
        deployment.to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": methods,
            "Properties": { "RestApiId": { "Ref": "Api" } }
        }),
    );
    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "StageDeployment": { "Value": { "Ref": deployment } }
        }
    })
    .to_string()
}

/// Deploy `body` under `stack` with `action` and wait for the stack to reach `wanted`,
/// reporting the resource failures a stack that did not reach it recorded.
async fn deploy(
    router: &MultiServiceRouter,
    action: &str,
    stack: &str,
    body: &str,
    wanted: &str,
) -> String {
    let (status, answered) = cloudformation(
        router,
        action,
        &[("StackName", stack), ("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "{action} {stack:?}: expected HTTP 200, got {status} with body {answered}"
    );
    let (reached, described) = wait_for_stack_status(router, stack, wanted).await;
    let failures = if reached == wanted {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, wanted,
        "{action} {stack:?}: expected the stack to reach {wanted}, got {described} with resource \
         failures {failures:?}"
    );
    described
}

/// How long a deployed stage is given to answer at all, and how often to look: an invoke of a
/// stage the deployment has just re-pointed is answered by the api's own data plane, which the
/// stack operation's completion already establishes — so the loop is a short safety net on the
/// CONDITION rather than a wait for a span.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 400;

/// Invoke `verb path` until the stage answers `expected`, and answer the last status it gave
/// with the body that came with it.
async fn invoke_until(
    router: &MultiServiceRouter,
    api_id: &str,
    verb: &str,
    path: &str,
    expected: u16,
) -> (u16, String) {
    let mut last = invoke(router, api_id, verb, path).await;
    for _ in 0..POLLS {
        if last.0 == expected {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = invoke(router, api_id, verb, path).await;
    }
    last
}

/// Adding a method to a deployed api declares another deployment and re-points the stage at
/// it: the stack takes that change in place, API Gateway reports the stage pointing at the new
/// snapshot, and the stage SERVES the method the redeployment added.
#[tokio::test]
async fn a_redeployed_rest_api_serves_the_method_the_redeployment_added() {
    let router = build_router();
    let stack = "rest-api-redeploy-serves";
    let case = "a redeployed api with a method added";

    let first = template(stack, "DeploymentOne", &[("GET", "200")]);
    let described = deploy(&router, "CreateStack", stack, &first, CREATE_COMPLETE).await;
    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no ApiId in {described}"));
    let first_deployment = outputs.get("StageDeployment").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the stack published no StageDeployment in {described}")
    });

    // The method the first deployment snapshotted is served, and the one the second adds is
    // not there yet: a stage serves a SNAPSHOT.
    let (status, answered) = invoke_until(&router, &api_id, "GET", "/items", 200).await;
    assert_eq!(
        status, 200,
        "case \"the first deployment\": expected the stage to serve GET /items, got {status} \
         with body {answered}"
    );
    let (status, answered) = invoke(&router, &api_id, "POST", "/items").await;
    assert_ne!(
        status, 201,
        "case \"the first deployment\": expected the stage to have no POST /items yet, got \
         {status} with body {answered}"
    );

    // The same api with a method added, the deployment declared under ANOTHER logical id (a
    // new snapshot) and the stage stating that one.
    let second = template(
        stack,
        "DeploymentTwo",
        &[("GET", "200"), ("POST", "201")],
    );
    let described = deploy(&router, "UpdateStack", stack, &second, UPDATE_COMPLETE).await;
    let second_deployment = outputs_of(&described)
        .get("StageDeployment")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the updated stack published no StageDeployment in {described}")
        });
    assert_ne!(
        second_deployment, first_deployment,
        "case {case:?}: expected the redeployment to take a snapshot of its own, got the first \
         one back ({first_deployment})"
    );

    // The STAGE points at the new snapshot, as API Gateway itself reports it.
    let (status, reported) =
        apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetStage, got {status} with body {reported}"
    );
    assert_eq!(
        reported.get("deploymentId").and_then(Value::as_str),
        Some(second_deployment.as_str()),
        "case {case:?}: expected the stage to point at the new deployment \
         ({second_deployment}), got {reported}"
    );

    // ...and it SERVES the method the redeployment added, beside the one it already had, each
    // with its own integration's status.
    let (status, answered) = invoke_until(&router, &api_id, "POST", "/items", 201).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected the stage to serve the added POST /items with the \
         integration's own status, got {status} with body {answered}"
    );
    let (status, answered) = invoke_until(&router, &api_id, "GET", "/items", 200).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stage to go on serving GET /items, got {status} with body \
         {answered}"
    );
}
