// SPDX-License-Identifier: CC0-1.0

//! A stack that deploys its REST API AGAIN applies the change to the api API Gateway already
//! holds, rather than failing the update.
//!
//! The rule under test: **a deployment that changes a resource's declaration applies that
//! change to the resource that already exists, through the update call the owning service
//! publishes for it — so every record a REST API is built out of (the api, a resource of it, a
//! method on that resource, a deployment of them and the stage that serves one) takes a
//! changed declaration in place, the stack reaches `UPDATE_COMPLETE`, and the configuration
//! that service reports back is the one the new template states.**
//!
//! The headline case is the one a redeployed api turns on: adding a method to an api declares
//! another DEPLOYMENT (a deployment is an immutable snapshot of the api's methods) and states
//! it as the stage's `DeploymentId`, so the stage has to be re-pointed at the new snapshot —
//! which is one `replace` of `/deploymentId` — and API Gateway then reports it pointing there.
//!
//! The rows of the table below differ along what the rule generalises over: the resource TYPE
//! whose declaration changed, and the SHAPE of the property that changed — a scalar the patch
//! call replaces, a collection the caller keys (a stage variable) and an identity the owning
//! service assigned (the deployment a stage points at). Every expected value is computed here
//! from what the template states and read back through API Gateway's own reads, never out of
//! the stack's own records.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, router_with_engine, xml_text, xml_texts, REGION};

/// The stack statuses this suite waits on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The stage every template here deploys to.
const STAGE: &str = "live";

/// How long to wait between polls of a stack's status, and how long to keep looking before
/// giving up. Generous: the wait is on the CONDITION, not on a span.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

/// Read one API Gateway control-plane record over its restJson1 wire: that service signs as
/// `apigateway` and binds every REST operation under `/restapis/…`.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
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

/// What one template of this suite declares. Every field is a value some case STATES
/// differently, so the two templates a case deploys differ in exactly what that case is about.
struct Declared<'d> {
    stack: &'d str,
    /// The logical id the deployment is declared under: a template that deploys the api again
    /// declares ANOTHER snapshot, which is another resource.
    deployment: &'d str,
    api_description: &'d str,
    path_part: &'d str,
    operation: &'d str,
    deployment_description: &'d str,
    stage_description: &'d str,
    variables: Value,
    /// The resources the case adds beside those five.
    extra: Vec<(&'d str, Value)>,
}

/// The api a case deploys, written out one `AWS::ApiGateway::*` resource at a time: the api,
/// one resource under its root, one method on it with a mock integration, a deployment of them
/// and the stage that serves it.
fn template(declared: &Declared<'_>) -> String {
    let mut resources = json!({
        "Api": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{}-api", declared.stack),
                "Description": declared.api_description,
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        },
        "Items": {
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                "PathPart": declared.path_part
            }
        },
        "GetItems": {
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": { "Ref": "Items" },
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "OperationName": declared.operation,
                "MethodResponses": [{ "StatusCode": "200" }],
                "Integration": mock_integration("200", r#"{"items":[]}"#)
            }
        },
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": declared.deployment },
                "StageName": STAGE,
                "Description": declared.stage_description,
                "Variables": declared.variables
            }
        }
    });
    let declarations = resources.as_object_mut().expect("an object");
    // The deployment is declared under the logical id the case states, which is what a template
    // deploying the api AGAIN changes, so it is inserted by name rather than written into the
    // document above. It DEPENDS ON every method of the api: a deployment is the snapshot taken
    // when it is created, so a method declared beside it has to exist before that snapshot is
    // taken to be in it.
    let mut depends_on: Vec<&str> = vec!["GetItems"];
    depends_on.extend(declared.extra.iter().map(|(logical_id, _)| *logical_id));
    declarations.insert(
        declared.deployment.to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": depends_on,
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "Description": declared.deployment_description
            }
        }),
    );
    for (logical_id, resource) in &declared.extra {
        declarations.insert((*logical_id).to_string(), resource.clone());
    }
    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "StageDeployment": { "Value": { "Ref": declared.deployment } }
        }
    })
    .to_string()
}

/// The `StackStatus` of `stack` right now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {code} with body {body}"
    );
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Deploy `body` under `stack` with `action` and wait until the stack first reports `wanted`,
/// reporting the resource failures a stack that did not reach it recorded.
///
/// The wait is on that CONDITION — the operation concludes when the owning services have
/// answered — so nothing here sleeps for a span.
async fn deploy(
    router: &MultiServiceRouter,
    action: &str,
    stack: &str,
    body: &str,
    wanted: &str,
) -> String {
    let (code, answered) = call(
        router,
        action,
        &[member("StackName", stack), member("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        code, 200,
        "{action} {stack:?}: expected HTTP 200, got {code} with body {answered}"
    );
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = stack_status(router, stack).await;
    while last.0 != wanted && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    if last.0 != wanted {
        let (_, events) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
        let failures = xml_texts(&events, "ResourceStatusReason");
        panic!(
            "{action} {stack:?}: expected the stack to reach {wanted}, got {} with resource \
             failures {failures:?}",
            last.1
        );
    }
    last.1
}

/// The `Outputs` the described stack published, keyed by output name.
fn outputs(described: &str) -> BTreeMap<String, String> {
    xml_texts(described, "OutputKey")
        .into_iter()
        .zip(xml_texts(described, "OutputValue"))
        .collect()
}

/// The physical id of each resource the described stack holds, keyed by logical id.
fn physical_ids(described: &str) -> BTreeMap<String, String> {
    xml_texts(described, "LogicalResourceId")
        .into_iter()
        .zip(xml_texts(described, "PhysicalResourceId"))
        .collect()
}

/// The output `name` the stack published, or a panic naming what was read instead.
fn output(described: &str, name: &str, case: &str) -> String {
    outputs(described).get(name).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the stack published no {name} output in {described}")
    })
}

/// Adding a method to a deployed api declares another deployment and re-points the stage at
/// it: the stack takes that change IN PLACE, and API Gateway reports the stage pointing at the
/// snapshot the redeployment took.
#[tokio::test]
async fn an_added_method_forces_a_new_deployment_the_stage_is_re_pointed_at() {
    let (router, _engine) = router_with_engine();
    let stack = "rest-api-redeploy-added-method";
    let case = "a redeployed api with a method added";

    let first = template(&Declared {
        stack,
        deployment: "DeploymentOne",
        api_description: "the api as first deployed",
        path_part: "items",
        operation: "getItems",
        deployment_description: "snapshot one",
        stage_description: "the live stage",
        variables: json!({ "Backend": "primary" }),
        extra: Vec::new(),
    });
    let described = deploy(&router, "CreateStack", stack, &first, CREATE_COMPLETE).await;
    let api_id = output(&described, "ApiId", case);
    let first_deployment = output(&described, "StageDeployment", case);

    // The api holds the one method the first deployment snapshotted.
    let (code, methods) = apigateway(
        &router,
        &format!("/restapis/{api_id}/resources?embed=methods"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from GetResources, got {code} with body {methods}"
    );

    // The same five resources with a method added, the deployment declared under ANOTHER
    // logical id (a new snapshot) and the stage stating that one.
    let second = template(&Declared {
        stack,
        deployment: "DeploymentTwo",
        api_description: "the api with both methods",
        path_part: "items",
        operation: "getItems",
        deployment_description: "snapshot two",
        stage_description: "the live stage",
        variables: json!({ "Backend": "primary" }),
        extra: vec![(
            "PostItems",
            json!({
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Items" },
                    "HttpMethod": "POST",
                    "AuthorizationType": "NONE",
                    "OperationName": "postItem",
                    "MethodResponses": [{ "StatusCode": "201" }],
                    "Integration": mock_integration("201", r#"{"created":true}"#)
                }
            }),
        )],
    });
    let described = deploy(&router, "UpdateStack", stack, &second, UPDATE_COMPLETE).await;
    let second_deployment = output(&described, "StageDeployment", case);
    assert_ne!(
        second_deployment, first_deployment,
        "case {case:?}: expected the redeployment to take a snapshot of its own, got the first \
         one back ({first_deployment})"
    );

    // The STAGE points at the new snapshot, as API Gateway itself reports it — the patch this
    // fix makes, read back through that service's own read rather than out of the stack.
    let (code, reported) = apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from GetStage, got {code} with body {reported}"
    );
    assert_eq!(
        reported.get("deploymentId").and_then(Value::as_str),
        Some(second_deployment.as_str()),
        "case {case:?}: expected the stage to point at the new deployment \
         ({second_deployment}), got {reported}"
    );

    // ...and the method the redeployment added is one API Gateway holds on the api, so what
    // that snapshot carries is both methods.
    let (_, resources) = apigateway(&router, &format!("/restapis/{api_id}/resources")).await;
    let items = resources
        .get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let on_the_path = items
        .iter()
        .find(|resource| resource.get("path").and_then(Value::as_str) == Some("/items"))
        .cloned()
        .unwrap_or(Value::Null);
    let resource_id = on_the_path
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for verb in ["GET", "POST"] {
        let (code, method) = apigateway(
            &router,
            &format!("/restapis/{api_id}/resources/{resource_id}/methods/{verb}"),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the api to hold the {verb} method, got {code} with body \
             {method}"
        );
    }
}

/// A changed declaration of every record a REST API is built out of is applied IN PLACE: one
/// update states a different value for each of the five types, and that service's own read of
/// each reports the new one.
#[tokio::test]
async fn a_changed_declaration_of_each_record_is_applied_in_place() {
    let (router, _engine) = router_with_engine();
    let stack = "rest-api-redeploy-in-place";
    let case = "the five records of a REST API, each redeclared";

    let first = template(&Declared {
        stack,
        deployment: "Deployment",
        api_description: "the api as first declared",
        path_part: "items",
        operation: "getItems",
        deployment_description: "the first snapshot",
        stage_description: "as first declared",
        variables: json!({ "Backend": "primary" }),
        extra: Vec::new(),
    });
    let described = deploy(&router, "CreateStack", stack, &first, CREATE_COMPLETE).await;
    let api_id = output(&described, "ApiId", case);

    // The same five resources, each with one property stated differently.
    let second = template(&Declared {
        stack,
        deployment: "Deployment",
        api_description: "the api as redeclared",
        path_part: "records",
        operation: "listItems",
        deployment_description: "the second snapshot",
        stage_description: "as redeclared",
        variables: json!({ "Backend": "standby" }),
        extra: Vec::new(),
    });
    deploy(&router, "UpdateStack", stack, &second, UPDATE_COMPLETE).await;

    let (_, stack_resources) =
        call(&router, "DescribeStackResources", &[member("StackName", stack)]).await;
    let physical = physical_ids(&stack_resources);
    let resource_id = physical.get("Items").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: no physical id for the api's resource in {stack_resources}")
    });
    let deployment_id = physical.get("Deployment").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: no physical id for the deployment in {stack_resources}")
    });

    // One row per record, each naming the read that reports it, the member the change is read
    // from and the value the second template states.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "the api's own description, a scalar UpdateRestApi replaces",
            format!("/restapis/{api_id}"),
            "description",
            "the api as redeclared",
        ),
        (
            "a resource's path segment, which UpdateResource moves",
            format!("/restapis/{api_id}/resources/{resource_id}"),
            "pathPart",
            "records",
        ),
        (
            "a method's operation name, a scalar UpdateMethod replaces",
            format!("/restapis/{api_id}/resources/{resource_id}/methods/GET"),
            "operationName",
            "listItems",
        ),
        (
            "a deployment's description, the one pointer UpdateDeployment writes",
            format!("/restapis/{api_id}/deployments/{deployment_id}"),
            "description",
            "the second snapshot",
        ),
        (
            "the stage's description, a scalar UpdateStage replaces",
            format!("/restapis/{api_id}/stages/{STAGE}"),
            "description",
            "as redeclared",
        ),
    ];
    for (row, uri, member_name, expected) in &rows {
        let (code, reported) = apigateway(&router, uri).await;
        assert_eq!(
            code, 200,
            "case {row:?}: expected HTTP 200 reading {uri}, got {code} with body {reported}"
        );
        assert_eq!(
            reported.get(*member_name).and_then(Value::as_str),
            Some(*expected),
            "case {row:?}: expected {member_name} to report {expected:?}, got {reported}"
        );
    }

    // A collection the caller KEYS is brought into step one entry at a time, which is how that
    // patch call writes one.
    let (code, reported) = apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        code, 200,
        "case \"a changed stage variable\": expected HTTP 200 from GetStage, got {code} with \
         body {reported}"
    );
    assert_eq!(
        reported
            .get("variables")
            .and_then(|variables| variables.get("Backend"))
            .and_then(Value::as_str),
        Some("standby"),
        "case \"a changed stage variable\": expected the changed variable to be in force, got \
         {reported}"
    );
}
