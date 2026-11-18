// SPDX-License-Identifier: CC0-1.0

//! A stack UPDATE that FAILS leaves the REST API stage it deployed serving the deployment it
//! was serving before, and every request to that stage's invoke URL is answered.
//!
//! The rule under test: **a stack operation that cannot complete is rolled back to the
//! declarations that were in force before it, so every resource the failed operation touched
//! is left as it was — a deployed stage goes on pointing at the deployment it already served,
//! and a request to its invoke URL is answered with an HTTP status whichever way the operation
//! ended, rather than left unanswered.** Two crates have to agree for that to hold
//! (CloudFormation decides the change, makes the call the owning service publishes for it and
//! disposes of a failed update, while API Gateway refuses the change it cannot apply and then
//! serves the stage's data plane), so the suite lives here and drives the emulator's own
//! assembled service set.
//!
//! The rows differ along WHAT the update asks for that the owning service refuses, which is
//! what the rule generalises over: a CHANGED declaration of a resource that already exists
//! (the stage re-pointed at a deployment the api does not have — a patch value no record
//! answers to), and a NEW declaration whose create the service refuses (a child whose parent
//! resource does not resolve, and a base path mapping of a custom domain name the account does
//! not hold). Each failure is a different operation of the owning service and a different
//! refusal class, and all three have to end the same way for the rule to hold.
//!
//! Every expected value is computed from what the row declares — the api id and the deployment
//! the stage serves come out of the stack's own outputs, which is what a client deploying the
//! stack reads — never out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{blocks_of, elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The stack statuses this suite waits on: the one a create that went through reaches, and the
/// terminal one an update that failed is disposed of into.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The stage the api is deployed to, and the path and verb its one method serves.
const STAGE: &str = "live";
const PATH: &str = "/items";
const VERB: &str = "GET";

/// The status the deployed method's mock integration answers, and the body it renders: what the
/// stage served before the failed update is what it has to go on serving after it.
const SERVED_STATUS: u16 = 200;
const SERVED_BODY: &str = r#"{"answered":"the first deployment"}"#;

/// How long the stage is given to answer, and how often to look. The answer is owed from the
/// instant the stack operation ends, which the wait on the stack status already establishes, so
/// this is a BOUND on the condition rather than a wait for a span: a passing run costs nothing
/// by it, and a request that is never answered fails the case instead of hanging.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 400;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// api CloudFormation deploys is the api this suite then invokes.
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
async fn invoke(router: &MultiServiceRouter, api_id: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(VERB)
        .uri(format!("/{STAGE}{PATH}"))
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

/// Invoke the stage until it answers [`SERVED_STATUS`], answering the last status it gave with
/// the body that came with it.
async fn invoke_until_served(router: &MultiServiceRouter, api_id: &str) -> (u16, String) {
    let mut last = invoke(router, api_id).await;
    for _ in 0..POLLS {
        if last.0 == SERVED_STATUS {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = invoke(router, api_id).await;
    }
    last
}

/// A MOCK integration that answers [`SERVED_STATUS`] with [`SERVED_BODY`]: a mock has no
/// backend, so the `statusCode` its rendered request template names is the integration response
/// the call maps to.
fn mock_integration() -> Value {
    json!({
        "Type": "MOCK",
        "RequestTemplates": {
            "application/json": format!(r#"{{"statusCode":{SERVED_STATUS}}}"#)
        },
        "IntegrationResponses": [{
            "StatusCode": SERVED_STATUS.to_string(),
            "ResponseTemplates": { "application/json": SERVED_BODY }
        }]
    })
}

/// The api the stack deploys, written out one `AWS::ApiGateway::*` resource at a time: the api,
/// one resource under its root, the method the stage serves, a deployment of it and the stage
/// that serves that deployment.
///
/// `stage_deployment` is what the stage STATES as its `DeploymentId` — the logical id of the
/// deployment for a template that deploys, and a literal id for a template whose update asks
/// the stage to serve a deployment the api does not have. `extra` is whatever else the row's
/// update declares, which is nothing for the template that creates the stack.
fn template(stack: &str, stage_deployment: Value, extra: Value) -> String {
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
                "PathPart": PATH.trim_start_matches('/')
            }
        },
        "Method": {
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": { "Ref": "Items" },
                "HttpMethod": VERB,
                "AuthorizationType": "NONE",
                "MethodResponses": [{ "StatusCode": SERVED_STATUS.to_string() }],
                "Integration": mock_integration()
            }
        },
        // A deployment is the snapshot taken when it is created, so the method declared beside
        // it has to exist before that snapshot is taken to be in it.
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["Method"],
            "Properties": { "RestApiId": { "Ref": "Api" } }
        },
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": stage_deployment,
                "StageName": STAGE
            }
        }
    });
    let declarations = resources.as_object_mut().expect("an object");
    for (logical_id, declaration) in extra.as_object().into_iter().flatten() {
        declarations.insert(logical_id.clone(), declaration.clone());
    }
    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "StageDeployment": { "Value": { "Ref": "Deployment" } }
        }
    })
    .to_string()
}

/// The resource-failure reasons a stack recorded, for a case that did not end where it should.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The logical ids the stack recorded a resource FAILURE against: the declarations whose own
/// call the owning service refused, which is what says WHICH declaration of a row's update was
/// the one that could not be applied.
async fn failed_logical_ids(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    blocks_of(&events, "member")
        .into_iter()
        .filter(|event| {
            opt_element(event, "ResourceStatus")
                .is_some_and(|status| status.ends_with("_FAILED"))
        })
        .filter_map(|event| opt_element(event, "LogicalResourceId"))
        .collect()
}

/// One row: what the failing update asks for, beside the template that creates the stack.
struct Row {
    case: &'static str,
    /// What the updated template states as the stage's `DeploymentId`: the deployment's own
    /// logical id where the row's failure is elsewhere.
    stage_deployment: fn() -> Value,
    /// What else the updated template declares.
    extra: fn() -> Value,
    /// The logical id of the declaration the owning service refuses, which the stack has to
    /// record the failure against: what tells this row's refusal apart from the others', so a
    /// template that failed for some other reason cannot pass as this case.
    refused: &'static str,
}

/// The logical id of the deployment the deploying template takes, which is also what the stage
/// states while the row's failure is elsewhere.
fn declared_deployment() -> Value {
    json!({ "Ref": "Deployment" })
}

/// A deployment id of this service's own form that names no deployment of the api: the stage
/// cannot be re-pointed at it, so the update that states it fails.
fn absent_deployment() -> Value {
    json!("zzzzzzzzzz")
}

/// Nothing beyond what the deploying template declares.
fn nothing_more() -> Value {
    json!({})
}

/// A child resource under a parent id of this service's own form that names no resource of the
/// api: the create the update makes for it is refused.
fn resource_under_an_absent_parent() -> Value {
    json!({
        "Orphan": {
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ParentId": "zzzzzzzzzz",
                "PathPart": "orphan"
            }
        }
    })
}

/// A base path mapping of a custom domain name the account holds no record for: the create the
/// update makes for it is refused.
fn mapping_of_an_absent_domain_name() -> Value {
    json!({
        "Mapping": {
            "Type": "AWS::ApiGateway::BasePathMapping",
            "Properties": {
                "DomainName": "absent.example.org",
                "RestApiId": { "Ref": "Api" },
                "Stage": STAGE,
                "BasePath": "unmapped"
            }
        }
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "the stage re-pointed at a deployment the api does not have",
        stage_deployment: absent_deployment,
        extra: nothing_more,
        refused: "Stage",
    },
    Row {
        case: "a child resource declared under a parent that does not resolve",
        stage_deployment: declared_deployment,
        extra: resource_under_an_absent_parent,
        refused: "Orphan",
    },
    Row {
        case: "a base path mapping declared for a domain name the account does not hold",
        stage_deployment: declared_deployment,
        extra: mapping_of_an_absent_domain_name,
        refused: "Mapping",
    },
];

#[tokio::test]
async fn a_failed_stack_update_leaves_the_stage_serving_what_it_served() {
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("rest-api-rollback-{index}");

        // The stack as it is DEPLOYED: the stage serves the deployment the template declares.
        let deploying = template(&stack, declared_deployment(), nothing_more());
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &deploying)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached,
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described} with \
             resource failures {:?}",
            reported_failures(&router, &stack).await
        );
        let outputs = outputs_of(&described);
        let api_id = outputs.get("ApiId").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack published no ApiId in {described}")
        });
        let served_deployment = outputs.get("StageDeployment").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the stack published no StageDeployment in {described}")
        });

        // What the stage serves BEFORE the update, which is what the rule says survives it.
        let (status, answered) = invoke_until_served(&router, &api_id).await;
        assert_eq!(
            status, SERVED_STATUS,
            "case {case:?}: expected the deployed stage to serve {VERB} {PATH} with \
             {SERVED_STATUS}, got {status} with body {answered}"
        );
        assert!(
            answered.contains(SERVED_BODY),
            "case {case:?}: expected the first deployment's integration to render \
             {SERVED_BODY}, got {answered:?}"
        );

        // The same stack with the one change the owning service will refuse.
        let failing = template(&stack, (row.stage_deployment)(), (row.extra)());
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[("StackName", &stack), ("TemplateBody", &failing)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );

        // The failed update is DISPOSED of: the stack reaches a terminal status rather than
        // staying in progress.
        let (reached, described) =
            wait_for_stack_status(&router, &stack, UPDATE_ROLLBACK_COMPLETE).await;
        assert_eq!(
            reached,
            UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the failed update to settle at {UPDATE_ROLLBACK_COMPLETE}, \
             got {described} with resource failures {:?}",
            reported_failures(&router, &stack).await
        );

        // ...and it is THIS row's declaration that could not be applied: the stack records the
        // failure against the logical id the row names, so an update that failed for another
        // reason — or a path that refused every update of this template — is not this case.
        let failed = failed_logical_ids(&router, &stack).await;
        assert!(
            failed.iter().any(|logical_id| logical_id == row.refused),
            "case {case:?}: expected the refusal to be recorded against {}, got {failed:?} with \
             reasons {:?}",
            row.refused,
            reported_failures(&router, &stack).await
        );

        // The STAGE still points at the deployment it served, as API Gateway itself reports it.
        let (status, reported) =
            apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetStage, got {status} with body {reported}"
        );
        assert_eq!(
            reported.get("deploymentId").and_then(Value::as_str),
            Some(served_deployment.as_str()),
            "case {case:?}: expected the stage to still serve {served_deployment}, got {reported}"
        );

        // ...and the stage goes on SERVING it: the request is answered, with the status and the
        // body the deployment that is in force renders.
        let (status, answered) = invoke_until_served(&router, &api_id).await;
        assert_eq!(
            status, SERVED_STATUS,
            "case {case:?}: expected the stage to go on serving {VERB} {PATH} with \
             {SERVED_STATUS} after the failed update, got {status} with body {answered}"
        );
        assert!(
            answered.contains(SERVED_BODY),
            "case {case:?}: expected the deployment that is in force to render {SERVED_BODY} \
             after the failed update, got {answered:?}"
        );
    }
}
