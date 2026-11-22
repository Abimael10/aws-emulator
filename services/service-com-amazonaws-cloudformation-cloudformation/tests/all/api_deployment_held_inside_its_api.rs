// SPDX-License-Identifier: CC0-1.0

//! What a stack's TEARDOWN does with an `AWS::ApiGateway::Deployment` whose own create left a
//! stage pointing at it, read back out of API Gateway over that service's own wire.
//!
//! The rule every case here holds the service to: **a deployment takes the stage its own create
//! made with it, so a teardown that removes the deployment is not refused on account of that
//! stage.** `CreateDeployment$stageName` is "the name of the Stage resource for the Deployment
//! resource to create", so a declaration stating `StageName` makes API Gateway create a stage
//! the stack does not manage — which the AWS SAM transform's generated deployment always does —
//! and API Gateway refuses to delete a deployment while a stage points at it ("deleting a
//! deployment will only succeed if there are no Stage resources associated with it"). The stage
//! is part of what the deployment put there, so its removal goes with the deployment's and the
//! teardown settles on the status a teardown that removed everything reports.
//!
//! The recorded trace this suite was written for is ONE case of it: a SAM template whose
//! generated deployment declares the `Stage` stage, in the ROLLBACK of a create that failed on
//! another resource — which is what ended `ROLLBACK_FAILED` with the api, its deployment and
//! its stages all left behind. The cases differ along what the rule generalises over — which
//! teardown runs (a failed create's rollback, and a `DeleteStack`), whether the template is a
//! SAM one or declares the api, its deployment and its stage itself, and whether the api the
//! deployment belongs to is one the teardown removes or one the stack RETAINS, which keeps the
//! api while the deployment inside it still goes.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::sam_transform::sam_template;
use crate::stack_resource_provisioning::{physical_id, stage_code_objects};
use crate::stack_lifecycle::{
    call, create_stack, kv, member, resource_event_statuses, router_with_engine, xml_text,
};

/// A `ManagedPolicyArns` entry that is no ARN at all, which is what makes the role's create
/// fail and the create roll back: the teardown this suite is about is then the rollback of a
/// create that failed on a resource of its own.
const NOT_AN_ARN: &str = "not-a-policy-arn";

/// The api, the resource and the method a hand-written template declares, plus the deployment
/// that snapshots them and the stage that serves it.
///
/// The DEPLOYMENT declares a `StageName` of its own, so API Gateway creates a stage for it
/// that the stack does not manage — exactly what the AWS SAM transform's generated deployment
/// does — beside the stage the template declares. `retain` is whether the api carries
/// `DeletionPolicy: Retain`.
fn api_with_an_unmanaged_stage(name: &str, retain: bool, extra: &str) -> String {
    let policy = match retain {
        true => r#""DeletionPolicy":"Retain","#,
        false => "",
    };
    format!(
        r#"{{"Resources":{{
          "Api":{{{policy}"Type":"AWS::ApiGateway::RestApi","Properties":{{
            "Name":"{name}","EndpointConfiguration":{{"Types":["REGIONAL"]}}}}}},
          "Items":{{"Type":"AWS::ApiGateway::Resource","Properties":{{
            "RestApiId":{{"Ref":"Api"}},
            "ParentId":{{"Fn::GetAtt":["Api","RootResourceId"]}},
            "PathPart":"items"}}}},
          "GetItems":{{"Type":"AWS::ApiGateway::Method","Properties":{{
            "RestApiId":{{"Ref":"Api"}},"ResourceId":{{"Ref":"Items"}},
            "HttpMethod":"GET","AuthorizationType":"NONE",
            "MethodResponses":[{{"StatusCode":"200"}}],
            "Integration":{{"Type":"MOCK",
              "RequestTemplates":{{"application/json":"{{\"statusCode\":200}}"}},
              "IntegrationResponses":[{{"StatusCode":"200"}}]}}}}}},
          "Deployment":{{"Type":"AWS::ApiGateway::Deployment","DependsOn":["GetItems"],
            "Properties":{{"RestApiId":{{"Ref":"Api"}},"StageName":"live"}}}},
          "Stage":{{"Type":"AWS::ApiGateway::Stage","Properties":{{
            "RestApiId":{{"Ref":"Api"}},"DeploymentId":{{"Ref":"Deployment"}},
            "StageName":"prod"}}}}{extra}}}}}"#
    )
}

/// A role whose create IAM refuses, declared beside the api above so the stack's CREATE fails
/// on a resource the api chain does not depend on.
fn refused_role() -> String {
    format!(
        r#",
          "Role":{{"Type":"AWS::IAM::Role","Properties":{{
            "AssumeRolePolicyDocument":{{"Version":"2012-10-17","Statement":[{{
              "Action":["sts:AssumeRole"],"Effect":"Allow",
              "Principal":{{"Service":["lambda.amazonaws.com"]}}}}]}},
            "ManagedPolicyArns":["{NOT_AN_ARN}"]}}}}"#
    )
}

/// Submit `template` as stack `stack` and settle the store, acknowledging the IAM resources a
/// template may declare, and answer the `StackId` the create minted — the only name a stack
/// the deletion REMOVED is still described by.
async fn submit(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    template: &str,
) -> String {
    stage_code_objects(router, case, template).await;
    let (code, body) = create_stack(
        router,
        stack,
        template,
        &kv(&[
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ]),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    xml_text(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: the create published no StackId in {body}"))
}

/// `GetRestApi` (`GET /restapis/{restApiId}`): the status API Gateway answers for one api id.
async fn get_rest_api(router: &MultiServiceRouter, api_id: &str) -> (u16, Value) {
    apigateway(router, "GET", &format!("/restapis/{api_id}")).await
}

/// Delete the stack and read it until it reports a terminal status.
///
/// Both calls name the stack by its `StackId`: "deleted stacks … can be viewed with the
/// DescribeStacks operation if the stack name is specified by its unique stack ID", which is
/// the only name a stack the deletion really removed answers under.
async fn delete_and_settle(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_id: &str,
) -> String {
    let (code, body) = call(router, "DeleteStack", &[member("StackName", stack_id)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DeleteStack to be accepted, got {code} {body}"
    );
    settle_the_store(router, engine).await;
    poll_to_terminal(router, case, stack_id).await
}

/// RULE: the ROLLBACK of a create that failed elsewhere removes the api it had deployed and
/// the deployment inside it, though a stage that deployment's own create made still points at
/// that deployment.
#[tokio::test]
async fn a_rollback_removes_an_api_whose_deployment_a_stage_still_points_at() {
    let case = "a failed create whose rollback takes down an api it had deployed";
    let (router, engine) = router_with_engine();
    let stack = "heldapi-rollback";
    let template = api_with_an_unmanaged_stage(
        "heldapi-rollback-api",
        false,
        &refused_role(),
    );
    let _stack_id = submit(&router, &engine, case, stack, &template).await;

    // The api id the stack created, read before the rollback has concluded is immaterial: the
    // record is what the rollback took down, and the id is what API Gateway is asked about.
    let api_id = physical_id(&router, stack, "Api").await;
    assert!(
        !api_id.is_empty(),
        "case {case:?}: expected the stack to have created the api it declares"
    );

    let described = poll_to_terminal(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the rollback to remove every resource the create made, got \
         {described}"
    );

    let statuses = resource_event_statuses(&router, case, stack, "Deployment").await;
    assert!(
        !statuses.iter().any(|status| status == "DELETE_FAILED"),
        "case {case:?}: expected the deployment's removal to carry the stage its own create \
         made with it, got {statuses:?}"
    );

    let (code, document) = get_rest_api(&router, &api_id).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected API Gateway to hold no api the rollback removed, got {code} \
         with {document}"
    );
}

/// RULE (the same rule, on `DeleteStack` and over the stage the AWS SAM transform's own
/// generated deployment creates): the stack's deletion removes the api and completes.
#[tokio::test]
async fn deleting_a_sam_stack_removes_the_api_its_generated_deployment_staged() {
    let case = "a SAM stack whose generated deployment created a stage of its own";
    let (router, engine) = router_with_engine();
    let stack = "heldapi-sam-delete";
    let template = sam_template(
        r#"{"Reader":{"Type":"AWS::Serverless::Function","Properties":{
             "Handler":"index.handler","Runtime":"python3.12",
             "CodeUri":"s3://code-bucket/Reader.zip",
             "Events":{"Get":{"Type":"Api","Properties":{
                "Path":"/items","Method":"get"}}}}}}"#,
    );
    let stack_id = submit(&router, &engine, case, stack, &template).await;
    let described = poll_to_terminal(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the SAM stack to deploy, got {described}"
    );

    let api_id = physical_id(&router, stack, "ServerlessRestApi").await;
    let (code, document) = get_rest_api(&router, &api_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected API Gateway to hold the api the stack deployed, got {code} \
         with {document}"
    );

    let described = delete_and_settle(&router, &engine, case, &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the deletion to remove every resource the stack held, got \
         {described}"
    );
    let (code, document) = get_rest_api(&router, &api_id).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected API Gateway to hold no api the deletion removed, got {code} \
         with {document}"
    );
}

/// RULE (the api the deployment belongs to is NOT what removes it): an api the stack RETAINS
/// stays exactly where it is, and the deletion of the deployment inside it still goes through —
/// the stage that deployment's own create made goes with it, so the stack's deletion completes
/// around the api its policy keeps.
#[tokio::test]
async fn a_deployment_inside_a_retained_api_is_still_removed_with_its_own_stage() {
    let case = "an api the stack retains, holding the stage the deployment's create made";
    let (router, engine) = router_with_engine();
    let stack = "heldapi-retained";
    let template = api_with_an_unmanaged_stage("heldapi-retained-api", true, "");
    let stack_id = submit(&router, &engine, case, stack, &template).await;
    let described = poll_to_terminal(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to deploy the api it declares, got {described}"
    );
    let api_id = physical_id(&router, stack, "Api").await;

    let described = delete_and_settle(&router, &engine, case, &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the deletion to remove the deployment the retained api holds, \
         got {described}"
    );
    let statuses = resource_event_statuses(&router, case, &stack_id, "Deployment").await;
    assert!(
        !statuses.iter().any(|status| status == "DELETE_FAILED"),
        "case {case:?}: expected the deployment's removal to carry its own stage with it, got \
         {statuses:?}"
    );
    // The api itself is what the policy keeps: `Retain` is asked of nobody, so API Gateway
    // still serves it after the stack is gone.
    let (code, document) = get_rest_api(&router, &api_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the retained api to be served still, got {code} with \
         {document}"
    );
    // ...and the stage the deployment created is gone with the deployment, while the api that
    // held it stays: the removal reached inside the api the stack retained.
    let (code, document) =
        apigateway(&router, "GET", &format!("/restapis/{api_id}/stages/live")).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected the stage the deployment's create made to go with it, got \
         {code} with {document}"
    );
}
