// SPDX-License-Identifier: CC0-1.0

//! A CodeDeploy deployment of an Amazon ECS service shifts the production route IN STAGES:
//! what a client sees of the route, of the two task sets and of the deployment WHILE one
//! increment's window is open.
//!
//! RULE: a service whose `deploymentController.type` is `CODE_DEPLOY` is one Amazon ECS runs
//! the original application task set of itself, so its `runningCount` reaches its
//! `desiredCount` and each running task is registered in the target group its `loadBalancers`
//! binding names, with no `CreateTaskSet` call; a deployment of that service then installs the
//! replacement task set behind the other group of its deployment group's pair and rewrites the
//! `prodTrafficRoute` listeners "in increments" — "CodeDeploy installs an updated version of
//! the application as a new replacement task set and reroutes production traffic from the
//! original application task set to the replacement task set" (`CreateDeployment`) in the
//! increments the deployment's own `trafficRoutingConfig` describes. So for every
//! configuration that describes MORE than one increment — a `TimeBasedCanary`, whose
//! `canaryPercentage` moves first and whose remainder moves `canaryInterval` minutes later, or
//! a `TimeBasedLinear`, which moves `linearPercentage` "at the start of each increment" — each
//! increment but the last leaves the route SPLIT, that increment's own share on the replacement
//! group and the rest on the original, with the original task set still the one serving
//! production traffic (`PRIMARY`) and the deployment still in progress for the whole of the
//! window its configuration asked for.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here: Amazon EC2
//! owns the network the tasks are placed in, Amazon ECS owns the service, its task sets and
//! their tasks, Elastic Load Balancing v2 holds the listener the production route IS, and
//! CodeDeploy drives the deployment. Everything is driven through the real operations over ONE
//! shared engine, so what is asserted is what each service persisted.
//!
//! `codedeploy_ecs_blue_green_moves_the_production_listener.rs` asserts where a deployment
//! ENDS — the whole route on the replacement group, the replacement promoted and the original
//! terminated — and deliberately does not catch a listener inside one increment's own window.
//! This suite asserts the window itself, which is what a client staging a release reads: the
//! rows differ along the configuration's traffic routing (a predefined canary built-in, a
//! canary of this account's own and a linear shift of its own), along the share that
//! configuration's first increment moves, and along how many tasks the service runs. Each row
//! states an interval far longer than any test run, so the stage it observes is read inside a
//! window that is still open rather than raced against a span passing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and keyed-list pairing this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{
    block_of, element, elements, form_encode, members_of, opt_element, pairs_of,
};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The ECS cluster, service, task-definition family and container every row runs. Each row
/// stands up an emulator of its own, so one set of names serves them all.
const CLUSTER: &str = "staged";
const SERVICE: &str = "staged-svc";
const FAMILY: &str = "stagedfamily";
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// The pair of target groups and the load balancer every row's traffic moves across.
const BLUE_GROUP: &str = "staged-blue";
const GREEN_GROUP: &str = "staged-green";
const LOAD_BALANCER: &str = "staged-alb";
const LISTENER_PORT: &str = "80";

/// The network every row runs in.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.90.1.0/24", "us-east-1a"),
    ("10.90.2.0/24", "us-east-1b"),
];

/// How long a clock-owed move is waited for, and how often it is looked at. Generous on
/// purpose: the deadline is only here so a failure is a failure rather than a hang, and every
/// wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(50);

/// `TaskSet$status`: "PRIMARY: The task set is serving production traffic", and "ACTIVE: The
/// task set isn't serving production traffic".
const PRIMARY: &str = "PRIMARY";
const ACTIVE: &str = "ACTIVE";

/// The `DeploymentStatus` of a deployment that is still carrying its deployment out
/// (`smithy_query(query="shape",shape="DeploymentStatus")`).
const IN_PROGRESS: &str = "InProgress";

/// The whole of a production route, as the percentage its shares are read as; see
/// [`production_shares`].
const WHOLE_ROUTE: i64 = 100;

/// One row: the deployment configuration the deployment runs under, the share of the route its
/// FIRST increment moves, and how many tasks the service runs.
struct Row {
    case: &'static str,
    /// The `deploymentConfigName` the deployment names.
    config: &'static str,
    /// The configuration to create first, where the row deploys under one of this account's
    /// own rather than a predefined built-in: its `TrafficRoutingType`, the percentage one
    /// increment moves, and the interval in minutes between two increments.
    own_config: Option<(&'static str, i64, i64)>,
    /// The share of the production route the first increment of [`Self::config`] moves.
    first_stage: i64,
    /// The service's `desiredCount`.
    desired: i64,
}

const ROWS: [Row; 3] = [
    // A predefined configuration of CodeDeploy's own published catalogue for the Amazon ECS
    // compute platform — `CodeDeployDefault.ECSCanary10Percent15Minutes`, which "shifts 10
    // percent of traffic in the first increment" and "the remaining 90 percent … 15 minutes
    // later" — rather than one this row creates, so the row also pins that a client naming a
    // shipped canary gets a canary.
    Row {
        case: "a predefined ECS canary built-in: a tenth of the route in its first stage",
        config: "CodeDeployDefault.ECSCanary10Percent15Minutes",
        own_config: None,
        first_stage: 10,
        desired: 1,
    },
    Row {
        case: "a canary of this account's own: nearly half the route in its first stage",
        config: "ecs-canary-45-percent-every-20-minutes",
        own_config: Some(("TimeBasedCanary", 45, 20)),
        first_stage: 45,
        desired: 2,
    },
    Row {
        case: "a linear shift of this account's own: a quarter of the route per stage",
        config: "ecs-linear-25-percent-every-20-minutes",
        own_config: Some(("TimeBasedLinear", 25, 20)),
        first_stage: 25,
        desired: 1,
    },
];

/// One emulator carrying Amazon EC2, Elastic Load Balancing v2, Amazon ECS and CodeDeploy over
/// ONE shared engine, exactly as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_codedeploy_codedeploy_20141006::EmulatedService::new(engine),
    );
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the ELBv2 model that must succeed, and answer its body.
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson1_1 call to `operation` of the service `target_prefix`/`signing_name` name,
/// answered with its status and parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDSTAGED/20260101/{REGION}\
                 /{signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must succeed.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = json_call(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        &body,
    )
    .await;
    assert_eq!(status, 200, "ECS {operation} answered {status} with {parsed}");
    parsed
}

/// One CodeDeploy call that must succeed.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) =
        json_call(router, "codedeploy", "CodeDeploy_20141006", operation, &body).await;
    assert_eq!(
        status, 200,
        "CodeDeploy {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every row runs in: a VPC with an internet gateway and a route table that sends
/// `0.0.0.0/0` to it associated with both of its subnets, and one self-referencing security
/// group the tasks and the load balancer all sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one EC2 minted.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        subnets.push(element(
            &ec2(
                router,
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
    let route_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", route_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    for subnet in &subnets {
        ec2(
            router,
            "AssociateRouteTable",
            &[
                ("RouteTableId", route_table.as_str()),
                ("SubnetId", subnet.as_str()),
            ],
        )
        .await;
    }
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "staged-group"),
                ("GroupDescription", "the tasks and the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The one rule this suite authorizes: the group takes ITSELF as a source on every port,
    // which is what lets a load balancer's check of a target in it arrive.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", "0"),
            ("IpPermissions.1.ToPort", "65535"),
            ("IpPermissions.1.Groups.1.GroupId", group.as_str()),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        group,
    }
}

/// Create the cluster and the TWO revisions of the one family: the one the service is created
/// with, and the one the deployment's AppSpec replaces it by.
async fn seed_ecs(router: &MultiServiceRouter) -> (String, String) {
    ecs_ok(router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let mut revisions = Vec::new();
    for image in [
        "public.ecr.aws/docker/library/busybox:1.36",
        "public.ecr.aws/docker/library/busybox:1.37",
    ] {
        let registered = ecs_ok(
            router,
            "RegisterTaskDefinition",
            json!({
                "family": FAMILY,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": image,
                    "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
                }],
            }),
        )
        .await;
        revisions.push(
            registered["taskDefinition"]["taskDefinitionArn"]
                .as_str()
                .unwrap_or_else(|| panic!("no taskDefinitionArn in {registered}"))
                .to_string(),
        );
    }
    (revisions[0].clone(), revisions[1].clone())
}

/// Create one `ip` target group in `network`'s VPC and answer its ARN.
async fn target_group(router: &MultiServiceRouter, network: &Network, name: &str) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
        ],
    )
    .await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn for {name} in {created}"))
}

/// Create the load balancer and the ONE listener that carries this row's production traffic,
/// forwarding to `blue`, and answer the listener's ARN.
async fn production_listener(
    router: &MultiServiceRouter,
    network: &Network,
    blue: &str,
) -> String {
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", LOAD_BALANCER),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
            ("SecurityGroups.member.1", network.group.as_str()),
        ],
    )
    .await;
    let load_balancer = element(&created, "LoadBalancerArn");
    let listener = elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", LISTENER_PORT),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", blue),
        ],
    )
    .await;
    element(&listener, "ListenerArn")
}

/// The share of the production route — as a percentage of the whole — the listener `arn`
/// forwards to each target group now; the empty map for a listener this store holds none of.
///
/// `TargetGroupTuple$Weight` is not itself a percentage: a forward action's groups split the
/// route in proportion to their weights, so the share one group takes is its weight over the
/// total of the action's weights — which is why a route reported as one group at weight 1 and
/// a route reported as one group at weight 100 are the SAME route, the whole of it on that
/// group. The share is what one stage of a shift can be compared against the next by. An
/// action that names a target group directly, carrying no weights at all, forwards the whole
/// route to it.
async fn production_shares(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, i64> {
    let described = elbv2(router, "DescribeListeners", &[("ListenerArns.member.1", arn)]).await;
    let Some(actions) = block_of(&described, "DefaultActions") else {
        return BTreeMap::new();
    };
    let mut weights: BTreeMap<String, i64> = pairs_of(actions, "TargetGroupArn", "Weight")
        .into_iter()
        .map(|(group, weight)| {
            let parsed = weight.parse::<i64>().unwrap_or_else(|_| {
                panic!("the weight {weight:?} of {group} is a number, in {described}")
            });
            (group, parsed)
        })
        .collect();
    if weights.is_empty() {
        weights = elements(actions, "TargetGroupArn")
            .into_iter()
            .map(|group| (group, 1))
            .collect();
    }
    let total: i64 = weights.values().sum();
    assert!(
        total > 0,
        "a production route forwards to a target group, got {described}"
    );
    weights
        .into_iter()
        .map(|(group, weight)| (group, weight * WHOLE_ROUTE / total))
        .collect()
}

/// Every target id the group `arn` holds a registration for.
async fn registered(router: &MultiServiceRouter, arn: &str) -> BTreeSet<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|description| {
            elements(description, "Id")
                .into_iter()
                .next()
                .filter(|id| !id.is_empty())
        })
        .collect()
}

/// The one service of this suite, as `DescribeServices` reports it.
async fn described_service(router: &MultiServiceRouter) -> Value {
    let described = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [SERVICE] }),
    )
    .await;
    described["services"][0].clone()
}

/// What one task set of the service reports: the three members the staged move is read through.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSetView {
    status: String,
    revision: String,
    running: i64,
}

/// The task sets the service holds, keyed by id.
async fn task_sets(router: &MultiServiceRouter) -> BTreeMap<String, TaskSetView> {
    described_service(router).await["taskSets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task_set| {
            Some((
                task_set["id"].as_str()?.to_string(),
                TaskSetView {
                    status: task_set["status"].as_str().unwrap_or_default().to_string(),
                    revision: task_set["taskDefinition"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    running: task_set["runningCount"].as_i64().unwrap_or_default(),
                },
            ))
        })
        .collect()
}

/// The `status` CodeDeploy reports for the deployment `id`.
async fn deployment_status(router: &MultiServiceRouter, id: &str) -> String {
    let read = codedeploy_ok(router, "GetDeployment", json!({ "deploymentId": id })).await;
    read["deploymentInfo"]["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look again until `look` holds, answering what it held of.
async fn poll_until<T, F, Fut>(case: &str, what: &str, mut look: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(settled) = look().await {
            return settled;
        }
        assert!(Instant::now() < deadline, "case {case:?}: expected {what}");
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_predefined_ecs_canary_leaves_its_first_increments_share_on_the_replacement() {
    stage_the_production_route(&ROWS[0]).await;
}

#[tokio::test]
async fn an_own_ecs_canary_leaves_its_canary_percentage_on_the_replacement() {
    stage_the_production_route(&ROWS[1]).await;
}

#[tokio::test]
async fn an_own_ecs_linear_shift_leaves_its_linear_percentage_on_the_replacement() {
    stage_the_production_route(&ROWS[2]).await;
}

/// Run ONE row end to end, on an emulator of its own: each row stands up its own network,
/// cluster, revisions, target groups, listener and service, so the three tests above share
/// nothing and run beside each other.
async fn stage_the_production_route(row: &Row) {
    let case = row.case;
    let router = emulator();
    let network = seed_network(&router).await;
    let (original_revision, replacement_revision) = seed_ecs(&router).await;
    let blue = target_group(&router, &network, BLUE_GROUP).await;
    let green = target_group(&router, &network, GREEN_GROUP).await;
    let listener = production_listener(&router, &network, &blue).await;
    let whole_route_on_blue: BTreeMap<String, i64> = BTreeMap::from([(blue.clone(), WHOLE_ROUTE)]);
    assert_eq!(
        production_shares(&router, &listener).await,
        whole_route_on_blue,
        "case {case:?}: the production route starts on the original target group alone"
    );

    // The blue side: its tasks are Amazon ECS's to run, and its deployments CodeDeploy's to
    // drive.
    let created = ecs_ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": original_revision,
            "desiredCount": row.desired,
            "launchType": "FARGATE",
            "deploymentController": { "type": "CODE_DEPLOY" },
            "loadBalancers": [{
                "targetGroupArn": blue,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": network.subnets,
                    "securityGroups": [network.group],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    let original_task_set = created["service"]["taskSets"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the original task set, got {created}"))
        .to_string();

    // Before any stage can start, the blue side has to RUN: the service converges on the count
    // it was created with, out of the task set Amazon ECS created for it, and every task of it
    // is a target of the group the service's binding named.
    let converged = poll_until(case, "runningCount to reach the desired count", || async {
        let service = described_service(&router).await;
        (service["runningCount"].as_i64() == Some(row.desired)
            && service["pendingCount"].as_i64() == Some(0))
        .then_some(service)
    })
    .await;
    assert_eq!(
        converged["taskSets"][0]["status"].as_str(),
        Some(PRIMARY),
        "case {case:?}: the original task set serves production traffic, got {converged}"
    );
    let blue_targets = registered(&router, &blue).await;
    assert_eq!(
        blue_targets.len(),
        usize::try_from(row.desired).expect("the count fits"),
        "case {case:?}: expected {} target(s) of the original group, got {blue_targets:?}",
        row.desired
    );

    // The application, the configuration the row stages under where that is one of this
    // account's own, and the deployment group naming the service, the pair and the listener.
    codedeploy_ok(
        &router,
        "CreateApplication",
        json!({ "applicationName": SERVICE, "computePlatform": "ECS" }),
    )
    .await;
    if let Some((routing, percentage, interval)) = row.own_config {
        let traffic_routing = if routing == "TimeBasedCanary" {
            json!({
                "type": routing,
                "timeBasedCanary": {
                    "canaryPercentage": percentage,
                    "canaryInterval": interval,
                },
            })
        } else {
            json!({
                "type": routing,
                "timeBasedLinear": {
                    "linearPercentage": percentage,
                    "linearInterval": interval,
                },
            })
        };
        codedeploy_ok(
            &router,
            "CreateDeploymentConfig",
            json!({
                "deploymentConfigName": row.config,
                "computePlatform": "ECS",
                "trafficRoutingConfig": traffic_routing,
            }),
        )
        .await;
    }
    codedeploy_ok(
        &router,
        "CreateDeploymentGroup",
        json!({
            "applicationName": SERVICE,
            "deploymentGroupName": SERVICE,
            "serviceRoleArn": format!("arn:aws:iam::123456789012:role/{SERVICE}"),
            "deploymentStyle": {
                "deploymentType": "BLUE_GREEN",
                "deploymentOption": "WITH_TRAFFIC_CONTROL",
            },
            "blueGreenDeploymentConfiguration": {
                "terminateBlueInstancesOnDeploymentSuccess": {
                    "action": "TERMINATE",
                    "terminationWaitTimeInMinutes": 0,
                },
            },
            "ecsServices": [{ "clusterName": CLUSTER, "serviceName": SERVICE }],
            "loadBalancerInfo": {
                "targetGroupPairInfoList": [{
                    "targetGroups": [{ "name": BLUE_GROUP }, { "name": GREEN_GROUP }],
                    "prodTrafficRoute": { "listenerArns": [listener] },
                }],
            },
        }),
    )
    .await;

    // The deployment itself: its revision IS the AppSpec, and what that declares is the
    // replacement task set.
    let app_spec = format!(
        "version: 0.0\n\
         Resources:\n\
         \x20 - TargetService:\n\
         \x20     Type: AWS::ECS::Service\n\
         \x20     Properties:\n\
         \x20       TaskDefinition: {replacement_revision}\n\
         \x20       LoadBalancerInfo:\n\
         \x20         ContainerName: {CONTAINER}\n\
         \x20         ContainerPort: {CONTAINER_PORT}\n"
    );
    let deployment = codedeploy_ok(
        &router,
        "CreateDeployment",
        json!({
            "applicationName": SERVICE,
            "deploymentGroupName": SERVICE,
            "deploymentConfigName": row.config,
            "revision": {
                "revisionType": "AppSpecContent",
                "appSpecContent": { "content": app_spec },
            },
        }),
    )
    .await;
    let deployment_id = deployment["deploymentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no deploymentId in {deployment}"))
        .to_string();

    // The first increment of the schedule: the route leaves the original group alone for the
    // first time, which is the stage this suite reads.
    let staged = poll_until(case, "the first increment to move its share of the route", || async {
        let route = production_shares(&router, &listener).await;
        (route != whole_route_on_blue).then_some(route)
    })
    .await;
    let expected: BTreeMap<String, i64> = BTreeMap::from([
        (blue.clone(), WHOLE_ROUTE - row.first_stage),
        (green.clone(), row.first_stage),
    ]);
    assert_eq!(
        staged, expected,
        "case {case:?}: expected the route split {expected:?} by the first increment, got \
         {staged:?}"
    );

    // Inside that window the replacement task set runs the service's tasks and holds its own
    // targets, while the task set serving production traffic is still the ORIGINAL one: a stage
    // splits the route rather than cutting over.
    let held = task_sets(&router).await;
    let original = held
        .get(&original_task_set)
        .unwrap_or_else(|| panic!("case {case:?}: the original task set is gone: {held:?}"));
    assert_eq!(
        (original.status.as_str(), original.revision.as_str()),
        (PRIMARY, original_revision.as_str()),
        "case {case:?}: the original task set still serves production traffic, got {held:?}"
    );
    let replacement = held
        .iter()
        .find(|(id, view)| *id != &original_task_set && view.revision == replacement_revision)
        .map(|(_, view)| view)
        .unwrap_or_else(|| panic!("case {case:?}: no replacement task set in {held:?}"));
    assert_eq!(
        (replacement.status.as_str(), replacement.running),
        (ACTIVE, row.desired),
        "case {case:?}: the replacement task set runs the service's count without serving \
         production traffic, got {replacement:?}"
    );
    let green_targets = registered(&router, &green).await;
    assert_eq!(
        green_targets.len(),
        usize::try_from(row.desired).expect("the count fits"),
        "case {case:?}: expected {} target(s) of the replacement group, got {green_targets:?}",
        row.desired
    );
    assert!(
        green_targets.is_disjoint(&blue_targets),
        "case {case:?}: the replacement task set runs its own tasks, got {green_targets:?} \
         beside {blue_targets:?}"
    );
    assert_eq!(
        registered(&router, &blue).await,
        blue_targets,
        "case {case:?}: the original task set keeps serving its share of the route, so its \
         targets stay registered"
    );

    // The deployment is NOT done: the remainder of the route moves only once the interval its
    // configuration states has elapsed, which no part of this run waits for.
    let status = deployment_status(&router, &deployment_id).await;
    assert_eq!(
        status, IN_PROGRESS,
        "case {case:?}: a deployment with an increment still to come is in progress, got \
         {status:?}"
    );
    assert_eq!(
        production_shares(&router, &listener).await,
        staged,
        "case {case:?}: reading the route again inside the window leaves it where the first \
         increment put it"
    );
}
