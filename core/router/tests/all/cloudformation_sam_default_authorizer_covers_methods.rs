// SPDX-License-Identifier: CC0-1.0

//! An AWS SAM api whose `Auth.DefaultAuthorizer` covers the methods its `DefinitionBody`
//! leaves uncovered, end to end: the stack deploys, and Amazon API Gateway holds each method
//! under the authorization the declaration asked for.
//!
//! The rule under test: **a SAM `AWS::Serverless::Api` whose `Auth.DefaultAuthorizer` names an
//! authorization applies it to every method of the api's definition that names no authorizer
//! of its own, and the api is created by IMPORTING that definition — so the method API Gateway
//! holds carries that authorization, and the deployed stage enforces it.** A method the
//! definition states its own requirement for keeps it, and one that states the EMPTY
//! requirement is deployed open.
//!
//! Why it matters: a template that relies on a default authorizer is the documented way to
//! secure a SAM api. A default that never reaches the deployed methods leaves every uncovered
//! route of the api serving anonymous callers, which is exactly the opposite of what the
//! declaration asked for — and nothing about the stack's own report says so.
//!
//! Two crates have to agree for that to hold (CloudFormation runs the transform and imports
//! the definition; API Gateway reads it, holds the methods and serves the stage), so the suite
//! lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: what the method already states (no
//! requirement at all, and the empty requirement that overrides the default), so the same
//! deployment is asserted to cover the one and leave the other open — and both are read back
//! through API Gateway's own `GetMethod` and then exercised on the deployed stage.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The macro the AWS Serverless Application Model is published as.
const TRANSFORM: &str = "AWS::Serverless-2016-10-31";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to, as the declaration names it.
const STAGE: &str = "live";

/// The `authorizationType` values the rows are stated in: the one whose caller signs the
/// request, and the open one.
const AWS_IAM: &str = "AWS_IAM";
const NONE: &str = "NONE";

/// The emulator as it is ASSEMBLED, on one engine: the stack's resources are the ones this
/// suite then reads and invokes through.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One CloudFormation call over its awsQuery wire.
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
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One API Gateway control-plane read over its `restJson1` wire.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// A request to the DEPLOYED stage, exactly as a client reaches AWS: the host is the api's own
/// execute-api endpoint and the path is the stage and the resource path. `signed` says whether
/// the call presents a SigV4 credential at all.
async fn invoke(
    router: &MultiServiceRouter,
    api_id: &str,
    path: &str,
    signed: bool,
) -> (u16, String) {
    let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}/{STAGE}{path}"))
        .header("host", host)
        .header("content-type", "application/json");
    if signed {
        builder = builder.header("authorization", authorization("execute-api"));
    }
    let request = builder.body(Bytes::new()).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One method of the deployed api: the path the definition declares it on, what the
/// definition says about its security, and the authorization API Gateway must hold it under.
struct Row {
    case: &'static str,
    path: &'static str,
    /// The `security` the definition states for the method: `None` for a method that states
    /// none at all, and `Some(json!([]))` for the empty requirement.
    security: Option<fn() -> Value>,
    authorization_type: &'static str,
}

/// The empty security requirement, which states that the method needs no authorization.
fn no_security() -> Value {
    json!([])
}

const ROWS: &[Row] = &[
    Row {
        case: "a method the definition names no authorizer for",
        path: "/covered",
        security: None,
        authorization_type: AWS_IAM,
    },
    Row {
        case: "a method that states the empty requirement over the default",
        path: "/open",
        security: Some(no_security),
        authorization_type: NONE,
    },
];

/// The mock integration every method of the definition carries, so the deployed stage really
/// answers the request it admits.
fn mock_integration() -> Value {
    json!({
        "type": "mock",
        "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        "responses": {
            "default": {
                "statusCode": "200",
                "responseTemplates": { "application/json": "{\"served\":true}" },
            },
        },
    })
}

/// The template the suite deploys: a SAM api whose definition declares the rows' methods and
/// whose `Auth` states the caller's own credential as the default authorizer.
fn template() -> String {
    let mut paths = json!({});
    for row in ROWS {
        let mut operation = json!({
            "responses": { "200": { "description": "ok" } },
            "x-amazon-apigateway-integration": mock_integration(),
        });
        if let (Some(security), Some(members)) = (row.security, operation.as_object_mut()) {
            members.insert("security".to_string(), security());
        }
        paths[row.path] = json!({ "get": operation });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Transform": TRANSFORM,
        "Resources": {
            "Web": {
                "Type": "AWS::Serverless::Api",
                "Properties": {
                    "StageName": STAGE,
                    "Auth": { "DefaultAuthorizer": AWS_IAM },
                    "DefinitionBody": {
                        "swagger": "2.0",
                        "info": { "title": "sam-default-authorizer", "version": "1.0" },
                        "paths": paths,
                    },
                },
            },
        },
        "Outputs": { "ApiId": { "Value": { "Ref": "Web" } } },
    })
    .to_string()
}

/// The id of the resource of `api` whose full path is `path`.
async fn resource_id(router: &MultiServiceRouter, api: &str, path: &str, case: &str) -> String {
    let (status, page) = apigateway(router, &format!("/restapis/{api}/resources")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResources must answer 200, got {status} {page}"
    );
    page.get("item")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("path").and_then(Value::as_str) == Some(path))
        })
        .and_then(|item| item.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: the api holds no resource at {path:?}: {page}"))
}

#[tokio::test]
async fn a_sam_default_authorizer_reaches_the_methods_api_gateway_holds() {
    let router = build_router();
    let stack = "sam-default-authorizer";
    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateStack must answer 200, got {status} with {created}"
    );
    let (settled, body) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "expected the SAM stack to deploy, got {settled:?} with {body}"
    );
    let api_id = outputs_of(&body)
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("the stack published no ApiId output in {body}"));

    for row in ROWS {
        // What API Gateway HOLDS for the method: its own read, which is what a client and
        // the stage's own data plane go on.
        let resource = resource_id(&router, &api_id, row.path, row.case).await;
        let (status, method) = apigateway(
            &router,
            &format!("/restapis/{api_id}/resources/{resource}/methods/GET"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: GetMethod must answer 200, got {status} {method}",
            row.case
        );
        assert_eq!(
            method.get("authorizationType").and_then(Value::as_str),
            Some(row.authorization_type),
            "case {:?}: expected the method to be held under {}, got {method}",
            row.case,
            row.authorization_type
        );

        // ...and what the DEPLOYED stage does with a call that presents no credential: the
        // method the default covers refuses it, and the one the definition left open serves
        // it. The two paths differ only in what the definition said about their security.
        let (status, served) = invoke(&router, &api_id, row.path, false).await;
        if row.authorization_type == AWS_IAM {
            assert_eq!(
                status, 403,
                "case {:?}: expected the covered method to refuse an unsigned call, got \
                 {status} {served}",
                row.case
            );
        } else {
            assert_eq!(
                status, 200,
                "case {:?}: expected the open method to serve an unsigned call, got {status} \
                 {served}",
                row.case
            );
        }
    }

    // The covered method admits a call that DOES present a credential, so what it enforces is
    // the authorization the declaration asked for rather than a route that answers nothing.
    let (status, served) = invoke(&router, &api_id, ROWS[0].path, true).await;
    assert_eq!(
        status, 200,
        "case \"a signed call to the covered method\": expected the stage to serve it, got \
         {status} {served}"
    );
}
