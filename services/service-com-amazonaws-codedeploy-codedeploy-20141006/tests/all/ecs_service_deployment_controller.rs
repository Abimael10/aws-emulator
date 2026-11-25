// SPDX-License-Identifier: CC0-1.0

//! The DEPLOYMENT CONTROLLER an Amazon ECS service a deployment group targets has
//! to be deployed by, driven over the wire through the same front door a client
//! uses (the harness in [`crate::codedeploy_lifecycle`], with Amazon ECS, Elastic
//! Load Balancing v2 and Amazon EC2 mounted beside CodeDeploy on one engine).
//!
//! RULE: a deployment group drives the Amazon ECS service its `ecsServices` names
//! through CodeDeploy, which that service only accepts when it was created with the
//! deployment controller that hands its deployments over — so a request that would
//! create or leave a group naming a service deployed by any other controller is
//! refused HTTP 400 `InvalidECSServiceException` with the sentence that states the
//! precondition (entirely fixed text, nothing of the request in it), and nothing is
//! stored for it.
//!
//! The rows differ along what the rule generalises over: which controller the named
//! service is deployed by (every value `DeploymentControllerType` carries —
//! `CODE_DEPLOY`, `ECS`, `EXTERNAL` — plus the one a service that named none
//! defaults to), how many pairs one request names and which of them is the
//! offending one, whether the group is being created or updated, and whether the
//! group is refused for the controller or for the pair not loading at all.
//!
//! Three crates have to agree for these rows to hold — CodeDeploy judges the
//! precondition, Amazon ECS owns the service and reports how it is deployed, and
//! Elastic Load Balancing v2 holds the target groups that service shifts traffic
//! between — so every resource is created through its owner's own API.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, Emulator};
use crate::ecs_service_resolution::{given_ecs_service, given_network, Network};

/// The modelled error a group naming a service CodeDeploy does not deploy earns.
const INVALID_ECS_SERVICE: &str = "InvalidECSServiceException";

/// CodeDeploy's sentence for that service: entirely fixed text, with neither the
/// pair the request named nor the controller the service does have in it.
const CONTROLLER_REQUIRED: &str =
    "Deployment group's ECS service must be configured for a CODE_DEPLOY deployment controller.";

/// Every value `DeploymentControllerType` carries
/// (`smithy_query(query="shape",shape="DeploymentControllerType")` on the Amazon
/// ECS model), each with the cluster and the service the fixture creates for it and
/// whether a deployment group may target it.
struct Controlled {
    case: &'static str,
    /// The `deploymentController.type` the service is created with, or `None` for a
    /// service created naming no controller at all — "if no deployment controller
    /// is specified, the default value of ECS is used".
    controller: Option<&'static str>,
    cluster: &'static str,
    service: &'static str,
    /// Whether a deployment group may be created over this service.
    targetable: bool,
}

const CONTROLLED: [Controlled; 4] = [
    Controlled {
        case: "a service deployed by CodeDeploy",
        controller: Some("CODE_DEPLOY"),
        cluster: "cd-cluster",
        service: "cd-service",
        targetable: true,
    },
    Controlled {
        case: "a service deployed by the Amazon ECS rolling update",
        controller: Some("ECS"),
        cluster: "ecs-cluster",
        service: "ecs-service",
        targetable: false,
    },
    Controlled {
        case: "a service that named no controller at all, so the default applies",
        controller: None,
        cluster: "default-cluster",
        service: "default-service",
        targetable: false,
    },
    Controlled {
        case: "a service deployed by an external controller",
        controller: Some("EXTERNAL"),
        cluster: "external-cluster",
        service: "external-service",
        targetable: false,
    },
];

/// The controller a service is created with, which is the one the account's service
/// carries: a request naming none takes the default Amazon ECS states for it.
fn created_with(controller: Option<&str>) -> &str {
    controller.unwrap_or("ECS")
}

/// One `ecsServices` entry, as a request names it.
fn pair(cluster: &str, service: &str) -> Value {
    json!({"clusterName": cluster, "serviceName": service})
}

/// A `CreateDeploymentGroup` request naming `ecs_services`, carrying the other
/// preconditions an Amazon ECS group has to satisfy — the blue/green,
/// traffic-controlled style and the load balancer it routes traffic behind — so the
/// controller of the service the pair names is the only thing under test.
fn create_request(application: &str, group: &str, ecs_services: Value) -> Value {
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": "arn:aws:iam::123456789012:role/CodeDeployRole",
        "ecsServices": ecs_services,
    });
    crate::codedeploy_lifecycle::require_platform_members("ECS", &mut request);
    request
}

/// The `ecsServices` `GetDeploymentGroup` reports for `group`.
async fn stored_ecs_services(emulator: &Emulator, application: &str, group: &str) -> Value {
    emulator
        .ok(
            "GetDeploymentGroup",
            json!({"applicationName": application, "deploymentGroupName": group}),
        )
        .await["deploymentGroupInfo"]["ecsServices"]
        .clone()
}

/// The deployment group names `ListDeploymentGroups` reports for `application`.
async fn listed_groups(emulator: &Emulator, application: &str) -> Vec<String> {
    emulator
        .ok(
            "ListDeploymentGroups",
            json!({"applicationName": application}),
        )
        .await["deploymentGroups"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// An emulator holding one service for every row of [`CONTROLLED`] and a CodeDeploy
/// application on the Amazon ECS compute platform.
async fn fixture(application: &str) -> Emulator {
    let emulator = Emulator::with_ecs();
    let network: Network = given_network(&emulator, "controller").await;
    for (slot, row) in CONTROLLED.iter().enumerate() {
        given_ecs_service(
            &emulator,
            &network,
            row.cluster,
            row.service,
            created_with(row.controller),
            slot,
        )
        .await;
    }
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "ECS"}),
        )
        .await;
    emulator
}

/// Assert the answer to one create: `targetable` says whether the group is created
/// and reads back, or refused with the controller sentence and left unstored.
async fn assert_create(
    emulator: &Emulator,
    application: &str,
    case: &str,
    group: &str,
    ecs_services: &Value,
    targetable: bool,
) {
    let (status, body) = emulator
        .call(
            "CreateDeploymentGroup",
            create_request(application, group, ecs_services.clone()),
        )
        .await;
    if targetable {
        assert_eq!(
            status, 200,
            "case {case:?}: expected the group to be created, got {status} with {body:?}"
        );
        // The pair the request named is what a read of the created group reports.
        let stored = stored_ecs_services(emulator, application, group).await;
        assert_eq!(
            &stored, ecs_services,
            "case {case:?}: expected the stored ecsServices to echo the request, got {stored:?}"
        );
        return;
    }
    assert_eq!(
        (status, error_code(&body).as_str(), body["message"].as_str()),
        (400, INVALID_ECS_SERVICE, Some(CONTROLLER_REQUIRED)),
        "case {case:?}: expected HTTP 400 {INVALID_ECS_SERVICE} with {CONTROLLER_REQUIRED:?}, \
         got {status} with {body:?}"
    );
}

/// RULE: a deployment group is created over an Amazon ECS service only when that
/// service is deployed by CodeDeploy; a service deployed by any other controller is
/// refused with the fixed sentence and no group is stored.
#[tokio::test]
async fn a_deployment_group_is_created_only_over_a_service_code_deploy_deploys() {
    const APPLICATION: &str = "controller-create-app";
    let emulator = fixture(APPLICATION).await;

    let mut served: Vec<String> = Vec::new();
    for row in &CONTROLLED {
        let group = format!("dg-{}", row.service);
        assert_create(
            &emulator,
            APPLICATION,
            row.case,
            &group,
            &json!([pair(row.cluster, row.service)]),
            row.targetable,
        )
        .await;
        if row.targetable {
            served.push(group);
        }
    }

    // Nothing is stored for a refused create: the application holds exactly the
    // groups the served rows created.
    let listed = listed_groups(&emulator, APPLICATION).await;
    assert_eq!(
        listed, served,
        "case \"the groups the application holds\": expected only the served rows' groups, got \
         {listed:?}"
    );
}

/// RULE applied to a request naming SEVERAL pairs: every pair the group targets has
/// to be deployed by CodeDeploy, so one entry that is not refuses the whole call
/// whichever position it is in, and a request whose every entry is may name as many
/// as it likes.
#[tokio::test]
async fn one_pair_that_code_deploy_does_not_deploy_refuses_the_whole_request() {
    const APPLICATION: &str = "controller-multi-app";
    let emulator = fixture(APPLICATION).await;
    let code_deploy = &CONTROLLED[0];
    let rolling = &CONTROLLED[1];
    let external = &CONTROLLED[3];

    // (case, group, the entries the request names, whether it is served)
    let rows: Vec<(&str, &str, Value, bool)> = vec![
        (
            "the rolling-update pair second, after one CodeDeploy deploys",
            "dg-second-bad",
            json!([
                pair(code_deploy.cluster, code_deploy.service),
                pair(rolling.cluster, rolling.service),
            ]),
            false,
        ),
        (
            "the rolling-update pair first",
            "dg-first-bad",
            json!([
                pair(rolling.cluster, rolling.service),
                pair(code_deploy.cluster, code_deploy.service),
            ]),
            false,
        ),
        (
            "the external pair beside the rolling-update one",
            "dg-both-bad",
            json!([
                pair(external.cluster, external.service),
                pair(rolling.cluster, rolling.service),
            ]),
            false,
        ),
        (
            "the same CodeDeploy-deployed pair named twice",
            "dg-repeated",
            json!([
                pair(code_deploy.cluster, code_deploy.service),
                pair(code_deploy.cluster, code_deploy.service),
            ]),
            true,
        ),
    ];

    let mut served: Vec<String> = Vec::new();
    for (case, group, ecs_services, targetable) in rows {
        assert_create(
            &emulator,
            APPLICATION,
            case,
            group,
            &ecs_services,
            targetable,
        )
        .await;
        if targetable {
            served.push(group.to_string());
        }
    }
    let listed = listed_groups(&emulator, APPLICATION).await;
    assert_eq!(
        listed, served,
        "case \"the groups the application holds\": expected only the served rows' groups, got \
         {listed:?}"
    );
}

/// RULE applied to an UPDATE: the targets the group would END UP with are what the
/// precondition is judged on, so an update that would move a group onto a service
/// CodeDeploy does not deploy is refused and changes nothing, while one onto a
/// service it does is served.
///
/// The ORDER the two `ecsServices` refusals are served in is checked here too: a
/// pair that could not be loaded at all is refused for the pair, and the controller
/// precondition is what a pair that DID load earns.
#[tokio::test]
async fn an_update_moves_a_group_only_onto_a_service_code_deploy_deploys() {
    const APPLICATION: &str = "controller-update-app";
    const GROUP: &str = "dg";
    let emulator = fixture(APPLICATION).await;
    let code_deploy = &CONTROLLED[0];
    let rolling = &CONTROLLED[1];
    let external = &CONTROLLED[3];
    let held = json!([pair(code_deploy.cluster, code_deploy.service)]);
    emulator
        .ok(
            "CreateDeploymentGroup",
            create_request(APPLICATION, GROUP, held.clone()),
        )
        .await;

    // (case, the replacement the update names, the message it is refused with)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "onto a service deployed by the Amazon ECS rolling update",
            json!([pair(rolling.cluster, rolling.service)]),
            Some(CONTROLLER_REQUIRED),
        ),
        (
            "onto a service deployed by an external controller",
            json!([pair(external.cluster, external.service)]),
            Some(CONTROLLER_REQUIRED),
        ),
        (
            "onto a cluster the account does not have",
            json!([pair("ghost-cluster", "ghost-service")]),
            // The pair never loaded, so the refusal is about the pair rather than
            // about how a service is deployed.
            Some(
                "Could not load ECS service information for cluster: ghost-cluster, service: \
                 ghost-service. Cause: ",
            ),
        ),
        (
            "onto the service it already targets",
            json!([pair(code_deploy.cluster, code_deploy.service)]),
            None,
        ),
    ];

    for (case, replacement, refusal) in rows {
        let (status, body) = emulator
            .call(
                "UpdateDeploymentGroup",
                json!({
                    "applicationName": APPLICATION,
                    "currentDeploymentGroupName": GROUP,
                    "ecsServices": replacement,
                }),
            )
            .await;
        match refusal {
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be served, got {status} with {body:?}"
            ),
            Some(expected) => {
                let message = body["message"].as_str().unwrap_or_default();
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, INVALID_ECS_SERVICE),
                    "case {case:?}: expected HTTP 400 {INVALID_ECS_SERVICE}, got {status} with \
                     {body:?}"
                );
                assert!(
                    message.starts_with(expected),
                    "case {case:?}: expected a message opening {expected:?}, got {message:?}"
                );
            }
        }
        // A refused update leaves the group's targets exactly as they were, and the
        // served one leaves it on the pair it named.
        let stored = stored_ecs_services(&emulator, APPLICATION, GROUP).await;
        assert_eq!(
            stored, held,
            "case {case:?}: expected the group to hold {held:?}, got {stored:?}"
        );
    }
}
