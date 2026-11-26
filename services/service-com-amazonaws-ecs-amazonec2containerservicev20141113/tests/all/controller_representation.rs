// SPDX-License-Identifier: CC0-1.0

//! How a service's DEPLOYMENT CONTROLLER decides what its running configuration is
//! reported AS, and what a cluster reports of the capacity providers its create
//! associated.
//!
//! The rules under test, each over the class rather than one request:
//!
//! > 1. A service the ECS scheduler deploys reports its running configuration as a
//! >    DEPLOYMENT and holds no task set; a service a controller that shifts traffic
//! >    between task sets deploys (`CODE_DEPLOY`) reports it as a TASK SET and no
//! >    deployment, and the original application task set is minted by the create itself
//! >    out of the service's own configuration — `PRIMARY`, running the service's
//! >    revision, scaled to the whole of the service's count, so its
//! >    `computedDesiredCount` is that count. An `EXTERNAL`-controlled service is left
//! >    with neither: its task sets are its caller's to create.
//! > 2. Whichever of those two halves a service reports, it is DERIVED from what is
//! >    stored: a count this API does take moves the task set's `computedDesiredCount`
//! >    with it, and a member this API refuses on such a service
//! >    (`taskDefinition`, `networkConfiguration`) leaves both the service and its task
//! >    set exactly as they were.
//! > 3. A cluster reports back the `capacityProviders` its create associated, in the
//! >    order the create named them, beside the `defaultCapacityProviderStrategy` over
//! >    them, while its status is `ACTIVE`.
//!
//! What grounds them: `DeploymentController$type` describes the blue/green deployment
//! CodeDeploy drives ("CodeDeploy installs an updated version of the application as a new
//! replacement task set and reroutes production traffic from the original application task
//! set to the replacement task set"), so the ORIGINAL task set is one Amazon ECS already
//! has; `TaskSet$status` `PRIMARY` is "The task set is serving production traffic" and
//! `TaskSet$computedDesiredCount` "is calculated by multiplying the service's desiredCount
//! by the task set's scale percentage"; `UpdateService`'s own doc: "For services using the
//! blue/green (CODE_DEPLOY) deployment controller, only the desired count, deployment
//! configuration, health check grace period, task placement constraints and strategies,
//! enable ECS managed tags option, and propagate tags can be updated using this API. If the
//! network configuration, platform version, task definition, or load balancer need to be
//! updated, create a new CodeDeploy deployment"; and `CreateCluster$capacityProviders` is
//! "The short name of one or more capacity providers to associate with the cluster", which
//! `Cluster$capacityProviders` is the list of.
//!
//! The rows span the `DeploymentControllerType` catalog
//! (`smithy_query(query="shape",shape="DeploymentControllerType")`: `ECS`, `CODE_DEPLOY`,
//! `EXTERNAL`) crossed with the desired count a create asks for, and — for the cluster —
//! the number of providers associated, their order, and whether they are the two Fargate
//! providers Amazon ECS publishes for every account or a provider of the caller's own.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, count, create_auto_scaling_group, create_cluster, list, ok, refusal,
    service, string,
};

/// The cluster the controller rows act in.
const CLUSTER: &str = "controllerrep";

/// The container the revisions below publish a port from, which is what a load-balancer
/// binding names. A `CODE_DEPLOY` service requires such a binding.
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// The sentence an update of a blue/green service earns for submitting the task definition
/// only a CodeDeploy deployment changes, as `UpdateService` words it.
const TASK_DEFINITION_IMMUTABLE: &str =
    "Unable to update task definition on services with a CODE_DEPLOY deployment controller. \
     Use AWS CodeDeploy to trigger a new deployment.";

/// The same for the network configuration of such a service.
const NETWORK_IMMUTABLE: &str =
    "Unable to update network parameters on services with a CODE_DEPLOY deployment \
     controller. Use AWS CodeDeploy to trigger a new deployment.";

/// Register one `bridge` revision of `family` whose container publishes [`CONTAINER_PORT`],
/// and answer its ARN. `memory` distinguishes one revision of the family from the next.
async fn register_bound_definition(
    router: &MultiServiceRouter,
    family: &str,
    memory: i64,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "bridge",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": "public.ecr.aws/nginx/nginx:latest",
                "memory": memory,
                "portMappings": [{ "containerPort": CONTAINER_PORT }],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// The one service `name` names in `CLUSTER`, read back through `DescribeServices`.
async fn described_service(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    let services = list(&body, "services", case);
    services
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {name} to be described, got {body}"))
}

/// The `(status, taskDefinition)` pairs a service reports for its deployments, in order.
fn deployment_views(reported: &Value, case: &str) -> Vec<(String, String)> {
    list(reported, "deployments", case)
        .iter()
        .map(|deployment| {
            (
                string(deployment, "status", case),
                string(deployment, "taskDefinition", case),
            )
        })
        .collect()
}

/// The `(status, computedDesiredCount, taskDefinition)` triples a service reports for its
/// task sets, in order — the three members the blue/green representation is read by.
fn task_set_views(reported: &Value, case: &str) -> Vec<(String, i64, String)> {
    list(reported, "taskSets", case)
        .iter()
        .map(|task_set| {
            (
                string(task_set, "status", case),
                count(task_set, "computedDesiredCount", case),
                string(task_set, "taskDefinition", case),
            )
        })
        .collect()
}

/// One row of the controller table: a service under one controller, at one desired count.
struct ControllerRow {
    case: &'static str,
    /// The service's name, which is also the family of the revision registered for it.
    name: &'static str,
    /// `deploymentController.type`.
    controller: &'static str,
    /// The `desiredCount` the create asks for.
    desired: i64,
    /// Whether the create names a `taskDefinition` at all: an `EXTERNAL` service's revision
    /// is its task set's, so such a create names none ("you can specify only parameters
    /// that aren't controlled at the task set level").
    names_a_revision: bool,
}

#[tokio::test]
async fn a_services_controller_decides_whether_it_reports_deployments_or_task_sets() {
    // BOTH stores: a blue/green service requires a load-balancer binding, and a binding
    // names a target group read from the Elastic Load Balancing v2 service's own store.
    let router = crate::load_balancer_target_groups::emulator();
    cluster_with_capacity(&router, CLUSTER, "setup").await;

    let rows: [ControllerRow; 6] = [
        ControllerRow {
            case: "the rolling-update controller maintaining no task",
            name: "rollingzero",
            controller: "ECS",
            desired: 0,
            names_a_revision: true,
        },
        ControllerRow {
            case: "the rolling-update controller maintaining three tasks",
            name: "rollingthree",
            controller: "ECS",
            desired: 3,
            names_a_revision: true,
        },
        ControllerRow {
            case: "the blue/green controller maintaining no task",
            name: "bluegreenzero",
            controller: "CODE_DEPLOY",
            desired: 0,
            names_a_revision: true,
        },
        ControllerRow {
            case: "the blue/green controller maintaining one task",
            name: "bluegreenone",
            controller: "CODE_DEPLOY",
            desired: 1,
            names_a_revision: true,
        },
        ControllerRow {
            case: "the blue/green controller maintaining three tasks",
            name: "bluegreenthree",
            controller: "CODE_DEPLOY",
            desired: 3,
            names_a_revision: true,
        },
        ControllerRow {
            case: "the external controller, whose task sets are its caller's",
            name: "externalnone",
            controller: "EXTERNAL",
            desired: 2,
            names_a_revision: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut request = json!({
            "cluster": CLUSTER,
            "serviceName": row.name,
            "desiredCount": row.desired,
            "deploymentController": { "type": row.controller },
        });
        if row.names_a_revision {
            register_bound_definition(&router, row.name, 128, case).await;
            request["taskDefinition"] = json!(row.name);
        }
        if row.controller == "CODE_DEPLOY" {
            // The traffic CodeDeploy shifts is a load balancer's, and a `bridge` revision's
            // tasks are reached at the container instance that runs them, so the group the
            // binding names is an `instance` one — one per service.
            let target_group = crate::load_balancer_target_groups::create_target_group(
                &router,
                &format!("{}-tg", row.name),
                "instance",
                case,
            )
            .await;
            request["loadBalancers"] = json!([{
                "targetGroupArn": target_group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }]);
        }

        let created = ok(&router, "CreateService", request, case).await;
        let created = created["service"].clone();
        // ROUND TRIP: the create and the read report the one stored record.
        let read = described_service(&router, row.name, case).await;
        for (source, reported) in [("the create", &created), ("the read", &read)] {
            let deployments = deployment_views(reported, case);
            let task_sets = task_set_views(reported, case);
            match row.controller {
                // The ECS scheduler rolls the service out itself, so what it reports is
                // the rollout serving production traffic, running the service's revision.
                "ECS" => {
                    let revision = string(reported, "taskDefinition", case);
                    assert_eq!(
                        deployments,
                        vec![("PRIMARY".to_string(), revision)],
                        "case {case:?}: expected {source} to report one PRIMARY deployment \
                         of the service's revision, got {reported}"
                    );
                    assert!(
                        task_sets.is_empty(),
                        "case {case:?}: expected {source} to report no task set for a \
                         service its own scheduler deploys, got {reported}"
                    );
                }
                // The blue/green controller deploys through task sets, so the service
                // reports the original application task set and no deployment at all.
                "CODE_DEPLOY" => {
                    let revision = string(reported, "taskDefinition", case);
                    assert_eq!(
                        task_sets,
                        vec![("PRIMARY".to_string(), row.desired, revision)],
                        "case {case:?}: expected {source} to report one PRIMARY task set of \
                         the service's revision at the service's count, got {reported}"
                    );
                    assert!(
                        deployments.is_empty(),
                        "case {case:?}: expected {source} to report no deployment for a \
                         service a controller deploys, got {reported}"
                    );
                    let scale = &list(reported, "taskSets", case)[0]["scale"];
                    assert_eq!(
                        (scale["value"].as_f64(), scale["unit"].as_str()),
                        (Some(100.0), Some("PERCENT")),
                        "case {case:?}: expected {source} to scale the original task set to \
                         the whole of the service's count, got {reported}"
                    );
                }
                // Nothing deploys an EXTERNAL service until its caller creates a task set.
                _ => {
                    assert!(
                        deployments.is_empty() && task_sets.is_empty(),
                        "case {case:?}: expected {source} to report neither a deployment \
                         nor a task set, got {reported}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_blue_green_services_task_set_follows_the_count_this_api_takes_and_nothing_it_refuses() {
    let router = crate::load_balancer_target_groups::emulator();
    cluster_with_capacity(&router, CLUSTER, "setup").await;
    let family = "bluegreenfollow";
    let first = register_bound_definition(&router, family, 128, "setup").await;
    let target_group = crate::load_balancer_target_groups::create_target_group(
        &router,
        "bluegreenfollow-tg",
        "instance",
        "setup",
    )
    .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": family,
            "taskDefinition": family,
            "desiredCount": 1,
            "deploymentController": { "type": "CODE_DEPLOY" },
            "loadBalancers": [{
                "targetGroupArn": target_group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        }),
        "setup",
    )
    .await;
    // A second revision of the same family, for the update that submits one.
    let second = register_bound_definition(&router, family, 256, "setup").await;

    // The counts this API DOES take, including the one that takes the service back to
    // none: each is the service's standing instruction from that call on, and the original
    // task set is scaled to the whole of it, so its computed count follows.
    for (case, desired) in [
        ("a count above the one the create asked for", 4),
        ("a count below it", 2),
        ("no task at all", 0),
    ] {
        let updated = ok(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": family,
                "desiredCount": desired,
                "healthCheckGracePeriodSeconds": 30,
            }),
            case,
        )
        .await;
        assert_eq!(
            count(&updated["service"], "desiredCount", case),
            desired,
            "case {case:?}: expected the count this API takes to be applied, got {updated}"
        );
        let read = described_service(&router, family, case).await;
        assert_eq!(
            task_set_views(&read, case),
            vec![("PRIMARY".to_string(), desired, first.clone())],
            "case {case:?}: expected the original task set to follow the service's count \
             while still running its revision, got {read}"
        );
    }

    // And the members this API REFUSES on such a service change neither the service nor
    // the task set that serves its traffic — whichever of them the request submits, and
    // whether the revision it names is the one the service already runs or another.
    let rows: [(&str, Value, &str); 3] = [
        (
            "the revision the service already runs",
            json!({ "taskDefinition": family }),
            TASK_DEFINITION_IMMUTABLE,
        ),
        (
            "another revision of the same family",
            json!({ "taskDefinition": second }),
            TASK_DEFINITION_IMMUTABLE,
        ),
        (
            "a network configuration",
            json!({ "networkConfiguration": crate::ecs_wire::awsvpc_network_configuration() }),
            NETWORK_IMMUTABLE,
        ),
    ];
    for (case, member, expected) in rows {
        let mut request = json!({ "cluster": CLUSTER, "service": family });
        for (name, value) in member.as_object().expect("the member is an object") {
            request[name] = value.clone();
        }
        let (code, message) = refusal(&router, "UpdateService", request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("InvalidParameterException", expected),
            "case {case:?}: expected the sentence the submitted member earns, got {code:?} \
             with {message:?}"
        );
        let read = described_service(&router, family, case).await;
        assert_eq!(
            string(&read, "taskDefinition", case),
            first,
            "case {case:?}: expected the refused update to leave the service's revision \
             alone, got {read}"
        );
        assert!(
            read["networkConfiguration"].is_null(),
            "case {case:?}: expected the refused update to write no network configuration, \
             got {read}"
        );
        assert_eq!(
            task_set_views(&read, case),
            vec![("PRIMARY".to_string(), 0, first.clone())],
            "case {case:?}: expected the refused update to leave the original task set \
             alone, got {read}"
        );
    }
}

/// One row of the cluster table: the providers a create associates, and the order it names
/// them in.
struct ProviderRow {
    case: &'static str,
    cluster: &'static str,
    /// The `capacityProviders` the create submits, which is what the cluster must report.
    providers: &'static [&'static str],
}

/// The caller's OWN capacity provider one row associates beside a managed one — a name the
/// reported defect did not cite, and one nothing publishes for an account: it is created
/// over an Auto Scaling group of the caller's first.
const OWN_PROVIDER: &str = "ownprovider";

/// The two capacity providers Amazon ECS publishes for every account, from
/// `CreateCluster$capacityProviders`: "To use a Fargate capacity provider, specify either
/// the FARGATE or FARGATE_SPOT capacity providers." Nothing creates them.
const MANAGED_PROVIDERS: [&str; 2] = ["FARGATE", "FARGATE_SPOT"];

#[tokio::test]
async fn a_cluster_reports_the_capacity_providers_its_create_associated() {
    let router = service();
    create_auto_scaling_group(&router, "asg-own", "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": OWN_PROVIDER,
            "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-own" },
        }),
        "setup",
    )
    .await;

    let rows: [ProviderRow; 5] = [
        ProviderRow {
            case: "both of the Fargate providers",
            cluster: "bothfargate",
            providers: &MANAGED_PROVIDERS,
        },
        ProviderRow {
            case: "the same two named in the other order",
            cluster: "reversedfargate",
            providers: &["FARGATE_SPOT", "FARGATE"],
        },
        ProviderRow {
            case: "one Fargate provider alone",
            cluster: "onefargate",
            providers: &["FARGATE"],
        },
        ProviderRow {
            case: "a provider of the caller's own beside a published one",
            cluster: "ownbeside",
            providers: &[OWN_PROVIDER, "FARGATE"],
        },
        ProviderRow {
            case: "no provider at all",
            cluster: "noprovider",
            providers: &[],
        },
    ];

    for row in &rows {
        let case = row.case;
        // Every associated provider is also what the cluster's default strategy may name,
        // so the strategy a row submits is the one over its own providers.
        let strategy: Vec<Value> = row
            .providers
            .iter()
            .map(|provider| json!({ "capacityProvider": provider, "weight": 1 }))
            .collect();
        let mut request = json!({ "clusterName": row.cluster });
        if !row.providers.is_empty() {
            request["capacityProviders"] = json!(row.providers);
            request["defaultCapacityProviderStrategy"] = json!(strategy);
        }
        let created = ok(&router, "CreateCluster", request, case).await;

        let described = ok(
            &router,
            "DescribeClusters",
            json!({ "clusters": [row.cluster] }),
            case,
        )
        .await;
        let clusters = list(&described, "clusters", case);
        let read = clusters.first().unwrap_or_else(|| {
            panic!("case {case:?}: expected the cluster to be described, got {described}")
        });

        let expected: Vec<Value> = row.providers.iter().map(|name| json!(name)).collect();
        let expected_strategy: Vec<(String, i64)> = row
            .providers
            .iter()
            .map(|name| ((*name).to_string(), 1))
            .collect();
        for (source, reported) in [("the create", &created["cluster"]), ("the read", read)] {
            assert_eq!(
                string(reported, "status", case),
                "ACTIVE",
                "case {case:?}: expected {source} to report the cluster ACTIVE, got {reported}"
            );
            assert_eq!(
                list(reported, "capacityProviders", case),
                expected,
                "case {case:?}: expected {source} to report the providers the create \
                 associated, got {reported}"
            );
            let strategy: Vec<(String, i64)> =
                list(reported, "defaultCapacityProviderStrategy", case)
                    .iter()
                    .map(|item| {
                        (
                            string(item, "capacityProvider", case),
                            count(item, "weight", case),
                        )
                    })
                    .collect();
            assert_eq!(
                strategy, expected_strategy,
                "case {case:?}: expected {source} to report the default strategy the create \
                 set, got {reported}"
            );
        }
    }

    // A cluster created with no provider is not a cluster that cannot have one: the
    // association is what puts the providers on it, and the cluster reports them after it.
    let case = "a provider associated after the create";
    create_cluster(&router, "associatedlater", case).await;
    let associated = ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "associatedlater",
            "capacityProviders": MANAGED_PROVIDERS,
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    assert_eq!(
        list(&associated["cluster"], "capacityProviders", case),
        MANAGED_PROVIDERS
            .iter()
            .map(|name| json!(name))
            .collect::<Vec<Value>>(),
        "case {case:?}: expected the association to be reported back, got {associated}"
    );
}
