// SPDX-License-Identifier: CC0-1.0

//! What becomes of the stage an `AWS::ApiGateway::Deployment` CREATED when the stack gives
//! that deployment up, end to end over the awsQuery wire.
//!
//! RULE under test: **a resource provider's delete takes down what its own create made, so a
//! resource the stack created can always be given up again: a deployment declared with a
//! `StageName` created that stage ("the name of the Stage resource for the Deployment resource
//! to create"), and API Gateway refuses to delete a deployment while a stage still points at
//! it ("deleting a deployment will only succeed if there are no Stage resources associated
//! with it"), so the delete clears that association first. A rollback and a stack deletion are
//! then both carried out to the end — the stack settles in its COMPLETE status rather than its
//! FAILED one — and the owning service holds nothing of the stack's resources afterwards.**
//!
//! Why it matters: a client that waits on `stack_rollback_complete` (or on a deletion) never
//! returns for a stack whose rollback stopped on a delete the owning service refused, and the
//! resources that rollback could not remove keep their names reserved — so a second deploy of
//! the corrected template collides with the leftovers of the first.
//!
//! The rows differ along what the rule generalises over: WHICH operation gives the deployment
//! up (a rollback of a failed create, and a deletion of a completed stack) and WHO created the
//! stage that points at it (the deployment itself, and a sibling `AWS::ApiGateway::Stage`
//! resource that names the deployment).
//!
//! The recorded trace this suite was written for is the first row: a stack whose
//! `AWS::ApiGateway::Stage` declares the stage name its sibling deployment already created,
//! which API Gateway refuses — and which AWS rolls all the way back.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::harness::{settled_stack, settled_stack_status, REGION};
use crate::stack_lifecycle::{
    call, create_stack, member, resource_event_statuses, router, wait_for_the_transition, xml_text,
};

/// The statuses this suite is stated about: the stack status a rollback that removed
/// everything settles in, the one a rollback that could not settles in, and the resource
/// status a deletion the owning service carried out records.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const DELETE_FAILED: &str = "DELETE_FAILED";

/// How the stack gives the deployment up in one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GivenUpBy {
    /// A resource of the stack failed to create, so CloudFormation rolls the create back.
    ARollback,
    /// The stack completed and the client then deletes it.
    ADeletion,
}

/// One row: the template deployed, how the deployment is given up, and the stage the api
/// carries while it stands.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The stage the deployment's own create makes, which the row's template names.
    stage: &'static str,
    given_up_by: GivenUpBy,
    /// Whether a sibling `AWS::ApiGateway::Stage` resource declares a stage of its own.
    sibling_stage: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a stage declaring the name its sibling deployment already created",
        stack: "apigw-stage-collides-with-deployment",
        stage: "prod",
        given_up_by: GivenUpBy::ARollback,
        sibling_stage: true,
    },
    Row {
        case: "a completed stack whose deployment created the only stage",
        stack: "apigw-deployment-own-stage-deleted",
        stage: "live",
        given_up_by: GivenUpBy::ADeletion,
        sibling_stage: false,
    },
    Row {
        case: "a completed stack whose stage resource points at the deployment",
        stack: "apigw-deployment-sibling-stage-deleted",
        stage: "edge",
        given_up_by: GivenUpBy::ADeletion,
        sibling_stage: true,
    },
];

/// The api every row declares: one `MOCK` method under the root, so the deployment has a
/// method to snapshot, and the deployment that creates the row's stage.
///
/// A row with a sibling stage declares an `AWS::ApiGateway::Stage` beside it. For the
/// ROLLBACK row that stage names the stage the deployment already created, which is the
/// collision the owning service refuses; for the deletion row it names a stage of its own and
/// points at the same deployment.
fn template(row: &Row) -> String {
    let mut resources = json!({
        "Api": {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{}-api", row.stack),
                "EndpointConfiguration": { "Types": ["REGIONAL"] },
            },
        },
        "Root": {
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "Integration": {
                    "Type": "MOCK",
                    "RequestTemplates": { "application/json": "{\"statusCode\":200}" },
                },
            },
        },
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["Root"],
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "StageName": row.stage,
            },
        },
    });
    if row.sibling_stage {
        let declared = match row.given_up_by {
            // The collision: the stage this resource declares is the one the deployment's own
            // create already made.
            GivenUpBy::ARollback => row.stage.to_string(),
            GivenUpBy::ADeletion => format!("{}-served", row.stage),
        };
        resources["Stage"] = json!({
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": declared,
            },
        });
    }
    json!({
        "Resources": resources,
        "Outputs": { "ApiId": { "Value": { "Ref": "Api" } } },
    })
    .to_string()
}

/// The physical id of the api the stack created, read off the stack's own output, or `None`
/// where the stack reports none — which is what a stack whose api was never created reports.
async fn published_api(router: &MultiServiceRouter, stack: &str) -> Option<String> {
    let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    (status == 200)
        .then(|| crate::stack_lifecycle::output_value(&body, "ApiId"))
        .flatten()
}

/// Whether API Gateway holds an api NAMED `name`: a page of that service's own collection
/// read, which reports no `item` member at all for a page that holds none.
async fn api_exists(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, page) = apigateway(router, "GET", "/restapis").await;
    assert_eq!(
        status, 200,
        "case {name:?}: GetRestApis must answer 200, got {status} {page}"
    );
    page.get("item")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|api| api.get("name").and_then(Value::as_str) == Some(name))
        })
}

#[tokio::test]
async fn a_deployment_the_stack_gives_up_takes_the_stage_it_created_with_it() {
    for row in ROWS {
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {created}",
            row.case
        );
        // The create's own terminal status, and — for the row whose create fails — the
        // rollback's: both are on the stack's clock.
        wait_for_the_transition().await;
        wait_for_the_transition().await;

        let name = format!("{}-api", row.stack);
        match row.given_up_by {
            GivenUpBy::ARollback => {
                let settled = settled_stack_status(row.case, &router, REGION, row.stack).await;
                assert_eq!(
                    settled, ROLLBACK_COMPLETE,
                    "case {:?}: expected the rollback to remove everything it created, got \
                     {settled:?}",
                    row.case
                );
                // The deployment's own deletion was CARRIED OUT: a delete the owning service
                // refused is what records the failed one.
                let statuses =
                    resource_event_statuses(&router, row.case, row.stack, "Deployment").await;
                assert!(
                    statuses.iter().any(|status| status == DELETE_COMPLETE)
                        && !statuses.iter().any(|status| status == DELETE_FAILED),
                    "case {:?}: expected the deployment's deletion to be carried out, got \
                     {statuses:?}",
                    row.case
                );
            }
            GivenUpBy::ADeletion => {
                // The stage the deployment created really serves the api before the stack is
                // deleted, so what the deletion gives up is a resource that exists.
                let api_id = published_api(&router, row.stack).await.unwrap_or_else(|| {
                    panic!("case {:?}: the stack published no ApiId output", row.case)
                });
                let (status, stage) =
                    apigateway(&router, "GET", &format!("/restapis/{api_id}/stages/{}", row.stage))
                        .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected API Gateway to hold the stage the deployment created, \
                     got {status} {stage}",
                    row.case
                );
                let (status, body) =
                    call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
                assert_eq!(
                    status, 200,
                    "case {:?}: DeleteStack must answer 200, got {status} {body}",
                    row.case
                );
                wait_for_the_transition().await;
                wait_for_the_transition().await;
                // The deletion is settled when this service stops reporting the stack as an
                // operation in flight: a stack it no longer reports at all is one whose
                // deletion was carried out, and a stack it still reports must report the
                // completed deletion rather than a failed one.
                let body = settled_stack(row.case, &router, REGION, row.stack).await;
                let reported = xml_text(&body, "StackStatus").unwrap_or_default();
                assert!(
                    reported.is_empty() || reported == DELETE_COMPLETE,
                    "case {:?}: expected the deletion to be carried out, got {reported:?}",
                    row.case
                );
            }
        }

        // ...and the owning service holds nothing of the stack's api afterwards, so the name
        // the declaration reserved is free again.
        assert!(
            !api_exists(&router, &name).await,
            "case {:?}: expected API Gateway to hold no api named {name:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_stage_colliding_with_its_deployments_own_stage_fails_that_resource() {
    // The collision itself, on the resource it is about: the stage the owning service refused
    // is the one that failed, and the api's own stage — the one the deployment created — is
    // what the refusal was about.
    let row = &ROWS[0];
    let case = "the failed stage resource";
    let router = router();
    let stack = "apigw-stage-collision-reason";
    let (status_code, created) = create_stack(&router, stack, &template(row), &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );
    wait_for_the_transition().await;
    wait_for_the_transition().await;

    let statuses = resource_event_statuses(&router, case, stack, "Stage").await;
    assert!(
        statuses.iter().any(|status| status == "CREATE_FAILED"),
        "case {case:?}: expected the colliding stage to fail, got {statuses:?}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled, ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack to roll back, got {settled:?}"
    );
    let (status, body) = call(&router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStacks must answer 200, got {status} {body}"
    );
    let reported = xml_text(&body, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, ROLLBACK_COMPLETE,
        "case {case:?}: expected the settled status to be reported, got {reported:?}"
    );
}
