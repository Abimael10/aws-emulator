// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECS services a deployment group TARGETS, resolved against the
//! service that owns them, driven over the wire through the same front door a
//! client uses (the harness in [`crate::codedeploy_lifecycle`], with Amazon ECS
//! mounted beside CodeDeploy on one engine).
//!
//! The rule every case here checks: an `ecsServices` entry names a cluster and a
//! service of ANOTHER service's records, so a deployment group is created or
//! updated with it only when that service can LOAD the pair — otherwise the call
//! is refused `InvalidECSServiceException` (HTTP 400), stating the pair the request
//! named and the cause its owner gave, and the store is left exactly as it was.
//!
//! The table spans what the rule generalises over: which half of the pair is
//! unknown, how many pairs one request names, which of them is the bad one, which
//! cluster a service lives in, and whether the group is being created or updated.
//!
//! Every service the fixture creates is deployed by CodeDeploy, which is the other
//! precondition a pair has to satisfy — it has its own suite
//! (`ecs_service_deployment_controller.rs`, which builds its services out of the
//! fixture here) — so LOADABILITY is the only thing these rows vary.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, Emulator};
use crate::server_platform_instance_deployments::element;

/// The cluster the fixture creates, holding [`WEB_SERVICE`].
const PROD_CLUSTER: &str = "prod";

/// A second cluster, holding [`API_SERVICE`] — so a case can tell "the account has
/// no such service" from "the account has it in another cluster".
const STAGING_CLUSTER: &str = "staging";

/// The service [`PROD_CLUSTER`] holds.
const WEB_SERVICE: &str = "web";

/// The service [`STAGING_CLUSTER`] holds.
const API_SERVICE: &str = "api";

/// The modelled error a pair that could not be loaded is refused with.
const INVALID_ECS_SERVICE: &str = "InvalidECSServiceException";

/// Amazon ECS's own sentence for a cluster reference it holds no cluster for,
/// which is the cause CodeDeploy relays then
/// (`state::CLUSTER_NOT_FOUND_MESSAGE` of that service).
const CLUSTER_NOT_FOUND: &str = "Cluster not found.";

/// Amazon ECS's own sentence for a service it holds none of, which is the cause
/// for a name a cluster the account DOES have does not carry (that service's
/// `state::SERVICE_NOT_FOUND_MESSAGE`).
const SERVICE_NOT_FOUND: &str = "Service not found.";

/// The deployment controller type that hands a service's deployments to CodeDeploy,
/// which is the one a deployment group may target
/// (`DeploymentControllerType` of the Amazon ECS model: `ECS`, `CODE_DEPLOY`,
/// `EXTERNAL`).
pub(crate) const CODE_DEPLOY_CONTROLLER: &str = "CODE_DEPLOY";

/// The container and the port every service the fixture creates serves on.
const CONTAINER: &str = "web";
const CONTAINER_PORT: i64 = 80;

/// The network and the load balancer every service the fixture creates sits in and
/// behind: a service deployed by CodeDeploy shifts traffic between Elastic Load
/// Balancing v2 target groups, so Amazon ECS requires it to be bound to one that a
/// load balancer forwards to.
pub(crate) struct Network {
    /// The Amazon VPC the target groups and the service's tasks are in.
    vpc: String,
    /// The two subnets the load balancer and the tasks are placed in.
    subnets: Vec<String>,
    /// The security group the tasks and the load balancer sit in.
    group: String,
    /// The load balancer whose listeners make a target group reachable.
    load_balancer: String,
}

/// Create that network through Amazon EC2's and Elastic Load Balancing v2's OWN
/// front doors, so every id below is one those services minted.
///
/// `name` keeps one emulator's resources apart from another's.
pub(crate) async fn given_network(emulator: &Emulator, name: &str) -> Network {
    let vpc = read(
        &emulator.ec2_ok("CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    // An internet-facing load balancer sits in PUBLIC subnets, so the network gets
    // the internet gateway and the default route Elastic Load Balancing requires of
    // them.
    let gateway = read(
        &emulator.ec2_ok("CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    emulator
        .ec2_ok(
            "AttachInternetGateway",
            &[
                ("InternetGatewayId", gateway.as_str()),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await;
    let mut subnets = Vec::new();
    for (cidr, zone) in [
        ("10.90.1.0/24", "us-east-1a"),
        ("10.90.2.0/24", "us-east-1b"),
    ] {
        subnets.push(read(
            &emulator
                .ec2_ok(
                    "CreateSubnet",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("CidrBlock", cidr),
                        ("AvailabilityZone", zone),
                    ],
                )
                .await,
            "subnetId",
        ));
    }
    let route_table = read(
        &emulator
            .ec2_ok("CreateRouteTable", &[("VpcId", vpc.as_str())])
            .await,
        "routeTableId",
    );
    emulator
        .ec2_ok(
            "CreateRoute",
            &[
                ("RouteTableId", route_table.as_str()),
                ("DestinationCidrBlock", "0.0.0.0/0"),
                ("GatewayId", gateway.as_str()),
            ],
        )
        .await;
    for subnet in &subnets {
        emulator
            .ec2_ok(
                "AssociateRouteTable",
                &[
                    ("RouteTableId", route_table.as_str()),
                    ("SubnetId", subnet.as_str()),
                ],
            )
            .await;
    }
    let group = read(
        &emulator
            .ec2_ok(
                "CreateSecurityGroup",
                &[
                    ("GroupName", &format!("{name}-group")),
                    ("GroupDescription", "the tasks and the load balancer"),
                    ("VpcId", vpc.as_str()),
                ],
            )
            .await,
        "groupId",
    );
    let load_balancer = read(
        &emulator
            .elbv2_ok(
                "CreateLoadBalancer",
                &[
                    ("Name", &format!("{name}-alb")),
                    ("Subnets.member.1", subnets[0].as_str()),
                    ("Subnets.member.2", subnets[1].as_str()),
                    ("SecurityGroups.member.1", group.as_str()),
                ],
            )
            .await,
        "LoadBalancerArn",
    );
    Network {
        vpc,
        subnets,
        group,
        load_balancer,
    }
}

/// The text of the first `<tag>` of a query-protocol answer, which every id above
/// is read out of.
fn read(body: &str, tag: &str) -> String {
    element(body, tag).unwrap_or_else(|| panic!("no <{tag}> in {body}"))
}

/// Create `cluster` and one service named `service` in it, deployed by
/// `controller`, through Amazon ECS's OWN front door — never by writing into its
/// store — answering the service ARN that service minted.
///
/// `slot` keeps each service's target group and listener apart from the others' on
/// the one load balancer.
///
/// The smallest service that API accepts for the controller it names: a desired
/// count of zero, so nothing is placed and no transition is owed, and — for a
/// controller that deploys the service's own revision — a registered task
/// definition bound to a target group a listener forwards to, which is what Amazon
/// ECS requires of a service CodeDeploy deploys.
pub(crate) async fn given_ecs_service(
    emulator: &Emulator,
    network: &Network,
    cluster: &str,
    service: &str,
    controller: &str,
    slot: usize,
) -> String {
    emulator
        .ecs_ok("CreateCluster", json!({"clusterName": cluster}))
        .await;
    // "This is used when a service uses the EXTERNAL deployment controller type":
    // such a service takes the revision its tasks run from the task set its caller
    // creates, so it names no task definition and binds to no load balancer.
    if controller == "EXTERNAL" {
        let created = emulator
            .ecs_ok(
                "CreateService",
                json!({
                    "cluster": cluster,
                    "serviceName": service,
                    "desiredCount": 0,
                    "deploymentController": {"type": controller},
                }),
            )
            .await;
        return service_arn(&created, cluster, service);
    }
    let family = format!("{cluster}-{service}");
    emulator
        .ecs_ok(
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": "public.ecr.aws/nginx/nginx:latest",
                    "portMappings": [{"containerPort": CONTAINER_PORT, "protocol": "tcp"}],
                }],
            }),
        )
        .await;
    // The target group the service registers its tasks into, and the listener that
    // makes it reachable: Amazon ECS refuses a binding to a group no load balancer
    // forwards to, and a revision in the `awsvpc` network mode needs the `ip`
    // target type.
    let target_group = read(
        &emulator
            .elbv2_ok(
                "CreateTargetGroup",
                &[
                    ("Name", &format!("tg-{slot}")),
                    ("Protocol", "HTTP"),
                    ("Port", &CONTAINER_PORT.to_string()),
                    ("TargetType", "ip"),
                    ("VpcId", network.vpc.as_str()),
                    ("HealthCheckProtocol", "HTTP"),
                    ("HealthCheckPath", "/"),
                ],
            )
            .await,
        "TargetGroupArn",
    );
    emulator
        .elbv2_ok(
            "CreateListener",
            &[
                ("LoadBalancerArn", network.load_balancer.as_str()),
                ("Protocol", "HTTP"),
                ("Port", &(8000 + slot).to_string()),
                ("DefaultActions.member.1.Type", "forward"),
                ("DefaultActions.member.1.TargetGroupArn", target_group.as_str()),
            ],
        )
        .await;
    let created = emulator
        .ecs_ok(
            "CreateService",
            json!({
                "cluster": cluster,
                "serviceName": service,
                "taskDefinition": family,
                "desiredCount": 0,
                "launchType": "FARGATE",
                "deploymentController": {"type": controller},
                "loadBalancers": [{
                    "targetGroupArn": target_group,
                    "containerName": CONTAINER,
                    "containerPort": CONTAINER_PORT,
                }],
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": network.subnets,
                        "securityGroups": [network.group],
                    }
                },
            }),
        )
        .await;
    service_arn(&created, cluster, service)
}

/// The ARN Amazon ECS minted for the service it just created.
fn service_arn(created: &Value, cluster: &str, service: &str) -> String {
    created["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateService {cluster}/{service} answered no serviceArn"))
        .to_string()
}

/// An emulator holding both clusters above with their one CodeDeploy-deployed
/// service each, and a CodeDeploy application on the Amazon ECS compute platform,
/// answering it beside the ARN Amazon ECS minted for the service of
/// [`PROD_CLUSTER`].
async fn fixture(application: &str) -> (Emulator, String) {
    let emulator = Emulator::with_ecs();
    let network = given_network(&emulator, "resolution").await;
    let web_arn = given_ecs_service(
        &emulator,
        &network,
        PROD_CLUSTER,
        WEB_SERVICE,
        CODE_DEPLOY_CONTROLLER,
        1,
    )
    .await;
    given_ecs_service(
        &emulator,
        &network,
        STAGING_CLUSTER,
        API_SERVICE,
        CODE_DEPLOY_CONTROLLER,
        2,
    )
    .await;
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "ECS"}),
        )
        .await;
    (emulator, web_arn)
}

/// One `ecsServices` entry, as a request names it.
fn pair(cluster: &str, service: &str) -> Value {
    json!({"clusterName": cluster, "serviceName": service})
}

/// A `CreateDeploymentGroup` request naming `ecs_services`.
///
/// Every group here belongs to an application on the Amazon ECS compute platform,
/// so the request carries that platform's own preconditions — the blue/green,
/// traffic-controlled style and the load balancer it routes traffic behind —
/// leaving the pair the row names as the only thing under test.
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
    let read = emulator
        .ok(
            "GetDeploymentGroup",
            json!({"applicationName": application, "deploymentGroupName": group}),
        )
        .await;
    read["deploymentGroupInfo"]["ecsServices"].clone()
}

/// The deployment group names `ListDeploymentGroups` reports for `application`.
async fn listed_groups(emulator: &Emulator, application: &str) -> Vec<String> {
    let listed = emulator
        .ok(
            "ListDeploymentGroups",
            json!({"applicationName": application}),
        )
        .await;
    listed["deploymentGroups"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Assert `message` is the refusal for `(cluster, service)` with `cause`, with the
/// relayed-call clause the owner's own refusal is reported inside when it refused
/// the load.
///
/// `relayed` carries the owner's `(status, error code)` for a cause it REFUSED the
/// load with; a cause the owner answered rather than refused carries none, so the
/// sentence ends with it. The request id inside the clause belongs to the relayed
/// call, so only its FORM is asserted.
fn assert_refusal(
    case: &str,
    message: &str,
    cluster: &str,
    service: &str,
    cause: &str,
    relayed: Option<(u16, &str)>,
) {
    let head = format!(
        "Could not load ECS service information for cluster: {cluster}, service: {service}. \
         Cause: {cause}"
    );
    let Some((status, code)) = relayed else {
        assert_eq!(
            message, head,
            "case {case:?}: expected {head:?}, got {message:?}"
        );
        return;
    };
    let clause = format!(" (Service: AmazonECS; Status Code: {status}; Error Code: {code}; ");
    let opening = format!("{head}{clause}Request ID: ");
    let closing = "; Proxy: null)";
    assert!(
        message.starts_with(&opening) && message.ends_with(closing),
        "case {case:?}: expected a message opening {opening:?} and closing {closing:?}, got \
         {message:?}"
    );
    let request_id = message[opening.len()..message.len() - closing.len()].to_string();
    assert!(
        ::service_common::query_error_envelope::is_request_id(&request_id),
        "case {case:?}: expected the relayed call's own request id, got {request_id:?}"
    );
}

/// What a refusal states: the cluster and the service it names, the cause, and the
/// owner's `(status, error code)` when it REFUSED the load rather than answering it.
type Refusal = (
    &'static str,
    &'static str,
    &'static str,
    Option<(u16, &'static str)>,
);

/// One row of the create table: the `ecsServices` a request names and what the
/// service answers for it.
struct CreateRow {
    case: &'static str,
    group: &'static str,
    /// The entries the request names, or `None` for a request naming the member
    /// not at all.
    ecs_services: Option<Value>,
    /// `None` for a request the service serves; otherwise the refusal it earns.
    refused: Option<Refusal>,
}

#[tokio::test]
async fn a_deployment_group_is_created_only_for_ecs_pairs_amazon_ecs_can_load() {
    const APPLICATION: &str = "ecs-resolution-app";
    let (emulator, web_arn) = fixture(APPLICATION).await;

    let rows = vec![
        CreateRow {
            case: "a pair the account holds",
            group: "dg-held",
            ecs_services: Some(json!([pair(PROD_CLUSTER, WEB_SERVICE)])),
            refused: None,
        },
        CreateRow {
            case: "a held pair naming its service by the ARN Amazon ECS minted",
            group: "dg-held-by-arn",
            ecs_services: Some(json!([pair(PROD_CLUSTER, &web_arn)])),
            refused: None,
        },
        CreateRow {
            case: "a pair the account holds in its other cluster",
            group: "dg-other-cluster",
            ecs_services: Some(json!([pair(STAGING_CLUSTER, API_SERVICE)])),
            refused: None,
        },
        CreateRow {
            case: "the same held pair named twice",
            group: "dg-repeated",
            ecs_services: Some(json!([
                pair(PROD_CLUSTER, WEB_SERVICE),
                pair(PROD_CLUSTER, WEB_SERVICE),
            ])),
            refused: None,
        },
        CreateRow {
            case: "both held pairs",
            group: "dg-both",
            ecs_services: Some(json!([
                pair(PROD_CLUSTER, WEB_SERVICE),
                pair(STAGING_CLUSTER, API_SERVICE),
            ])),
            refused: None,
        },
        CreateRow {
            case: "no ecsServices at all",
            group: "dg-none",
            ecs_services: None,
            refused: None,
        },
        CreateRow {
            case: "an empty ecsServices list",
            group: "dg-empty",
            ecs_services: Some(json!([])),
            refused: None,
        },
        CreateRow {
            case: "a cluster the account does not have",
            group: "dg-ghost-cluster",
            ecs_services: Some(json!([pair("ghost-cluster", "ghost-service")])),
            refused: Some((
                "ghost-cluster",
                "ghost-service",
                CLUSTER_NOT_FOUND,
                Some((400, "ClusterNotFoundException")),
            )),
        },
        CreateRow {
            case: "a held cluster and a service it does not hold",
            group: "dg-ghost-service",
            ecs_services: Some(json!([pair(PROD_CLUSTER, "ghost-service")])),
            refused: Some((PROD_CLUSTER, "ghost-service", SERVICE_NOT_FOUND, None)),
        },
        CreateRow {
            case: "a service of the OTHER cluster, named under this one",
            group: "dg-cross-cluster",
            ecs_services: Some(json!([pair(PROD_CLUSTER, API_SERVICE)])),
            refused: Some((PROD_CLUSTER, API_SERVICE, SERVICE_NOT_FOUND, None)),
        },
        CreateRow {
            case: "a held pair first and an unknown cluster second",
            group: "dg-second-bad",
            ecs_services: Some(json!([
                pair(PROD_CLUSTER, WEB_SERVICE),
                pair("ghost-cluster-2", WEB_SERVICE),
            ])),
            refused: Some((
                "ghost-cluster-2",
                WEB_SERVICE,
                CLUSTER_NOT_FOUND,
                Some((400, "ClusterNotFoundException")),
            )),
        },
    ];

    let mut served: Vec<&str> = Vec::new();
    for row in &rows {
        let mut request = create_request(APPLICATION, row.group, json!([]));
        match row.ecs_services.as_ref() {
            Some(entries) => request["ecsServices"] = entries.clone(),
            None => {
                request
                    .as_object_mut()
                    .expect("the request is an object")
                    .remove("ecsServices");
            }
        }
        let (status, body) = emulator.call("CreateDeploymentGroup", request).await;
        let Some((cluster, service, cause, relayed)) = row.refused else {
            assert_eq!(
                status, 200,
                "case {:?}: expected the group to be created, got {status} with {body:?}",
                row.case
            );
            served.push(row.group);
            // The pair the request named is what a read of the group reports; a
            // group that names no target reports the member not at all.
            let stored = stored_ecs_services(&emulator, APPLICATION, row.group).await;
            let expected = match row.ecs_services.as_ref() {
                Some(entries) if entries.as_array().is_some_and(|list| !list.is_empty()) => {
                    entries.clone()
                }
                _ => Value::Null,
            };
            assert_eq!(
                stored,
                expected,
                "case {:?}: expected the stored ecsServices to echo the request, got {stored:?}",
                row.case
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {:?}: expected the create to be refused, got {status} with {body:?}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            INVALID_ECS_SERVICE,
            "case {:?}: expected {INVALID_ECS_SERVICE}, got {body:?}",
            row.case
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert_refusal(row.case, message, cluster, service, cause, relayed);
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

#[tokio::test]
async fn an_update_moves_a_group_onto_ecs_pairs_amazon_ecs_can_load_and_no_others() {
    const APPLICATION: &str = "ecs-update-app";
    const GROUP: &str = "dg";
    let (emulator, _web_arn) = fixture(APPLICATION).await;
    emulator
        .ok(
            "CreateDeploymentGroup",
            create_request(APPLICATION, GROUP, json!([pair(PROD_CLUSTER, WEB_SERVICE)])),
        )
        .await;

    // (case, the replacement the update names, the refusal it earns)
    let rows: Vec<(&str, Value, Option<Refusal>)> = vec![
        (
            "onto a cluster the account does not have",
            json!([pair("ghost-cluster", WEB_SERVICE)]),
            Some((
                "ghost-cluster",
                WEB_SERVICE,
                CLUSTER_NOT_FOUND,
                Some((400, "ClusterNotFoundException")),
            )),
        ),
        (
            "onto a service a held cluster does not hold",
            json!([pair(STAGING_CLUSTER, WEB_SERVICE)]),
            Some((STAGING_CLUSTER, WEB_SERVICE, SERVICE_NOT_FOUND, None)),
        ),
        (
            "onto a pair the account holds",
            json!([pair(STAGING_CLUSTER, API_SERVICE)]),
            None,
        ),
    ];

    // The targets the group holds, which a refused update must leave alone.
    let mut held = json!([pair(PROD_CLUSTER, WEB_SERVICE)]);
    for (case, replacement, refusal) in rows {
        let (status, body) = emulator
            .call(
                "UpdateDeploymentGroup",
                json!({
                    "applicationName": APPLICATION,
                    "currentDeploymentGroupName": GROUP,
                    "ecsServices": replacement.clone(),
                }),
            )
            .await;
        match refusal {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the update to be served, got {status} with {body:?}"
                );
                held = replacement;
            }
            Some((cluster, service, cause, relayed)) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the update to be refused, got {status} with {body:?}"
                );
                assert_eq!(
                    error_code(&body),
                    INVALID_ECS_SERVICE,
                    "case {case:?}: expected {INVALID_ECS_SERVICE}, got {body:?}"
                );
                let message = body["message"].as_str().unwrap_or_default();
                assert_refusal(case, message, cluster, service, cause, relayed);
            }
        }
        let stored = stored_ecs_services(&emulator, APPLICATION, GROUP).await;
        assert_eq!(
            stored, held,
            "case {case:?}: expected the group to hold {held:?}, got {stored:?}"
        );
    }
}

/// A pair is resolved against the service that OWNS it, so an emulator that mounts
/// no Amazon ECS at all can answer nothing about one: the load cannot be made, and
/// a gate refuses nothing it did not resolve.
#[tokio::test]
async fn a_pair_is_not_refused_where_amazon_ecs_cannot_be_asked_at_all() {
    const APPLICATION: &str = "ecs-unasked-app";
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": APPLICATION, "computePlatform": "ECS"}),
        )
        .await;
    let case = "no Amazon ECS mounted beside CodeDeploy";
    let (status, body) = emulator
        .call(
            "CreateDeploymentGroup",
            create_request(
                APPLICATION,
                "dg-unasked",
                json!([pair("any-cluster", "any-service")]),
            ),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create to be served, got {status} with {body:?}"
    );
    let stored = stored_ecs_services(&emulator, APPLICATION, "dg-unasked").await;
    assert_eq!(
        stored,
        json!([pair("any-cluster", "any-service")]),
        "case {case:?}: expected the named pair to be stored, got {stored:?}"
    );
}
