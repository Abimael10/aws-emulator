// SPDX-License-Identifier: CC0-1.0

//! An Amazon ECS rollout and every task it launches reach a TERMINAL state off the clock, so a
//! client polling a Fargate service placed in an isolated subnet always gets a verdict.
//!
//! RULE: `Deployment$rolloutState` is a state machine with ONE entry and two ends — "When a
//! service deployment is started, it begins in an `IN_PROGRESS` state. When the service reaches
//! a steady state, the deployment transitions to a `COMPLETED` state. If the service fails to
//! reach a steady state and circuit breaker is turned on, the deployment transitions to a
//! `FAILED` state" — and it belongs to the DEPLOYMENT a release started (`ListServiceDeployments`:
//! "A service deployment happens when you release a software update for the service"). So a
//! rollout that reaches a steady state, the steady state of a service that wants NO tasks
//! included, keeps `COMPLETED` however much work a later change to the count the service wants
//! gives the scheduler — that change releases nothing and starts no deployment — while a rollout
//! still under way that cannot reach one is taken to `FAILED` with the reason it failed for. The
//! member that moves when a settled service is scaled is the one about convergence,
//! `Deployment$stabilityStatus`. A task the scheduler launches ends the same way:
//! `Task$lastStatus` ("the last known status for the task") reaches `RUNNING` with
//! `Task$startedAt` ("the time when the task transitioned from the `PENDING` state to the
//! `RUNNING` state") when its placement can fetch the images its containers run, and reaches
//! `STOPPED` with the `TaskStopCode` member for a task whose containers were never started, a
//! `Task$stoppedReason` carrying the container runtime's `CannotPullContainerError` for the
//! reference it could not resolve, NO `startedAt` and no `exitCode` on any container when it
//! cannot — and an `awsvpc` task carries exactly one `ElasticNetworkInterface` attachment either
//! way, whose `Attachment$details` name the subnet it was placed in ("for elastic network
//! interfaces, this includes the network interface ID, the MAC address, the subnet ID, and the
//! private IPv4 address"). Whether the placement reaches the registry is Amazon VPC's answer,
//! read off the route table in force on its subnet: a subnet with no route off the VPC reaches
//! neither a public registry nor the Amazon ECR endpoints a VPC without them publishes, whether
//! or not the launch asked for a public address, while a subnet routed to an attached internet
//! gateway carries the traffic of a task that was given one.
//!
//! It lives beside the router because the estate the rule is read over is three services':
//! Amazon EC2 owns the VPC, the subnets, their route tables and the internet gateway the path
//! is read off, Amazon ECR owns the private registry the revision's references address, and
//! Amazon ECS owns the service, the scale-up, the tasks and the deployment. Every fact is
//! created through the owning service's own API, and every value asserted is read back through
//! `DescribeServices`, `ListTasks` and `DescribeTasks` — the three reads a client asking "did
//! my rollout succeed, fail, or is it still coming up?" has.
//!
//! The rows differ along what the rule generalises over: WHICH call asks for the tasks (the
//! create, or an `UpdateService` after the rollout settled), where the placement's default route
//! goes (nowhere, an attached internet gateway), whether the launch asked for a public address,
//! how many containers the revision declares and which registry their references address (two
//! containers — a non-essential FireLens log router beside the essential service container —
//! pulling from a repository of this account's own Amazon ECR registry, and one pulling from a
//! public registry), and how many tasks are asked for. The recorded configuration — one task of
//! a two-container revision in a subnet with no route off the VPC, asked for after the service
//! settled wanting none — is a single row; the last row is the same placement wanting its task
//! from the create on, which is the rollout the circuit breaker takes to `FAILED`. The positive
//! control a client reads beside the rollout — a task run straight into the routed subnet with a
//! public address — is asserted once at the end.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query-protocol suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions the ec2Query and the IAM awsQuery requests carry.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_VERSION: &str = "2010-05-08";

/// The cluster every service and the control task below run in.
const CLUSTER: &str = "isolatedrollout";

/// The two containers of the recorded revision: a FireLens log router, which is NOT essential,
/// and the service container, which is.
const LOG_ROUTER_CONTAINER: &str = "log-router";
const SERVICE_CONTAINER: &str = "service";

/// The log router kind the FireLens container declares
/// (`smithy_query(query="shape",shape="FirelensConfigurationType")` -> fluentbit, fluentd).
const FIRELENS_TYPE: &str = "fluentbit";

/// The network every row runs in: one VPC, one subnet routed to an attached internet gateway,
/// and two subnets sharing a route table that carries no route off the VPC at all.
const VPC_CIDR: &str = "10.96.0.0/16";
const ROUTED_SUBNET: (&str, &str) = ("10.96.0.0/20", "us-east-1a");
const ISOLATED_SUBNETS: [(&str, &str); 2] = [
    ("10.96.16.0/20", "us-east-1a"),
    ("10.96.32.0/20", "us-east-1b"),
];

/// The destination of the route that carries traffic OFF a VPC: "a route table contains a set
/// of rules, called routes" and the one for everything outside the VPC is the default route
/// (Amazon VPC User Guide, "Route tables").
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The repository of this account's own Amazon ECR registry the recorded revision's two
/// references address, and the tags they name. NOTHING is pushed into it: what the rows below
/// turn on is whether the placement can REACH the registry, which is decided before any
/// manifest is asked for.
const REPOSITORY: &str = "isolatedrollout/app";
const LOG_ROUTER_TAG: &str = "log-router";
const SERVICE_TAG: &str = "service";

/// An image in a PUBLIC container registry, whose registry is no AWS service and so is
/// reachable only over the internet.
const PUBLIC_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The execution role Amazon ECS assumes to pull a task's images, trusted by the Amazon ECS
/// tasks principal, and the managed policy AWS publishes for one ("AmazonECSTaskExecutionRole
/// Policy", the task execution role of the Amazon ECS Developer Guide) — attached rather than
/// hand-written, so the launch is decided over the document IAM really holds.
const EXECUTION_ROLE: &str = "isolatedrolloutexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The task size the recorded revision declares, which is one of the Fargate CPU/memory
/// combinations (`TaskDefinition$cpu`: "2048 (2 vCPU) - Available memory values: 4096 (4 GB)
/// and 16384 (16 GB) in 1024 (1 GB) increments"), and the smallest one the single-container
/// revisions declare.
const RECORDED_CPU: &str = "2048";
const RECORDED_MEMORY: &str = "4096";
const SMALL_CPU: &str = "256";
const SMALL_MEMORY: &str = "512";

/// `Deployment$status` of the deployment a client reads the rollout state off: the service's
/// current one.
const DEPLOYMENT_PRIMARY: &str = "PRIMARY";

/// The states `DeploymentRolloutState` holds
/// (`smithy_query(query="shape",shape="DeploymentRolloutState")` -> COMPLETED, FAILED,
/// IN_PROGRESS): the two terminal ones a client polls for, and the one it polls through.
const ROLLOUT_COMPLETED: &str = "COMPLETED";
const ROLLOUT_FAILED: &str = "FAILED";
const ROLLOUT_IN_PROGRESS: &str = "IN_PROGRESS";

/// `Deployment$stabilityStatus`, the member that says whether the service has CONVERGED on the
/// task set it wants — "The task `runningCount` is equal to the `computedDesiredCount`" — as
/// distinct from the state of the rollout that deployed it
/// (`smithy_query(query="shape",shape="StabilityStatus")` -> STEADY_STATE, STABILIZING).
const STEADY_STATE: &str = "STEADY_STATE";
const STABILIZING: &str = "STABILIZING";

/// `Task$lastStatus` of a task that is running, and of one that has stopped — the two the
/// documented PROVISIONING/PENDING/RUNNING and DEACTIVATING/STOPPING/STOPPED lifecycles end at.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Attachment$type` of the elastic network interface an `awsvpc` task is allocated, and the
/// `Attachment$details` entry naming where it was created.
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";
const SUBNET_ID_DETAIL: &str = "subnetId";

/// `AwsVpcConfiguration$assignPublicIp` as each row names it: "whether the task's elastic
/// network interface receives a public IP address".
const ASSIGN_PUBLIC_IP_ENABLED: &str = "ENABLED";
const ASSIGN_PUBLIC_IP_DISABLED: &str = "DISABLED";

/// The registry a reference that names no registry at all is fetched from, which is how a
/// container runtime resolves a bare name — kept here only so the expected `stoppedReason`
/// below can be composed for any reference, including one of that shape.
const DOCKER_HUB_REGISTRY: &str = "registry-1.docker.io";

/// How long a wait below keeps looking, and how often. Generous on purpose, by well over an
/// order of magnitude over the spans the transitions themselves take: the deadline is only here
/// so a failure is a failure rather than a hang, and a run sharing a loaded machine with every
/// other suite of the workspace must not be read as one.
const POLL_BUDGET: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Where one row's placement sends traffic bound for anything outside the VPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// The two subnets whose shared route table carries no default route.
    Isolated,
    /// The subnet routed to the VPC's attached internet gateway.
    Routed,
}

/// What one row's revision declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Revision {
    /// The recorded shape: a non-essential FireLens log router beside the essential service
    /// container, both pulling from this account's own Amazon ECR registry.
    FirelensPair,
    /// One essential container pulling from a public registry.
    SingleService,
}

/// Where one row's rollout and its tasks END.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The images can be fetched from this placement: the tasks run and the rollout completes.
    Runs,
    /// They cannot: every task stops before it ran and the circuit breaker fails the rollout.
    FailsToStart,
}

/// One row: where the service's tasks are placed, what they run, which call asks for how many
/// of them, and where the rollout and the tasks end.
struct Row {
    case: &'static str,
    /// The name the row's family and service are built from.
    name: &'static str,
    placement: Placement,
    assign_public_ip: &'static str,
    revision: Revision,
    /// The `desiredCount` the service is CREATED with.
    created_desired: i64,
    /// The count `UpdateService` raises it to afterwards, and `None` for a row that keeps the
    /// count its create asked for.
    scale_to: Option<i64>,
    /// `Deployment$rolloutState` the row's rollout must END at: a rollout that reached a steady
    /// state keeps `COMPLETED`, whatever work a later scale-up gives the scheduler, and one
    /// that never reached one is taken to `FAILED` by the deployment circuit breaker.
    rollout: &'static str,
    /// What the tasks the service ends up wanting do.
    verdict: Verdict,
}

const ROWS: [Row; 4] = [
    Row {
        case: "the recorded configuration: two isolated subnets, a FireLens pair out of this \
               account's registry, no public address, scaled up after it settled",
        name: "isolated",
        placement: Placement::Isolated,
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        revision: Revision::FirelensPair,
        created_desired: 0,
        scale_to: Some(1),
        rollout: ROLLOUT_COMPLETED,
        verdict: Verdict::FailsToStart,
    },
    Row {
        case: "an isolated subnet where the launch asked for a public address anyway, pulling \
               from a public registry",
        name: "isolated-public-ip",
        placement: Placement::Isolated,
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        revision: Revision::SingleService,
        created_desired: 0,
        scale_to: Some(1),
        rollout: ROLLOUT_COMPLETED,
        verdict: Verdict::FailsToStart,
    },
    Row {
        case: "the routed subnet with a public address, scaled up to two tasks",
        name: "routed",
        placement: Placement::Routed,
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        revision: Revision::SingleService,
        created_desired: 0,
        scale_to: Some(2),
        rollout: ROLLOUT_COMPLETED,
        verdict: Verdict::Runs,
    },
    Row {
        case: "the same isolated placement, but wanting its task from the create on, so the \
               rollout that cannot start it is the one still under way",
        name: "isolated-at-create",
        placement: Placement::Isolated,
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        revision: Revision::FirelensPair,
        created_desired: 1,
        scale_to: None,
        rollout: ROLLOUT_FAILED,
        verdict: Verdict::FailsToStart,
    },
];

impl Row {
    /// The task-definition family the row's revision is registered under.
    fn family(&self) -> String {
        format!("isorollout-{}", self.name)
    }

    /// The service the row scales up.
    fn service_name(&self) -> String {
        format!("isorollout-{}", self.name)
    }

    /// How many tasks the service ends up wanting: the scale-up's count where it makes one, the
    /// create's otherwise.
    fn desired(&self) -> i64 {
        self.scale_to.unwrap_or(self.created_desired)
    }

    /// `Service$runningCount` ("the number of tasks in the cluster that are in the `RUNNING`
    /// state") the row's service must end up reporting.
    fn expected_running_count(&self) -> i64 {
        match self.verdict {
            Verdict::Runs => self.desired(),
            Verdict::FailsToStart => 0,
        }
    }

    /// The container names the row's revision declares, in the order it declares them.
    fn containers(&self) -> Vec<&'static str> {
        match self.revision {
            Revision::FirelensPair => vec![LOG_ROUTER_CONTAINER, SERVICE_CONTAINER],
            Revision::SingleService => vec![SERVICE_CONTAINER],
        }
    }
}

/// The references one revision's containers run, in the order the revision declares them.
fn images_of(revision: Revision, registry: &Registry) -> Vec<String> {
    match revision {
        Revision::FirelensPair => vec![
            registry.reference(LOG_ROUTER_TAG),
            registry.reference(SERVICE_TAG),
        ],
        Revision::SingleService => vec![PUBLIC_IMAGE.to_string()],
    }
}

/// The REGISTRY an image reference is fetched from: the part before the first `/` when that
/// part is a host — it carries a `.` or a `:`, or is `localhost` — and [`DOCKER_HUB_REGISTRY`]
/// otherwise, which is how a container runtime resolves a reference that names no registry.
fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((host, _)) if host.contains('.') || host.contains(':') || host == "localhost" => host,
        _ => DOCKER_HUB_REGISTRY,
    }
}

/// The WHOLE `stoppedReason` a task whose placement cannot reach the registry of `image` must
/// carry: the container runtime's error class, the reference the revision named, and the
/// registry that reference resolves to in the condition no path to it produces. Spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make this pass.
fn expected_stopped_reason(image: &str) -> String {
    let host = registry_host(image);
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: failed to do request: \
         Get \"https://{host}/v2/\": dial tcp {host}:443: i/o timeout"
    )
}

/// One emulator carrying Amazon EC2, IAM, Amazon ECR and Amazon ECS over ONE shared engine,
/// exactly as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
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

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and the signature are not credentials: the router above runs with request authentication
/// off, so nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
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

/// Drive one IAM action that must succeed, and answer its body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "iam", form(action, IAM_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One `awsJson1_1` RPC call that must succeed, answered with its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
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
    assert_eq!(
        status, 200,
        "{signing_name} {operation} answered {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await
}

/// One Amazon ECR call that must succeed.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        body,
    )
    .await
}

/// The network every row runs in, every id of it minted by Amazon EC2.
struct Network {
    routed_subnet: String,
    isolated_subnets: Vec<String>,
    security_group: String,
}

impl Network {
    /// The subnets one row's service is placed in, as its `awsvpcConfiguration` names them.
    fn subnets_of(&self, placement: Placement) -> Vec<String> {
        match placement {
            Placement::Isolated => self.isolated_subnets.clone(),
            Placement::Routed => vec![self.routed_subnet.clone()],
        }
    }
}

/// The repository of this account's registry the recorded revision's references address.
struct Registry {
    /// `Repository$repositoryUri`: "the URI for the repository. You can use this URI for
    /// container image push and pull operations".
    uri: String,
}

impl Registry {
    /// The reference that addresses this repository's `tag`.
    fn reference(&self, tag: &str) -> String {
        format!("{}:{tag}", self.uri)
    }
}

/// Create one subnet of this suite's VPC through Amazon EC2 itself.
async fn subnet(router: &MultiServiceRouter, vpc: &str, (cidr, zone): (&str, &str)) -> String {
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[("VpcId", vpc), ("CidrBlock", cidr), ("AvailabilityZone", zone)],
        )
        .await,
        "subnetId",
    )
}

/// Create a route table of this suite's VPC, associate it with every named subnet, and send
/// [`DEFAULT_ROUTE`] to `gateway` — or nowhere, for subnets with no route off the VPC.
///
/// Answers the table's id, which the caller reads the stored routes back out of.
async fn route_table(
    router: &MultiServiceRouter,
    vpc: &str,
    subnets: &[String],
    gateway: Option<&str>,
) -> String {
    let table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc)]).await,
        "routeTableId",
    );
    for subnet_id in subnets {
        ec2(
            router,
            "AssociateRouteTable",
            &[("RouteTableId", table.as_str()), ("SubnetId", subnet_id)],
        )
        .await;
    }
    if let Some(gateway) = gateway {
        ec2(
            router,
            "CreateRoute",
            &[
                ("RouteTableId", table.as_str()),
                ("DestinationCidrBlock", DEFAULT_ROUTE),
                ("GatewayId", gateway),
            ],
        )
        .await;
    }
    table
}

/// Create the network every row runs in — the VPC, an attached internet gateway, the routed
/// subnet, the two isolated subnets and the security group every placement names — and assert
/// the precondition every row rests on: the table in force on each placement's subnet carries a
/// default route exactly where it should.
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
    let routed_subnet = subnet(router, &vpc, ROUTED_SUBNET).await;
    let mut isolated_subnets = Vec::new();
    for cidr_and_zone in ISOLATED_SUBNETS {
        isolated_subnets.push(subnet(router, &vpc, cidr_and_zone).await);
    }
    // The routed subnet's own table sends everything outside the VPC to the gateway; the
    // isolated subnets share one table that sends it nowhere.
    let routed_table = route_table(
        router,
        &vpc,
        std::slice::from_ref(&routed_subnet),
        Some(internet_gateway.as_str()),
    )
    .await;
    let isolated_table = route_table(router, &vpc, &isolated_subnets, None).await;
    for (table, routed) in [(&routed_table, true), (&isolated_table, false)] {
        let described = ec2(
            router,
            "DescribeRouteTables",
            &[("RouteTableId.1", table.as_str())],
        )
        .await;
        assert_eq!(
            elements(&described, "destinationCidrBlock").contains(&DEFAULT_ROUTE.to_string()),
            routed,
            "setup: expected the table {table} to carry a default route: {routed}, got {described}"
        );
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "isorollout-group"),
                ("GroupDescription", "the group every placement below names"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        routed_subnet,
        isolated_subnets,
        security_group,
    }
}

/// Create the execution role Amazon ECS assumes to pull a task's images, through IAM's own API,
/// and answer its ARN.
async fn seed_execution_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", trust.as_str()),
        ],
    )
    .await;
    iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyArn", EXECUTION_ROLE_POLICY),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Create the repository the recorded revision's references address, through Amazon ECR's own
/// API, and answer what it published for it. Nothing is pushed into it.
async fn seed_registry(router: &MultiServiceRouter) -> Registry {
    let created = ecr(
        router,
        "CreateRepository",
        json!({ "repositoryName": REPOSITORY }),
    )
    .await;
    let uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateRepository must report a repositoryUri, got {created}"))
        .to_string();
    Registry { uri }
}

/// The container definitions one row's revision declares.
fn container_definitions(row: &Row, registry: &Registry) -> Value {
    let images = images_of(row.revision, registry);
    match row.revision {
        // The recorded shape: the log router is NOT essential — "if the `essential` parameter of
        // a container is marked as `false`, its failure doesn't affect the rest of the
        // containers in a task" — and carries the `firelensConfiguration` that makes it the
        // task's log router.
        Revision::FirelensPair => json!([
            {
                "name": LOG_ROUTER_CONTAINER,
                "image": images[0],
                "essential": false,
                "firelensConfiguration": { "type": FIRELENS_TYPE },
            },
            {
                "name": SERVICE_CONTAINER,
                "image": images[1],
                "essential": true,
            },
        ]),
        Revision::SingleService => json!([
            {
                "name": SERVICE_CONTAINER,
                "image": images[0],
                "essential": true,
            },
        ]),
    }
}

/// Register the revision one row's service runs: an `awsvpc` revision of the row's containers,
/// pulling as the execution role.
async fn register_revision(
    router: &MultiServiceRouter,
    row: &Row,
    registry: &Registry,
    execution_role: &str,
) {
    let (cpu, memory) = match row.revision {
        Revision::FirelensPair => (RECORDED_CPU, RECORDED_MEMORY),
        Revision::SingleService => (SMALL_CPU, SMALL_MEMORY),
    };
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family(),
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": cpu,
            "memory": memory,
            "executionRoleArn": execution_role,
            "containerDefinitions": container_definitions(row, registry),
        }),
    )
    .await;
}

/// The `awsvpcConfiguration` one placement runs under.
fn network_configuration(network: &Network, placement: Placement, assign_public_ip: &str) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": network.subnets_of(placement),
            "securityGroups": [network.security_group],
            "assignPublicIp": assign_public_ip,
        }
    })
}

/// What `DescribeServices` says about one row's service: the rollout a client polls, and the
/// count of tasks it is running.
#[derive(Debug, Clone)]
struct ServiceView {
    deployment_id: String,
    rollout_state: String,
    rollout_state_reason: String,
    running_count: i64,
    stability_status: String,
}

/// Read one row's service back through `DescribeServices`, off its `PRIMARY` deployment — the
/// first one it reports, which is the deployment a client reads the rollout state from.
async fn service_view(router: &MultiServiceRouter, row: &Row) -> ServiceView {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name()] }),
    )
    .await;
    let service = described["services"][0].clone();
    let deployment = service["deployments"][0].clone();
    assert_eq!(
        deployment["status"].as_str(),
        Some(DEPLOYMENT_PRIMARY),
        "case {:?}: the first deployment reported is the current one, got {service}",
        row.case
    );
    let text = |member: &str| {
        deployment[member]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    ServiceView {
        deployment_id: text("id"),
        rollout_state: text("rolloutState"),
        rollout_state_reason: text("rolloutStateReason"),
        running_count: service["runningCount"].as_i64().unwrap_or_default(),
        stability_status: text("stabilityStatus"),
    }
}

/// Assert one read of a row's service reports the rollout that COMPLETED, with the description
/// of that state the same rollout's own record carries.
///
/// The transition is one-way: `Deployment$rolloutState` gives the state machine no transition
/// out of `COMPLETED`, and the call that changed the count the service wants started no
/// deployment of its own — so a read that answered `IN_PROGRESS` here would be a rollout with
/// no end left to reach.
fn assert_completed_rollout(view: &ServiceView, case: &str, phase: &str) {
    assert_eq!(
        view.rollout_state, ROLLOUT_COMPLETED,
        "case {case:?} ({phase}): expected the rollout that reached its steady state to keep \
         it, got {view:?}"
    );
    assert_eq!(
        view.rollout_state_reason,
        format!("ECS deployment {} completed.", view.deployment_id),
        "case {case:?} ({phase}): expected the rollout to describe the state it is in, got \
         {view:?}"
    );
}

/// The task ARNs of one row's service with the `desiredStatus` `desired`.
async fn task_arns(router: &MultiServiceRouter, row: &Row, desired: &str) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name(),
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

/// The subnets the one `ElasticNetworkInterface` attachment of `task` names, asserting there is
/// exactly one such attachment and that it names a subnet at all.
fn attachment_subnets(task: &Value, case: &str) -> Vec<String> {
    let interfaces: Vec<&Value> = task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|attachment| attachment["type"].as_str() == Some(ELASTIC_NETWORK_INTERFACE))
        .collect();
    assert_eq!(
        interfaces.len(),
        1,
        "case {case:?}: an awsvpc task is allocated exactly one elastic network interface, got \
         {task}"
    );
    interfaces
        .iter()
        .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
        .filter(|detail| detail["name"].as_str() == Some(SUBNET_ID_DETAIL))
        .filter_map(|detail| detail["value"].as_str().map(str::to_string))
        .collect()
}

/// Poll `probe` until it answers `Some`, or the budget runs out. The wait is for the CONDITION:
/// every move below is one the clock owes, not one a call makes.
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

/// Every task of one row's service, as a line per task: what a client watching the rollout can
/// see about where its tasks got to. Read for a failure message, so a rollout that never
/// settles names the tasks that kept it from settling.
async fn task_summary(router: &MultiServiceRouter, row: &Row) -> String {
    let mut lines: Vec<String> = Vec::new();
    for desired in [RUNNING, STOPPED] {
        for arn in task_arns(router, row, desired).await {
            let task = describe_task(router, &arn).await;
            lines.push(format!(
                "desiredStatus={desired} lastStatus={:?} stopCode={:?} stoppedReason={:?} \
                 containers={:?}",
                task["lastStatus"],
                task["stopCode"],
                task["stoppedReason"],
                task["containers"],
            ));
        }
    }
    lines.join("; ")
}

/// Wait until one row's rollout has left [`ROLLOUT_IN_PROGRESS`], and answer the service as it
/// reads then. `phase` names which of the scenario's two waits this is.
async fn settled_rollout(router: &MultiServiceRouter, row: &Row, phase: &str) -> ServiceView {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let view = service_view(router, row).await;
        if view.rollout_state == ROLLOUT_COMPLETED || view.rollout_state == ROLLOUT_FAILED {
            return view;
        }
        // The state machine has three members, so a rollout that has not settled is the one a
        // client polls THROUGH rather than an unknown state.
        assert_eq!(
            view.rollout_state, ROLLOUT_IN_PROGRESS,
            "case {:?} ({phase}): a rollout that has not settled is in progress, got {view:?}",
            row.case
        );
        assert!(
            Instant::now() < deadline,
            "case {:?} ({phase}): no terminal rolloutState of {} within {POLL_BUDGET:?}, last \
             {view:?}, tasks: {}",
            row.case,
            row.service_name(),
            task_summary(router, row).await
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn an_isolated_subnet_rollout_and_its_tasks_reach_a_terminal_verdict() {
    let router = emulator();
    let network = seed_network(&router).await;
    let execution_role = seed_execution_role(&router).await;
    let registry = seed_registry(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // Every row's service is created with the count ITS row names: three of them want NO tasks,
    // which is a service that already holds the task set it wants, and the fourth wants the task
    // its placement cannot start from the create on.
    for row in &ROWS {
        register_revision(&router, row, &registry, &execution_role).await;
        let created = ecs(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name(),
                "taskDefinition": row.family(),
                "desiredCount": row.created_desired,
                "launchType": "FARGATE",
                "networkConfiguration": network_configuration(
                    &network,
                    row.placement,
                    row.assign_public_ip,
                ),
                // The breaker is what gives a rollout that cannot reach a steady state an END:
                // "If the service fails to reach a steady state and circuit breaker is turned
                // on, the deployment transitions to a FAILED state". No rollback is configured,
                // so the state such a rollout reaches is the one it keeps.
                "deploymentConfiguration": {
                    "deploymentCircuitBreaker": { "enable": true, "rollback": false },
                },
            }),
        )
        .await;
        assert_eq!(
            created["service"]["desiredCount"].as_i64(),
            Some(row.created_desired),
            "case {:?}: the created service must want {} task(s), got {created}",
            row.case,
            row.created_desired
        );
    }
    // The services that want nothing settle on their own, before anything is placed: that
    // `COMPLETED` is the state the scale-up below must not take back.
    for row in ROWS.iter().filter(|row| row.created_desired == 0) {
        let case = row.case;
        let view = settled_rollout(&router, row, "wanting no tasks").await;
        assert_completed_rollout(&view, case, "wanting no tasks");
        assert_eq!(
            view.running_count, 0,
            "case {case:?}: a service that placed nothing runs nothing, got {view:?}"
        );
        let listed = task_arns(&router, row, RUNNING).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a service that wants no tasks has none, got {listed:?}"
        );
    }

    // The one call the scenario is about: every row that scales is scaled UP, so the clock-owed
    // placements of all of them fall due together rather than one row's budget after another's.
    for row in ROWS.iter().filter(|row| row.scale_to.is_some()) {
        let desired = row.desired();
        let updated = ecs(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service_name(),
                "desiredCount": desired,
            }),
        )
        .await;
        assert_eq!(
            updated["service"]["desiredCount"].as_i64(),
            Some(desired),
            "case {:?}: the scaled service must want {desired} task(s), got {updated}",
            row.case
        );
        // Scaling gives the SCHEDULER work, and the member that says the service has not
        // converged on what it wants is the deployment's own `stabilityStatus` — "The task
        // `runningCount` is equal to the `computedDesiredCount`" is what a steady one reports —
        // not the state of a rollout that is already over.
        let scaled = service_view(&router, row).await;
        assert_completed_rollout(&scaled, row.case, "straight after the scale-up");
    }

    for row in &ROWS {
        let case = row.case;
        let placed: BTreeSet<String> = network.subnets_of(row.placement).into_iter().collect();
        let expected_containers = row.containers();
        let view = settled_rollout(&router, row, "the count it ends up wanting").await;
        assert_eq!(
            view.rollout_state, row.rollout,
            "case {case:?}: expected the rollout to end {}, got {view:?}",
            row.rollout
        );
        match row.rollout {
            // A rollout the breaker failed says what failed it, which is neither of the two
            // descriptions a rollout that is under way or over carries.
            ROLLOUT_FAILED => {
                for description in [
                    format!("ECS deployment {} in progress.", view.deployment_id),
                    format!("ECS deployment {} completed.", view.deployment_id),
                ] {
                    assert_ne!(
                        view.rollout_state_reason, description,
                        "case {case:?}: a failed rollout says why it failed, got {view:?}"
                    );
                }
                assert!(
                    !view.rollout_state_reason.is_empty(),
                    "case {case:?}: a failed rollout says why it failed, got {view:?}"
                );
            }
            _ => assert_completed_rollout(&view, case, "the count it ends up wanting"),
        }

        match row.verdict {
            Verdict::Runs => {
                // The images can be fetched from this placement, so every task the service
                // wants RUNS, each reporting when it started and the subnet its own interface
                // was created in.
                let wanted = usize::try_from(row.desired()).unwrap_or_default();
                let tasks = poll_until(
                    || async {
                        let mut tasks: Vec<Value> = Vec::new();
                        for arn in task_arns(&router, row, RUNNING).await {
                            let task = describe_task(&router, &arn).await;
                            if task["lastStatus"].as_str() == Some(RUNNING) {
                                tasks.push(task);
                            }
                        }
                        (tasks.len() == wanted).then_some(tasks)
                    },
                    &format!("case {case:?}: {wanted} running task(s) of {}", row.service_name()),
                )
                .await;
                for task in &tasks {
                    assert!(
                        task["startedAt"].as_f64().is_some(),
                        "case {case:?}: a running task reports when it started, got {task}"
                    );
                    assert_eq!(
                        task["stopCode"].as_str(),
                        None,
                        "case {case:?}: a task that started carries no stop code, got {task}"
                    );
                    let subnets = attachment_subnets(task, case);
                    assert!(
                        subnets.iter().all(|subnet| placed.contains(subnet)) && !subnets.is_empty(),
                        "case {case:?}: expected the interface in one of {placed:?}, got \
                         {subnets:?} from {task}"
                    );
                }
                // A service that holds the task set it wants is one whose deployment has
                // STABILIZED, which is the convergence read a client has beside the rollout.
                let settled = poll_until(
                    || async {
                        let view = service_view(&router, row).await;
                        (view.stability_status == STEADY_STATE).then_some(view)
                    },
                    &format!("case {case:?}: a steady deployment of {}", row.service_name()),
                )
                .await;
                assert_eq!(
                    settled.running_count,
                    row.expected_running_count(),
                    "case {case:?}: expected the service to run {} task(s), got {settled:?}",
                    row.expected_running_count()
                );
                assert_completed_rollout(&settled, case, "holding the tasks it wants");
                continue;
            }
            Verdict::FailsToStart => {}
        }

        // The images cannot be fetched from this placement, so every task the scheduler
        // launched is STOPPED before it ever ran: the reason names the reference it could not
        // resolve, nothing reports a start or an exit code, and the interface the task was
        // allocated still names the subnet it was placed in.
        let stopped = poll_until(
            || async {
                for arn in task_arns(&router, row, STOPPED).await {
                    let task = describe_task(&router, &arn).await;
                    // A task Amazon ECS marks for the stop a failed launch earns is on its way
                    // out like any other stopping task, so the one read here is the task that
                    // has ARRIVED at `STOPPED`.
                    if task["lastStatus"].as_str() == Some(STOPPED) {
                        return Some(task);
                    }
                }
                None
            },
            &format!("case {case:?}: a stopped task of {}", row.service_name()),
        )
        .await;
        assert_eq!(
            stopped["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose image could not be fetched never started, got {stopped}"
        );
        let first_image = images_of(row.revision, &registry)
            .first()
            .cloned()
            .expect("every revision here declares a container");
        let expected = expected_stopped_reason(&first_image);
        assert_eq!(
            stopped["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {stopped}"
        );
        assert_eq!(
            stopped["startedAt"].as_f64(),
            None,
            "case {case:?}: a task that never ran has no startedAt, got {stopped}"
        );
        let containers = stopped["containers"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            containers
                .iter()
                .filter_map(|container| container["name"].as_str())
                .collect::<Vec<&str>>(),
            expected_containers,
            "case {case:?}: the stopped task reports every container of its revision, got \
             {stopped}"
        );
        for container in &containers {
            assert_eq!(
                container["lastStatus"].as_str(),
                Some(STOPPED),
                "case {case:?}: every container of a stopped task is stopped, got {container}"
            );
            assert_eq!(
                container["exitCode"].as_i64(),
                None,
                "case {case:?}: a container that never started reports no exit code, got \
                 {container}"
            );
        }
        let subnets = attachment_subnets(&stopped, case);
        assert!(
            subnets.iter().all(|subnet| placed.contains(subnet)) && !subnets.is_empty(),
            "case {case:?}: expected the interface in one of {placed:?}, got {subnets:?} from \
             {stopped}"
        );
        let settled = service_view(&router, row).await;
        assert_eq!(
            settled.running_count, 0,
            "case {case:?}: a service whose tasks cannot start runs none, got {settled:?}"
        );
        assert_eq!(
            settled.stability_status, STABILIZING,
            "case {case:?}: a service short of the tasks it wants has not stabilized, got \
             {settled:?}"
        );
        assert_eq!(
            settled.rollout_state, row.rollout,
            "case {case:?}: a rollout that reached {} keeps it, got {settled:?}",
            row.rollout
        );
    }

    // The positive control a client reads beside the rollout: one task run straight into the
    // routed subnet with a public address reaches `RUNNING`, so a client can tell a placement
    // that cannot start a task from one that can.
    let control = ecs(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": ROWS[2].family(),
            "launchType": "FARGATE",
            "count": 1,
            "networkConfiguration": network_configuration(
                &network,
                Placement::Routed,
                ASSIGN_PUBLIC_IP_ENABLED,
            ),
        }),
    )
    .await;
    let control_arn = control["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("RunTask must report the task it accepted, got {control}"))
        .to_string();
    let case = "the control task: the routed subnet, with a public address";
    let running = poll_until(
        || async {
            let task = describe_task(&router, &control_arn).await;
            (task["lastStatus"].as_str() == Some(RUNNING)).then_some(task)
        },
        &format!("case {case:?}: the control task {control_arn} running"),
    )
    .await;
    assert!(
        running["startedAt"].as_f64().is_some(),
        "case {case:?}: a running task reports when it started, got {running}"
    );
    assert_eq!(
        running["stopCode"].as_str(),
        None,
        "case {case:?}: a task that started carries no stop code, got {running}"
    );
    assert_eq!(
        attachment_subnets(&running, case),
        vec![network.routed_subnet.clone()],
        "case {case:?}: expected the interface in the subnet the run named, got {running}"
    );
}
