// SPDX-License-Identifier: CC0-1.0

//! The stop a task whose LAUNCH Amazon ECS could not make earns outlives every stop its
//! service's scheduler asks for afterwards: a task that never ran is reported as one that
//! failed to start, not as one the scheduler stopped.
//!
//! RULE: `Task$stopCode` is "the stop code indicating why a task was stopped", and
//! `TaskFailedToStart` is its member for a task that failed before its containers started —
//! which is what a task whose image could not be fetched from the placement it was made into
//! is, with `Task$stoppedReason` ("the reason that the task was stopped") carrying the
//! container runtime's `CannotPullContainerError` for the reference it could not resolve.
//! `ServiceSchedulerInitiated` is the member for a task the service scheduler took out of a
//! task set it was holding up — a scaling activity, a deployment that replaces it, a failed
//! health check — so a task whose launch had ALREADY failed when the scheduler asked it to
//! stop reports that failure: nothing the scheduler does can launch it, and it never ran.
//!
//! The rows differ along what the rule generalises over: WHICH stop the scheduler makes while
//! the failed launch is still coming up (none, a scale-in to zero, a new deployment rolled
//! over it, the deployment circuit breaker's rollback), how many tasks the scale-up asked for,
//! and whether the service declares a monitor on its rollout at all. The recorded
//! configuration — one task scaled up into subnets with no route off the VPC — is a single
//! row.
//!
//! The control at the end is the other half of the same rule: a service in a subnet that DOES
//! reach the registry runs its task, and the scale-in that stops that task IS the scheduler's
//! own stop, so `ServiceSchedulerInitiated` keeps the meaning a client branches on.
//!
//! The suite lives beside the router because three services have to agree: Amazon EC2 owns the
//! subnets and route tables the image fetch is decided against, Amazon ECS owns the service,
//! its scale-ups and the tasks they place, and Elastic Load Balancing v2 owns the target group
//! the service binds and the registrations it must not hold for a task that never ran.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The ECS cluster and the one container of every revision below.
const CLUSTER: &str = "isostop";
const CONTAINER: &str = "app";

/// The port the container declares a `portMapping` for, and the port every binding names.
const CONTAINER_PORT: i64 = 80;

/// The network every row runs in: one VPC, two subnets for the internal load balancer in two
/// Availability Zones, and one /24 per row.
const VPC_CIDR: &str = "10.93.0.0/16";
const LOAD_BALANCER_SUBNETS: [(&str, &str); 2] = [
    ("10.93.1.0/24", "us-east-1a"),
    ("10.93.2.0/24", "us-east-1b"),
];

/// The Availability Zone every row's own subnet is created in.
const ZONE: &str = "us-east-1a";

/// The destination of the route that carries traffic OFF a VPC: the default route (Amazon VPC
/// User Guide, "Route tables").
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The image every row's revision runs: a reference into a PUBLIC container registry, whose
/// registry is no AWS service and so is reachable only over the internet.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const REGISTRY: &str = "public.ecr.aws";

/// The image the CONTROL runs: a reference that names no registry at all, which a container
/// runtime resolves against Docker Hub — an image this emulator's gallery serves, so the
/// control's task can actually be launched.
const CONTROL_IMAGE: &str = "nginx:1.27";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The `TaskStopCode` members this suite tells apart
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated): the stop of a task that failed before its containers
/// started, and the stop of a task the service scheduler stopped.
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";

/// The error class a container runtime words a fetch it could not make in.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// `AwsVpcConfiguration$assignPublicIp` as each placement names it.
const ASSIGN_PUBLIC_IP_ENABLED: &str = "ENABLED";
const ASSIGN_PUBLIC_IP_DISABLED: &str = "DISABLED";

/// How long a row waits for the clock-owed moves it depends on — the placement its scale-up
/// asks for, the start that placement owes and the stop a failed fetch earns — and how often
/// it looks. Generous on purpose, by well over an order of magnitude over the spans
/// themselves: the deadline is only here so a failure is a failure rather than a hang, and a
/// run sharing a loaded machine with every other suite of the workspace must not be read as
/// one.
const POLL_BUDGET: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// What the row does to the service while the launch its scale-up scheduled is still coming
/// up — the stop the scheduler is asked for in that window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scaling {
    /// Nothing: the scale-up is the only call, and the failed launch stops on its own.
    None,
    /// Back to zero and up again, so the scheduler asks the launch that already failed to
    /// stop as a scaling activity.
    DownAndUp,
    /// A new deployment, so the scheduler asks the launch that already failed to stop as a
    /// task the rollout replaces.
    Redeploy,
}

/// One row: the subnet its service places tasks in, the monitor its rollout carries, how many
/// tasks its scale-up asks for and what the scheduler is asked to do in the meantime.
struct Row {
    case: &'static str,
    /// The name every resource of the row is built from.
    name: &'static str,
    /// The row's own /24 inside [`VPC_CIDR`], with no route off the VPC.
    cidr: &'static str,
    /// The listener port that makes the row's target group one the load balancer forwards to.
    listener_port: &'static str,
    /// `DeploymentCircuitBreaker` the service declares: `None` for a service that declares
    /// none, else whether that breaker asked for a rollback.
    breaker: Option<bool>,
    /// How many tasks the scale-up asks for.
    desired: i64,
    scaling: Scaling,
}

const ROWS: [Row; 6] = [
    Row {
        case: "the recorded configuration: one task, nothing else asked of the scheduler",
        name: "plain",
        cidr: "10.93.11.0/24",
        listener_port: "8091",
        breaker: None,
        desired: 1,
        scaling: Scaling::None,
    },
    Row {
        case: "scaled back to zero and up again while the launch that failed was coming up",
        name: "downandup",
        cidr: "10.93.12.0/24",
        listener_port: "8092",
        breaker: None,
        desired: 1,
        scaling: Scaling::DownAndUp,
    },
    Row {
        case: "a new deployment rolled over the launch that failed",
        name: "redeploy",
        cidr: "10.93.13.0/24",
        listener_port: "8093",
        breaker: None,
        desired: 1,
        scaling: Scaling::Redeploy,
    },
    Row {
        case: "three tasks, scaled back to zero and up again",
        name: "three",
        cidr: "10.93.14.0/24",
        listener_port: "8094",
        breaker: None,
        desired: 3,
        scaling: Scaling::DownAndUp,
    },
    Row {
        case: "the circuit breaker watching the rollout, rolling back on failure",
        name: "rollback",
        cidr: "10.93.15.0/24",
        listener_port: "8095",
        breaker: Some(true),
        desired: 1,
        scaling: Scaling::DownAndUp,
    },
    Row {
        case: "the circuit breaker watching the rollout with no rollback",
        name: "norollback",
        cidr: "10.93.16.0/24",
        listener_port: "8096",
        breaker: Some(false),
        desired: 2,
        scaling: Scaling::None,
    },
];

impl Row {
    /// The task-definition family the row's revision is registered under.
    fn family(&self) -> String {
        format!("isostop-{}", self.name)
    }

    /// The target group the row's service binds.
    fn group_name(&self) -> String {
        format!("isostop-{}", self.name)
    }

    /// The service the row scales up.
    fn service_name(&self) -> String {
        format!("isostop-{}", self.name)
    }
}

/// The WHOLE `stoppedReason` every task of every row must carry: the container runtime's
/// error class, the reference the revision named and the registry that reference resolves to,
/// in the condition a placement with no path to it produces. Spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make this test pass.
fn expected_reason() -> String {
    format!(
        "{CANNOT_PULL}: failed to resolve ref {IMAGE}: failed to do request: \
         Get \"https://{REGISTRY}/v2/\": dial tcp {REGISTRY}:443: i/o timeout"
    )
}

/// The control's own names: a service in a subnet that DOES reach the registry.
const CONTROL_NAME: &str = "control";
const CONTROL_CIDR: &str = "10.93.21.0/24";
const CONTROL_LISTENER_PORT: &str = "8101";

/// One emulator carrying Amazon EC2, Elastic Load Balancing v2 and Amazon ECS over ONE shared
/// engine, exactly as the production router builds them.
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
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
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
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson1_1 call to Amazon ECS that must succeed, answered with its parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
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
    assert_eq!(status, 200, "ECS {operation} answered {status} with {parsed}");
    parsed
}

/// The network every row's resources are created in, every id of it minted by Amazon EC2.
struct Network {
    vpc: String,
    internet_gateway: String,
    load_balancer_subnets: Vec<String>,
    security_group: String,
}

/// Create one subnet of this suite's VPC through Amazon EC2 itself.
async fn subnet(router: &MultiServiceRouter, vpc: &str, cidr: &str, zone: &str) -> String {
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Create a route table of this suite's VPC, associate it with `subnet_id` and send
/// [`DEFAULT_ROUTE`] to the VPC's internet gateway when `routed` — or nowhere, which is a
/// subnet with no route off the VPC at all. Answers the table's id.
async fn route_table(
    router: &MultiServiceRouter,
    network: &Network,
    subnet_id: &str,
    routed: bool,
) -> String {
    let table = element(
        &ec2(
            router,
            "CreateRouteTable",
            &[("VpcId", network.vpc.as_str())],
        )
        .await,
        "routeTableId",
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[("RouteTableId", table.as_str()), ("SubnetId", subnet_id)],
    )
    .await;
    if routed {
        ec2(
            router,
            "CreateRoute",
            &[
                ("RouteTableId", table.as_str()),
                ("DestinationCidrBlock", DEFAULT_ROUTE),
                ("GatewayId", network.internet_gateway.as_str()),
            ],
        )
        .await;
    }
    table
}

/// Create the network every row runs in: the VPC, an attached internet gateway the control's
/// subnet routes to, the internal load balancer's two subnets and the security group every
/// placement names.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let internet_gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", internet_gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let mut load_balancer_subnets = Vec::new();
    for (cidr, zone) in LOAD_BALANCER_SUBNETS {
        load_balancer_subnets.push(subnet(router, &vpc, cidr, zone).await);
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "isostop-group"),
                ("GroupDescription", "the group this suites resources are in"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        vpc,
        internet_gateway,
        load_balancer_subnets,
        security_group,
    }
}

/// Register one `awsvpc`, Fargate-compatible revision whose one container runs `image`.
async fn register_revision(router: &MultiServiceRouter, family: &str, image: &str) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": image,
                "essential": true,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
            }],
        }),
    )
    .await;
}

/// Create one `ip` target group — the target type a service whose tasks use the `awsvpc`
/// network mode must bind — behind a listener of `load_balancer`, and answer its ARN.
async fn bound_target_group(
    router: &MultiServiceRouter,
    network: &Network,
    load_balancer: &str,
    name: &str,
    listener_port: &str,
) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
        ],
    )
    .await;
    let group = opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("{name}: no TargetGroupArn in {created}"));
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer),
            ("Protocol", "HTTP"),
            ("Port", listener_port),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
        ],
    )
    .await;
    group
}

/// The `awsvpcConfiguration` a placement in `subnet_id` runs under.
fn network_configuration(network: &Network, subnet_id: &str, assign_public_ip: &str) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": [subnet_id],
            "securityGroups": [network.security_group],
            "assignPublicIp": assign_public_ip,
        }
    })
}

/// The addresses `DescribeTargetHealth` reports as registered in `arn`.
async fn registered_targets(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body)
        .into_iter()
        .flat_map(|description| elements(description, "Id"))
        .collect()
}

/// Scale one service to `count` through `UpdateService`.
async fn scale(router: &MultiServiceRouter, service: &str, count: i64) {
    let updated = ecs(
        router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": service,
            "desiredCount": count,
        }),
    )
    .await;
    assert_eq!(
        updated["service"]["desiredCount"].as_i64(),
        Some(count),
        "{service}: the scaled service must want {count} task(s), got {updated}"
    );
}

/// The task ARNs of one service with the `desiredStatus` `desired`.
async fn task_arns(router: &MultiServiceRouter, service: &str, desired: &str) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": service,
            "desiredStatus": desired,
        }),
    )
    .await;
    listed["taskArns"]
        .as_array()
        .map(|arns| {
            arns.iter()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One task as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, arn: &str) -> Value {
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
    )
    .await;
    described["tasks"][0].clone()
}

/// Every task of one service, oldest first — the ARNs end in a monotonically minted id, so
/// the first of them is the task a client reading "the service's first task" gets.
async fn tasks_of(router: &MultiServiceRouter, service: &str) -> Vec<Value> {
    let mut arns = task_arns(router, service, RUNNING).await;
    arns.extend(task_arns(router, service, STOPPED).await);
    arns.sort();
    arns.dedup();
    let mut tasks = Vec::new();
    for arn in &arns {
        tasks.push(describe_task(router, arn).await);
    }
    tasks
}

/// What a client watching one service sees of its tasks, as a line per task. Read for a
/// failure message, so an assertion names the tasks that broke it.
fn summary(tasks: &[Value]) -> String {
    tasks
        .iter()
        .map(|task| {
            format!(
                "{{lastStatus={} desiredStatus={} stopCode={} stoppedReason={}}}",
                task["lastStatus"], task["desiredStatus"], task["stopCode"], task["stoppedReason"],
            )
        })
        .collect::<Vec<String>>()
        .join(", ")
}

/// `(desiredCount, runningCount)` as `DescribeServices` reports them.
async fn service_counts(router: &MultiServiceRouter, service: &str) -> (i64, i64) {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [service] }),
    )
    .await;
    let reported = described["services"][0].clone();
    (
        reported["desiredCount"].as_i64().unwrap_or_default(),
        reported["runningCount"].as_i64().unwrap_or_default(),
    )
}

/// Poll `probe` until it answers `Some`, or the budget runs out. The wait is for the
/// CONDITION: every move below is one the clock owes, not one a call makes.
async fn poll_until<T, F, Fut>(probe: F, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        if let Some(answer) = probe().await {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{description}: not observed within {POLL_BUDGET:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_failed_launch_keeps_its_own_stop_through_every_scheduler_stop() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs(
        &router,
        "CreateCluster",
        json!({
            "clusterName": CLUSTER,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
        }),
    )
    .await;
    // An internal load balancer: the rows' own subnets have no route off the VPC, which is
    // the whole point of the placement under test.
    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "isostop-alb"),
                ("Scheme", "internal"),
                ("Subnets.member.1", network.load_balancer_subnets[0].as_str()),
                ("Subnets.member.2", network.load_balancer_subnets[1].as_str()),
                ("SecurityGroups.member.1", network.security_group.as_str()),
            ],
        )
        .await,
        "LoadBalancerArn",
    );

    // Every row's service is created wanting NO tasks, so nothing is placed until the
    // scale-up below asks for it — the shape the recorded scenario starts from.
    let mut subnets: Vec<String> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    for row in &ROWS {
        let subnet_id = subnet(&router, &network.vpc, row.cidr, ZONE).await;
        let table = route_table(&router, &network, &subnet_id, false).await;
        // The precondition every assertion below rests on, read back out of Amazon EC2: the
        // table in force on the placement's subnet carries no default route at all.
        let tables = ec2(
            &router,
            "DescribeRouteTables",
            &[("RouteTableId.1", table.as_str())],
        )
        .await;
        assert!(
            !elements(&tables, "destinationCidrBlock").contains(&DEFAULT_ROUTE.to_string()),
            "case {:?}: expected no default route in the subnet's table, got {tables}",
            row.case
        );
        register_revision(&router, &row.family(), IMAGE).await;
        let group = bound_target_group(
            &router,
            &network,
            &load_balancer,
            &row.group_name(),
            row.listener_port,
        )
        .await;
        let mut request = json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name(),
            "taskDefinition": row.family(),
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": network_configuration(
                &network,
                &subnet_id,
                ASSIGN_PUBLIC_IP_DISABLED,
            ),
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        });
        if let Some(rollback) = row.breaker {
            // "If the service fails to reach a steady state and circuit breaker is turned
            // on, the deployment transitions to a FAILED state", and a breaker that asked for
            // a rollback then takes the service back to its last completed deployment — a new
            // rollout, whose scheduler replaces the tasks the failed one placed.
            request["deploymentConfiguration"] = json!({
                "deploymentCircuitBreaker": { "enable": true, "rollback": rollback },
            });
        }
        ecs(&router, "CreateService", request).await;
        subnets.push(subnet_id);
        groups.push(group);
    }

    // The call the scenario is about, with the stop each row asks the scheduler for in the
    // window the failed launch is still coming up in.
    for (row, subnet_id) in ROWS.iter().zip(&subnets) {
        scale(&router, &row.service_name(), row.desired).await;
        match row.scaling {
            Scaling::None => {}
            Scaling::DownAndUp => {
                scale(&router, &row.service_name(), 0).await;
                scale(&router, &row.service_name(), row.desired).await;
            }
            Scaling::Redeploy => {
                // `networkConfiguration` is one of the members whose own doc says it
                // "triggers a new service deployment", so this rolls one over the launch that
                // is still coming up: its tasks are tasks the new rollout replaces.
                ecs(
                    &router,
                    "UpdateService",
                    json!({
                        "cluster": CLUSTER,
                        "service": row.service_name(),
                        "networkConfiguration": network_configuration(
                            &network,
                            subnet_id,
                            ASSIGN_PUBLIC_IP_DISABLED,
                        ),
                    }),
                )
                .await;
            }
        }
    }

    for (row, group) in ROWS.iter().zip(&groups) {
        let case = row.case;
        let service = row.service_name();
        let wanted = usize::try_from(row.desired).unwrap_or(1);
        // Every launch the scale-up made reaches its verdict: as many of the service's tasks
        // have STOPPED as it asked for, and the group holds nothing at any instant on the way
        // there — the half a client cannot otherwise tell from "still coming up".
        let tasks = poll_until(
            || async {
                let registered = registered_targets(&router, group).await;
                assert!(
                    registered.is_empty(),
                    "case {case:?}: no target may be registered for a task that never ran, got \
                     {registered:?}"
                );
                let tasks = tasks_of(&router, &service).await;
                let stopped = tasks
                    .iter()
                    .filter(|task| task["lastStatus"].as_str() == Some(STOPPED))
                    .count();
                (stopped >= wanted).then_some(tasks)
            },
            &format!("case {case:?}: {wanted} stopped task(s) of {service}"),
        )
        .await;

        // The task a client reading "the service's first stopped task" gets.
        let first = tasks
            .iter()
            .find(|task| task["lastStatus"].as_str() == Some(STOPPED))
            .expect("the poll above waited for one");
        assert_eq!(
            first["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: the first stopped task of the service never ran, got {}",
            summary(&tasks)
        );
        let expected = expected_reason();
        assert_eq!(
            first["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {}",
            summary(&tasks)
        );
        // And so does EVERY task of the service, whichever stop took it away: nothing this
        // service placed could be launched, so no task of it was stopped by the scheduler and
        // none of them ever started.
        for task in &tasks {
            assert_ne!(
                task["stopCode"].as_str(),
                Some(SERVICE_SCHEDULER_INITIATED),
                "case {case:?}: no task of this service ran, so none of them was stopped by \
                 the scheduler, got {}",
                summary(&tasks)
            );
            assert_eq!(
                task["startedAt"].as_f64(),
                None,
                "case {case:?}: a task that never ran has no startedAt, got {}",
                summary(&tasks)
            );
            if task["lastStatus"].as_str() == Some(STOPPED) {
                assert_eq!(
                    task["stopCode"].as_str(),
                    Some(TASK_FAILED_TO_START),
                    "case {case:?}: every stopped task of this service failed to start, got {}",
                    summary(&tasks)
                );
                assert_eq!(
                    task["stoppedReason"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the stopped reason {expected:?}, got {}",
                    summary(&tasks)
                );
            }
        }
        let (desired, running) = service_counts(&router, &service).await;
        assert_eq!(
            (desired, running),
            (row.desired, 0),
            "case {case:?}: expected the service to want {} task(s) and run none, got {}",
            row.desired,
            summary(&tasks)
        );
        let registered = registered_targets(&router, group).await;
        assert!(
            registered.is_empty(),
            "case {case:?}: expected the bound group to hold no registration, got {registered:?}"
        );
    }

    // The CONTROL, which is the other half of the rule: a service whose placement reaches the
    // registry runs its task, and the scale-in that stops THAT task is the scheduler's own
    // stop — so the stop code a client branches on still tells the two apart.
    let case = "the control: a routed subnet and a public address, scaled up and back down";
    let control_subnet = subnet(&router, &network.vpc, CONTROL_CIDR, ZONE).await;
    route_table(&router, &network, &control_subnet, true).await;
    register_revision(&router, CONTROL_NAME, CONTROL_IMAGE).await;
    let control_group = bound_target_group(
        &router,
        &network,
        &load_balancer,
        CONTROL_NAME,
        CONTROL_LISTENER_PORT,
    )
    .await;
    ecs(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": CONTROL_NAME,
            "taskDefinition": CONTROL_NAME,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": network_configuration(
                &network,
                &control_subnet,
                ASSIGN_PUBLIC_IP_ENABLED,
            ),
            "loadBalancers": [{
                "targetGroupArn": control_group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        }),
    )
    .await;
    scale(&router, CONTROL_NAME, 1).await;
    let running = poll_until(
        || async {
            let tasks = tasks_of(&router, CONTROL_NAME).await;
            tasks
                .into_iter()
                .find(|task| task["lastStatus"].as_str() == Some(RUNNING))
        },
        &format!("case {case:?}: the control's task running"),
    )
    .await;
    assert_eq!(
        running["stopCode"].as_str(),
        None,
        "case {case:?}: a task that is running carries no stop code, got {running}"
    );
    assert!(
        running["startedAt"].as_f64().is_some(),
        "case {case:?}: a running task reports when it started, got {running}"
    );
    scale(&router, CONTROL_NAME, 0).await;
    let stopped = poll_until(
        || async {
            let tasks = tasks_of(&router, CONTROL_NAME).await;
            tasks
                .into_iter()
                .find(|task| task["lastStatus"].as_str() == Some(STOPPED))
        },
        &format!("case {case:?}: the control's task stopped"),
    )
    .await;
    assert_eq!(
        stopped["stopCode"].as_str(),
        Some(SERVICE_SCHEDULER_INITIATED),
        "case {case:?}: a task the scheduler scaled in was stopped by the scheduler, got \
         {stopped}"
    );
    assert!(
        stopped["startedAt"].as_f64().is_some(),
        "case {case:?}: the control's task ran before the scheduler stopped it, got {stopped}"
    );
}
