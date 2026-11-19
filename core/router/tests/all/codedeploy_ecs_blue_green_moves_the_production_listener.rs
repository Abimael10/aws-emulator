// SPDX-License-Identifier: CC0-1.0

//! A CodeDeploy BLUE/GREEN deployment of an Amazon ECS service installs a replacement task
//! set and MOVES the production listener onto its target group.
//!
//! RULE: a service whose `deploymentController.type` is `CODE_DEPLOY` is one Amazon ECS runs
//! the original application task set of itself — "CodeDeploy installs an updated version of
//! the application as a new replacement task set and reroutes production traffic from the
//! original application task set to the replacement task set. The original task set is
//! terminated after a successful deployment" (`CreateService`) — so such a service's
//! `runningCount` reaches its `desiredCount` with no `CreateTaskSet` call, and a deployment
//! of it then creates the replacement task set behind the OTHER target group of its
//! deployment group's pair, waits for that task set to reach a steady state, and rewrites
//! the `prodTrafficRoute` listeners to forward there — in the increments its deployment
//! configuration describes, with the replacement promoted to `PRIMARY` by the increment that
//! completes the move and the original task set deleted afterwards.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here: Amazon
//! EC2 owns the network the tasks are placed in, Amazon ECS owns the service, its task sets
//! and their tasks, Elastic Load Balancing v2 holds the listener the traffic route IS, and
//! CodeDeploy drives the deployment. Everything is driven through the real operations over
//! ONE shared engine, so what is asserted is what each service persisted.
//!
//! The two tests differ along what the rule generalises over: the deployment configuration's
//! own traffic routing — one that moves the whole route at once, and a canary that moves it in
//! two increments and so completes only on the second — how many tasks the service runs, and
//! the ORDER the deployment group lists its pair of target groups in, which must not decide
//! which way the traffic moves. What ONE increment leaves on each group is asserted on the
//! write itself in the `state::ecs_blue_green` unit tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and keyed-list pairing this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{block_of, element, elements, form_encode, members_of, opt_element, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The ECS cluster, task-definition family and container every row runs.
const CLUSTER: &str = "bluegreen";
const FAMILY: &str = "bluegreenfamily";
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// The network every row runs in.
const VPC_CIDR: &str = "10.80.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.80.1.0/24", "us-east-1a"),
    ("10.80.2.0/24", "us-east-1b"),
];

/// How long the clock-owed moves below are waited for, and how often they are looked at.
/// Generous on purpose: the deadline is only here so a failure is a failure rather than a
/// hang, and every wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// `TaskSet$status`: "PRIMARY: The task set is serving production traffic", and "ACTIVE: The
/// task set isn't serving production traffic".
const PRIMARY: &str = "PRIMARY";

/// The terminal `DeploymentStatus` a deployment that did what it described reaches.
const SUCCEEDED: &str = "Succeeded";

/// The canary configuration the incremental test deploys under: the Amazon ECS built-in that
/// shifts 10% of the route and then the rest, over the shorter of the two published intervals
/// (`CodeDeployDefault.ECSCanary10Percent5Minutes`), so the deployment really walks two
/// increments and completes only on the second.
const CANARY_CONFIG: &str = "CodeDeployDefault.ECSCanary10Percent5Minutes";

/// One row: the deployment configuration its deployment runs under, how many tasks its
/// service runs, and the share of the traffic the first increment of that configuration
/// moves — `None` for a configuration that moves the whole route at once.
struct Row {
    case: &'static str,
    /// The names this row's own resources take, so two rows never collide.
    service_name: &'static str,
    application: &'static str,
    group: &'static str,
    blue_group: &'static str,
    green_group: &'static str,
    load_balancer: &'static str,
    listener_port: &'static str,
    /// The `deploymentConfigName` the deployment names: a predefined Amazon ECS built-in.
    deployment_config: &'static str,
    desired: i64,
    /// Whether the deployment group lists the pair's REPLACEMENT group first.
    ///
    /// Which group each task set serves through is decided by what the production listeners
    /// forward to, never by the order the group happens to list the pair in, so a row that
    /// lists it the other way round must move the traffic the same way.
    replacement_listed_first: bool,
}

const ROWS: [Row; 2] = [
    Row {
        case: "the recorded shape: one task, the whole route moved at once",
        service_name: "atonce",
        application: "bluegreen-atonce-app",
        group: "bluegreen-atonce-dg",
        blue_group: "bg-atonce-blue",
        green_group: "bg-atonce-green",
        load_balancer: "bg-atonce-alb",
        listener_port: "80",
        deployment_config: "CodeDeployDefault.ECSAllAtOnce",
        desired: 1,
        replacement_listed_first: false,
    },
    Row {
        case: "two tasks, a canary's two increments, and the pair listed replacement-first",
        service_name: "canary",
        application: "bluegreen-canary-app",
        group: "bluegreen-canary-dg",
        blue_group: "bg-canary-blue",
        green_group: "bg-canary-green",
        load_balancer: "bg-canary-alb",
        listener_port: "8080",
        deployment_config: CANARY_CONFIG,
        desired: 2,
        replacement_listed_first: true,
    },
];

/// One emulator carrying Amazon EC2, Elastic Load Balancing v2, Amazon ECS and CodeDeploy
/// over ONE shared engine, exactly as the production router builds them.
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
                "AWS4-HMAC-SHA256 Credential=AKIDBLUEGREEN/20260101/{REGION}\
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
    assert_eq!(
        status, 200,
        "ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// One CodeDeploy call that must succeed.
async fn codedeploy_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = json_call(
        router,
        "codedeploy",
        "CodeDeploy_20141006",
        operation,
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "CodeDeploy {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every row runs in: a VPC with an internet gateway and a route table that
/// sends `0.0.0.0/0` to it associated with both of its public subnets, and one
/// self-referencing security group the tasks and the load balancers all sit in.
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
                ("GroupName", "bluegreen-group"),
                ("GroupDescription", "the tasks and the load balancers"),
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
    row: &Row,
    blue: &str,
) -> String {
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", row.load_balancer),
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
            ("Port", row.listener_port),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", blue),
        ],
    )
    .await;
    element(&listener, "ListenerArn")
}

/// The target groups the listener `arn` forwards its production traffic to now, each with the
/// weight the route gives it — the empty map for a listener this store holds none of.
///
/// An action that names ONE target group forwards the whole route to it, which is the weight
/// the whole route carries; an action that weights several carries each one's own.
async fn production_route(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let described = elbv2(router, "DescribeListeners", &[("ListenerArns.member.1", arn)]).await;
    let Some(actions) = block_of(&described, "DefaultActions") else {
        return BTreeMap::new();
    };
    let weighted = pairs_of(actions, "TargetGroupArn", "Weight");
    if !weighted.is_empty() {
        return weighted;
    }
    elements(actions, "TargetGroupArn")
        .into_iter()
        .map(|arn| (arn, "100".to_string()))
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

/// The one service of this row, as `DescribeServices` reports it.
async fn described_service(router: &MultiServiceRouter, row: &Row) -> Value {
    let described = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name] }),
    )
    .await;
    described["services"][0].clone()
}

/// What one task set of the service reports: the three members the move is read through.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSetView {
    status: String,
    revision: String,
    running: i64,
}

/// The task sets the service holds, keyed by id.
async fn task_sets(router: &MultiServiceRouter, row: &Row) -> BTreeMap<String, TaskSetView> {
    described_service(router, row).await["taskSets"]
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

/// Look again until `settled` holds, answering what it held of.
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
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}"
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_code_deploy_ecs_deployment_moves_the_whole_production_route_at_once() {
    deploy_and_move_the_route(&ROWS[0]).await;
}

#[tokio::test]
async fn a_code_deploy_ecs_deployment_moves_the_production_route_in_its_canarys_increments() {
    deploy_and_move_the_route(&ROWS[1]).await;
}

/// Run ONE row end to end, on an emulator of its own: each row stands up its own network,
/// cluster, revisions, target groups and listener, so the two tests above share nothing and
/// run beside each other.
async fn deploy_and_move_the_route(row: &Row) {
    let router = emulator();
    let network = seed_network(&router).await;
    let (original_revision, replacement_revision) = seed_ecs(&router).await;

    {
        let case = row.case;
        let blue = target_group(&router, &network, row.blue_group).await;
        let green = target_group(&router, &network, row.green_group).await;
        let listener = production_listener(&router, &network, row, &blue).await;
        assert_eq!(
            production_route(&router, &listener).await.into_keys().collect::<Vec<_>>(),
            vec![blue.clone()],
            "case {case:?}: the production route starts on the original target group"
        );

        // The service: its tasks are Amazon ECS's to run, and its deployments CodeDeploy's
        // to drive.
        let created = ecs_ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name,
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
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the original task set, got {created}")
            })
            .to_string();

        // The cause this suite is here for: the service converges on the count it was
        // created with, out of the task set Amazon ECS created for it.
        let converged = poll_until(case, "runningCount to reach the desired count", || async {
            let service = described_service(&router, row).await;
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
        let original_targets = registered(&router, &blue).await;
        assert_eq!(
            original_targets.len(),
            usize::try_from(row.desired).expect("the count fits"),
            "case {case:?}: every running task of the original task set is a target of its \
             group, got {original_targets:?}"
        );

        // The deployment group: the service it targets, and the pair of target groups and the
        // listener its traffic moves between.
        codedeploy_ok(
            &router,
            "CreateApplication",
            json!({ "applicationName": row.application, "computePlatform": "ECS" }),
        )
        .await;
        // The pair the group names, in the order this row lists it: the group each task set
        // serves through follows the production route, not this order.
        let pair = if row.replacement_listed_first {
            json!([{ "name": row.green_group }, { "name": row.blue_group }])
        } else {
            json!([{ "name": row.blue_group }, { "name": row.green_group }])
        };
        codedeploy_ok(
            &router,
            "CreateDeploymentGroup",
            json!({
                "applicationName": row.application,
                "deploymentGroupName": row.group,
                "serviceRoleArn": format!("arn:aws:iam::123456789012:role/{}", row.group),
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
                "ecsServices": [{ "clusterName": CLUSTER, "serviceName": row.service_name }],
                "loadBalancerInfo": {
                    "targetGroupPairInfoList": [{
                        "targetGroups": pair,
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
                "applicationName": row.application,
                "deploymentGroupName": row.group,
                "deploymentConfigName": row.deployment_config,
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

        // The replacement task set is installed in the service, running the revision the
        // AppSpec named and not yet serving production traffic.
        let replacement = poll_until(case, "the replacement task set to be installed", || async {
            task_sets(&router, row)
                .await
                .into_iter()
                .find(|(id, held)| {
                    id != &original_task_set && held.revision == replacement_revision
                })
                .map(|(id, _)| id)
        })
        .await;

        // The move completes once every increment the configuration schedules has been
        // carried out: the listener then forwards to the replacement group alone. WHAT each
        // increment before that leaves on each group is asserted on the write itself, in
        // the `state::ecs_blue_green` unit tests, rather than by catching a listener inside
        // one increment's own window.
        let moved = poll_until(case, "the production route to move to the replacement", || async {
            let route = production_route(&router, &listener).await;
            (route.len() == 1 && route.contains_key(&green)).then_some(route)
        })
        .await;
        assert_eq!(
            moved.into_keys().collect::<Vec<_>>(),
            vec![green.clone()],
            "case {case:?}: the production route ends on the replacement group alone"
        );
        let replacement_targets = registered(&router, &green).await;
        assert_eq!(
            replacement_targets.len(),
            usize::try_from(row.desired).expect("the count fits"),
            "case {case:?}: every running task of the replacement task set is a target of \
             its group, got {replacement_targets:?}"
        );
        assert!(
            replacement_targets.is_disjoint(&original_targets),
            "case {case:?}: the replacement task set runs its own tasks, got \
             {replacement_targets:?} beside {original_targets:?}"
        );

        // The replacement task set is the one serving production traffic, the original one is
        // taken away, and the service is back to the count it wants.
        let settled = poll_until(case, "the original task set to be terminated", || async {
            let held = task_sets(&router, row).await;
            (held.len() == 1 && held.contains_key(&replacement)).then_some(held)
        })
        .await;
        let held = settled
            .get(&replacement)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the replacement task set"));
        assert_eq!(
            (held.status.as_str(), held.revision.as_str()),
            (PRIMARY, replacement_revision.as_str()),
            "case {case:?}: the replacement task set serves production traffic, running the \
             revision the AppSpec named"
        );
        assert_eq!(
            held.running, row.desired,
            "case {case:?}: the replacement task set holds the service's desired count"
        );
        let service = poll_until(case, "the service to hold only the replacement's tasks", || async {
            let service = described_service(&router, row).await;
            (service["runningCount"].as_i64() == Some(row.desired)).then_some(service)
        })
        .await;
        assert_eq!(
            service["taskDefinition"].as_str(),
            Some(replacement_revision.as_str()),
            "case {case:?}: the service reports the revision its primary task set runs, got \
             {service}"
        );

        // And the deployment that did all of it reports the outcome of what it did.
        let finished = poll_until(case, "the deployment to reach a terminal status", || async {
            let read = codedeploy_ok(
                &router,
                "GetDeployment",
                json!({ "deploymentId": deployment_id }),
            )
            .await;
            read["deploymentInfo"]["completeTime"]
                .is_null()
                .then_some(())
                .map_or(Some(read), |()| None)
        })
        .await;
        assert_eq!(
            finished["deploymentInfo"]["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected a deployment that succeeded, got {finished}"
        );
    }
}
