// SPDX-License-Identifier: CC0-1.0

//! Giving up an `AWS::ApiGateway::Deployment` whose api holds stages the stack RELEASED rather
//! than deleted, read back out of API Gateway over that service's own wire.
//!
//! RULE under test: **deleting a deployment clears the stages of its api that still SERVE it,
//! whoever created them and whether or not the stack still manages them, and leaves every other
//! stage of that api exactly as it is.** API Gateway will not let a deployment go while a stage
//! points at it ("deleting a deployment will only succeed if there are no Stage resources
//! associated with it", whose refusal says those stages "must be moved or deleted"), so clearing
//! that association is part of deleting the deployment — and WHICH stages hold it comes from that
//! service's own listing, which narrows an api's stages by the deployment they serve
//! (`GetStages`' `deploymentId` query parameter is "the stages' deployment identifiers":
//! `smithy_query(query="operation",operation="GetStages",model_path="services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json")`).
//!
//! These cases take the RELEASED half of that rule, which `apigateway_deployment_pointing_stages_cleared.rs`
//! does not cover: a stage left live by a `DeletionPolicy: Retain` on the stack's own stage
//! resource — the shape the stack's deletion meets after a retaining stage move — rather than one
//! left by a replacement or created outside the stack. The rows differ along what the rule
//! generalises over: HOW MANY released stages point at the deployment, whether the stage the
//! DEPLOYMENT's own create made (its declared `StageName`, which the stack never manages) is among
//! them, and whether the api also holds a live stage serving another, retained deployment — the
//! stage no read narrowed to this deployment reports. The last case keeps the api itself under
//! `DeletionPolicy: Retain`, so the api OUTLIVES the deletion: what became of each stage inside it
//! can then only be the association being cleared, never the api taking everything with it.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the api, deployment and stage reads over API
//! Gateway's own `restJson1` wire — so what is asserted is what a client that deploys the template
//! and then calls API Gateway observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::harness::{call, member, router, settled_stack, xml_text, xml_texts, REGION};
use crate::stack_lifecycle::resource_event_statuses;

/// The statuses these cases hold their stack and its resources to.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const DELETE_FAILED: &str = "DELETE_FAILED";

/// The logical id of the SECOND deployment a case declares, with a stage of its own: both are
/// `DeletionPolicy: Retain`, so this deletion asks nobody about either and the stage stays
/// serving it.
const OTHER_DEPLOYMENT: &str = "OtherDeployment";

/// The template a case deploys: the api, one `MOCK` method on its root so a deployment is a
/// snapshot of something, the deployment, and one `Stage` resource per name in `released` under
/// `DeletionPolicy: Retain` — "CloudFormation keeps the resource without deleting" it, so each
/// stays live, still serving the deployment, when the deletion reaches it.
///
/// `deployment_stage` is the `StageName` the DEPLOYMENT itself declares, which makes
/// `CreateDeployment` create a stage the stack never manages. `other_stage` adds a second
/// deployment of the same api, retained, with a retained stage of its own. `retain_api` keeps the
/// api itself, so the deletion removes nothing that holds the stages.
fn template(
    stack: &str,
    released: &[&str],
    deployment_stage: Option<&str>,
    other_stage: Option<&str>,
    retain_api: bool,
) -> String {
    let mut declared = json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "EndpointConfiguration": { "Types": ["REGIONAL"] },
                },
            },
            "RootGet": {
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
                "DependsOn": ["RootGet"],
                "Properties": { "RestApiId": { "Ref": "Api" } },
            },
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "DeploymentId": { "Value": { "Ref": "Deployment" } },
        },
    });
    if let Some(stage_name) = deployment_stage {
        declared["Resources"]["Deployment"]["Properties"]["StageName"] = json!(stage_name);
    }
    for (index, stage_name) in released.iter().enumerate() {
        declared["Resources"][format!("Stage{index}")] = json!({
            "Type": "AWS::ApiGateway::Stage",
            "DeletionPolicy": "Retain",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": stage_name,
            },
        });
    }
    if let Some(stage_name) = other_stage {
        declared["Resources"][OTHER_DEPLOYMENT] = json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DeletionPolicy": "Retain",
            "DependsOn": ["RootGet"],
            "Properties": { "RestApiId": { "Ref": "Api" }, "Description": "the other snapshot" },
        });
        declared["Resources"]["OtherStage"] = json!({
            "Type": "AWS::ApiGateway::Stage",
            "DeletionPolicy": "Retain",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": OTHER_DEPLOYMENT },
                "StageName": stage_name,
            },
        });
        declared["Outputs"]["OtherDeploymentId"] = json!({ "Value": { "Ref": OTHER_DEPLOYMENT } });
    }
    if retain_api {
        declared["Resources"]["Api"]["DeletionPolicy"] = json!("Retain");
    }
    declared.to_string()
}

/// `CreateStack` the template and answer the settled stack's own `DescribeStacks` body, holding
/// the create to `CREATE_COMPLETE`.
///
/// Nothing here waits for a span: a stack's terminal status is due at an instant this service
/// records, so the wait is for that status APPEARING ([`settled_stack`]).
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) -> String {
    let (code, answered) = call(
        router,
        "CreateStack",
        &[member("StackName", stack), member("TemplateBody", body)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateStack to answer HTTP 200, got {code} with body {answered}"
    );
    let described = settled_stack(case, router, REGION, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    if settled != CREATE_COMPLETE {
        let (_, events) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
        panic!(
            "case {case:?}: expected the create to reach {CREATE_COMPLETE}, got {settled:?} with \
             resource failures {:?}",
            xml_texts(&events, "ResourceStatusReason")
        );
    }
    described
}

/// The value of the stack output `name`, or a panic naming what the stack published instead.
fn output(described: &str, name: &str, case: &str) -> String {
    xml_texts(described, "OutputKey")
        .into_iter()
        .zip(xml_texts(described, "OutputValue"))
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the stack published no {name} output: {described}")
        })
}

/// Delete the stack, addressed by the id a completed deletion is still described by, and hold it
/// to `DELETE_COMPLETE`.
async fn deleted(router: &MultiServiceRouter, case: &str, stack: &str, described: &str) {
    let stack_id = xml_text(described, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: the stack reported no StackId: {described}"));
    let (code, body) = call(router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DeleteStack to answer HTTP 200, got {code} with body {body}"
    );
    let settled_body = settled_stack(case, router, REGION, &stack_id).await;
    let settled = xml_text(&settled_body, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled, DELETE_COMPLETE,
        "case {case:?}: expected the deletion to be carried out, got {settled:?} with reason {:?}",
        xml_text(&settled_body, "StackStatusReason")
    );
    // The deployment's own deletion was CARRIED OUT: a delete the owning service refused is what
    // records the failed one.
    let statuses = resource_event_statuses(router, case, &stack_id, "Deployment").await;
    assert!(
        statuses.iter().any(|status| status == DELETE_COMPLETE)
            && !statuses.iter().any(|status| status == DELETE_FAILED),
        "case {case:?}: expected the deployment's deletion to be carried out, got {statuses:?}"
    );
}

/// The names of the stages API Gateway reports as serving `deployment_id`, through the filter
/// `GetStages` publishes for exactly that question, sorted so the assertion is about the SET.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    deployment_id: &str,
) -> Vec<String> {
    let (code, document) = apigateway(
        router,
        "GET",
        &format!("/restapis/{api_id}/stages?deploymentId={deployment_id}"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetStages to answer 200, got {code} with {document}"
    );
    let mut stages: Vec<String> = document
        .get("item")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|stage| stage.get("stageName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    stages.sort();
    stages
}

/// The status API Gateway answers for one stage of one api.
async fn stage_status(router: &MultiServiceRouter, api_id: &str, stage: &str) -> (u16, Value) {
    apigateway(router, "GET", &format!("/restapis/{api_id}/stages/{stage}")).await
}

/// One row: the stack, the stages it RELEASES onto the deployment, the stage the deployment's own
/// create makes, and the stage of the api's other deployment.
struct Row {
    case: &'static str,
    stack: &'static str,
    released: &'static [&'static str],
    deployment_stage: Option<&'static str>,
    other: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "one stage the stack released onto the deployment",
        stack: "apigw-released-stage-one",
        released: &["live"],
        deployment_stage: None,
        other: None,
    },
    Row {
        case: "two released stages and the one the deployment's own create made",
        stack: "apigw-released-stage-many",
        released: &["prod", "staging"],
        deployment_stage: Some("live"),
        other: None,
    },
    Row {
        case: "a released stage beside one serving another, retained deployment",
        stack: "apigw-released-stage-other",
        released: &["live"],
        deployment_stage: None,
        other: Some("beta"),
    },
];

#[tokio::test]
async fn a_deployment_given_up_takes_the_stages_its_stack_released_onto_it() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let described = deployed(
            &router,
            case,
            row.stack,
            &template(
                row.stack,
                row.released,
                row.deployment_stage,
                row.other,
                false,
            ),
        )
        .await;
        let api_id = output(&described, "ApiId", case);
        let deployment_id = output(&described, "DeploymentId", case);

        // What the deletion is up against really exists, as API Gateway itself reports it: every
        // released stage of the row, and the one the deployment's create made, serves the
        // deployment the stack is about to give up.
        let mut expected: Vec<String> = row
            .released
            .iter()
            .chain(row.deployment_stage.iter())
            .map(|stage| (*stage).to_string())
            .collect();
        expected.sort();
        let serving_it = serving(&router, case, &api_id, &deployment_id).await;
        assert_eq!(
            serving_it, expected,
            "case {case:?}: expected these stages to serve the deployment, got {serving_it:?}"
        );
        // ...while the stage of the api's OTHER deployment serves that one and is reported for
        // it, so the listing this delete reads is narrowed by the deployment and not the api.
        if let Some(stage) = row.other {
            let other_id = output(&described, "OtherDeploymentId", case);
            let serving_other = serving(&router, case, &api_id, &other_id).await;
            assert_eq!(
                serving_other,
                vec![stage.to_string()],
                "case {case:?}: expected {stage:?} to serve the other deployment, got \
                 {serving_other:?}"
            );
        }

        deleted(&router, case, row.stack, &described).await;

        // ...and nothing of the api is left: the deletion removed the api, so every read of it
        // and of the stages inside it answers API Gateway's own missing-resource refusal.
        let mut gone = expected.clone();
        gone.extend(row.other.map(str::to_string));
        for stage in &gone {
            let (code, document) = stage_status(&router, &api_id, stage).await;
            assert_eq!(
                code, 404,
                "case {case:?}: expected {stage:?} to be gone, got {code} with {document}"
            );
        }
        let (code, document) = apigateway(&router, "GET", &format!("/restapis/{api_id}")).await;
        assert_eq!(
            code, 404,
            "case {case:?}: expected the api to be gone, got {code} with {document}"
        );
    }
}

#[tokio::test]
async fn a_released_stage_is_cleared_inside_an_api_the_stack_keeps() {
    let case = "an api the stack retains, holding a released stage and another deployment's";
    let stack = "apigw-released-stage-retained-api";
    let router = router();
    let described = deployed(
        &router,
        case,
        stack,
        &template(stack, &["live"], None, Some("beta"), true),
    )
    .await;
    let api_id = output(&described, "ApiId", case);
    let deployment_id = output(&described, "DeploymentId", case);
    let other_id = output(&described, "OtherDeploymentId", case);
    let serving_it = serving(&router, case, &api_id, &deployment_id).await;
    assert_eq!(
        serving_it,
        vec!["live".to_string()],
        "case {case:?}: expected the released stage to serve the deployment, got {serving_it:?}"
    );

    deleted(&router, case, stack, &described).await;

    // The api the policy keeps is still served...
    let (code, document) = apigateway(&router, "GET", &format!("/restapis/{api_id}")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the retained api to be served still, got {code} with {document}"
    );
    // ...the stage that served the deleted deployment is gone from inside that very api, so it
    // was the association being cleared and not the api taking everything with it...
    let (code, document) = stage_status(&router, &api_id, "live").await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected the stage that served the deployment to go with it, got {code} \
         with {document}"
    );
    // ...and the stage of the api's other, retained deployment is left exactly as it was, still
    // serving it.
    let (code, document) = stage_status(&router, &api_id, "beta").await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the other deployment's stage to be left as it was, got {code} \
         with {document}"
    );
    let serving_other = serving(&router, case, &api_id, &other_id).await;
    assert_eq!(
        serving_other,
        vec!["beta".to_string()],
        "case {case:?}: expected that stage to serve the other deployment still, got \
         {serving_other:?}"
    );
}
