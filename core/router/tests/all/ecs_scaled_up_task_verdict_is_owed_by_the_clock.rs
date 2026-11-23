// SPDX-License-Identifier: CC0-1.0

//! The verdict a scaled-up Amazon ECS task reaches is owed by the CLOCK, so a client gets it
//! whichever read it waits on.
//!
//! RULE: a task Amazon ECS accepts moves through the lifecycle on its own, so every launched
//! task reaches a terminal `Task$lastStatus` ("the last known status for the task", with the
//! documented PROVISIONING/PENDING/RUNNING and DEACTIVATING/STOPPING/STOPPED lifecycles):
//! `RUNNING` with `Task$startedAt` ("the time when the task transitioned from the `PENDING`
//! state to the `RUNNING` state") where its placement can fetch the images its containers run,
//! and `STOPPED` with the `TaskStopCode` member for a task whose containers were never started
//! (`Task$stopCode`: "The stop code indicating why a task was stopped. The stoppedReason might
//! contain additional details"), a `Task$stoppedReason` carrying the container runtime's
//! `CannotPullContainerError` for the reference it could not resolve, NO `startedAt` and no
//! `exitCode` on any container where it cannot. The move belongs to the task, not to the call
//! that looks: a reporting read materialises what was already due and changes nothing a later
//! caller sees, so the verdict arrives for a client waiting on `DescribeTasks`, for one waiting
//! on `ListTasks` alone, and for one watching only `DescribeServices$runningCount` ("the number
//! of tasks in the cluster that are in the `RUNNING` state"). Either way an
//! `awsvpc` task carries exactly one `ElasticNetworkInterface` attachment whose
//! `Attachment$details` name the subnet it was placed in ("for elastic network interfaces, this
//! includes the network interface ID, the MAC address, the subnet ID, and the private IPv4
//! address"), and whether the placement reaches the registry is Amazon VPC's answer, read off
//! the route table in force on that subnet: a subnet with no route off the VPC reaches neither a
//! public registry nor the endpoints Amazon ECR publishes into a VPC that has none, whether or
//! not the launch asked for a public address, while a subnet routed to an attached internet
//! gateway carries the traffic of a task that was given one.
//!
//! It lives beside the router because the estate the rule is read over is three services':
//! Amazon EC2 owns the VPC, the subnets, their one shared isolated route table and the internet
//! gateway the path is read off, Amazon ECR owns the private registry a revision's references
//! address, and Amazon ECS owns the service, the scale-up and the tasks. Every fact is created
//! through the owning service's own API, and every value asserted is read back through
//! `ListTasks`, `DescribeTasks` and `DescribeServices`.
//!
//! The rows differ along what the rule generalises over: WHICH read the client waits on while
//! the task settles — `DescribeTasks`, `ListTasks(desiredStatus=STOPPED)` and
//! `DescribeServices$runningCount` — and, with it, where the placement's default route goes
//! (nowhere, an attached internet gateway), whether the launch asked for a public address, how
//! many containers the revision declares and which registry their references address (two
//! containers, a non-essential FireLens log router beside the essential service container,
//! pulling from a repository of this account's own Amazon ECR registry; one pulling from a
//! public registry), and where the task ends. Each row is placed in subnets of its OWN, so no
//! row's placement can be satisfied by another's task. That a verdict needs NO Amazon ECS read
//! at all to arrive — the lifecycle watched in Amazon EC2's own inventory of the interface the
//! task was allocated — is pinned in the owning crate's
//! `tests/all/task_verdict_is_owed_by_the_clock.rs`.

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

/// The cluster every service below runs in.
const CLUSTER: &str = "clockverdict";

/// The two containers of the two-container revision: a FireLens log router, which is NOT
/// essential, and the service container, which is.
const LOG_ROUTER_CONTAINER: &str = "log-router";
const SERVICE_CONTAINER: &str = "service";

/// The log router kind the FireLens container declares
/// (`smithy_query(query="shape",shape="FirelensConfigurationType")` -> fluentbit, fluentd).
const FIRELENS_TYPE: &str = "fluentbit";

/// The network every row runs in: one VPC, one subnet routed to an attached internet gateway,
/// and two PAIRS of subnets sharing one route table that carries no route off the VPC at all —
/// a pair per isolated row, so no row's placement can be satisfied by another's task.
const VPC_CIDR: &str = "10.96.0.0/16";
const ROUTED_SUBNET: (&str, &str) = ("10.96.0.0/20", "us-east-1a");
const ISOLATED_SUBNET_PAIRS: [[(&str, &str); 2]; 2] = [
    [
        ("10.96.16.0/20", "us-east-1a"),
        ("10.96.32.0/20", "us-east-1b"),
    ],
    [
        ("10.96.48.0/20", "us-east-1a"),
        ("10.96.64.0/20", "us-east-1b"),
    ],
];

/// The destination of the route that carries traffic OFF a VPC: "a route table contains a set of
/// rules, called routes" and the one for everything outside the VPC is the default route (Amazon
/// VPC User Guide, "Route tables").
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The repository of this account's own Amazon ECR registry the two-container revision's
/// references address, and the tags they name. NOTHING is pushed into it: what these rows turn on
/// is whether the placement can REACH the registry, which is decided before any manifest is
/// asked for.
const REPOSITORY: &str = "clockverdict/app";
const LOG_ROUTER_TAG: &str = "log-router";
const SERVICE_TAG: &str = "service";

/// An image in a PUBLIC container registry, whose registry is no AWS service and so is reachable
/// only over the internet.
const PUBLIC_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The execution role Amazon ECS assumes to pull a task's images, trusted by the Amazon ECS tasks
/// principal, and the managed policy AWS publishes for one ("AmazonECSTaskExecutionRolePolicy",
/// the task execution role of the Amazon ECS Developer Guide) — attached rather than
/// hand-written, so the launch is decided over the document IAM really holds.
const EXECUTION_ROLE: &str = "clockverdictexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The Fargate task sizes the revisions declare (`TaskDefinition$cpu`: "2048 (2 vCPU) - Available
/// memory values: 4096 (4 GB) and 16384 (16 GB) in 1024 (1 GB) increments"), the larger for the
/// two-container revision and the smallest for the single-container ones.
const PAIR_CPU: &str = "2048";
const PAIR_MEMORY: &str = "4096";
const SMALL_CPU: &str = "256";
const SMALL_MEMORY: &str = "512";

/// `AwsVpcConfiguration$assignPublicIp` as each row names it: "whether the task's elastic network
/// interface receives a public IP address".
const ASSIGN_PUBLIC_IP_ENABLED: &str = "ENABLED";
const ASSIGN_PUBLIC_IP_DISABLED: &str = "DISABLED";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
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

/// How long a wait below keeps looking, and how often. Generous on purpose, by well over an order
/// of magnitude over the spans the transitions themselves take: the deadline is only here so a
/// failure is a failure rather than a hang, and a run sharing a loaded machine with every other
/// suite of the workspace must not be read as one.
const POLL_BUDGET: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Where one row's task is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// The row's own pair of subnets, whose shared table carries no default route.
    Isolated(usize),
    /// The subnet routed to the VPC's attached internet gateway.
    Routed,
}

/// What one row's revision declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Revision {
    /// A non-essential FireLens log router beside the essential service container, both pulling
    /// from this account's own Amazon ECR registry.
    FirelensPair,
    /// One essential container pulling from a public registry.
    SingleService,
}

/// WHICH read the row's client waits on while its task settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watch {
    /// `DescribeTasks`, the read a client that holds a task ARN polls.
    DescribedTask,
    /// `ListTasks(desiredStatus=STOPPED)` alone — nothing is described until the end.
    StoppedListing,
    /// `DescribeServices$runningCount` alone.
    ServiceRunningCount,
}

/// Where one row's task ENDS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The images can be fetched from this placement: the task runs.
    Runs,
    /// They cannot: the task stops before it ever ran.
    FailsToStart,
}

/// One row: what the client waits on, where its task is placed, what it runs and where it ends.
struct Row {
    case: &'static str,
    /// The name the row's family and service are built from.
    name: &'static str,
    watch: Watch,
    placement: Placement,
    assign_public_ip: &'static str,
    revision: Revision,
    verdict: Verdict,
}

const ROWS: [Row; 3] = [
    Row {
        case: "a client waiting on DescribeTasks: two isolated subnets, a FireLens pair out of \
               this account's registry, no public address",
        name: "described",
        watch: Watch::DescribedTask,
        placement: Placement::Isolated(0),
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        revision: Revision::FirelensPair,
        verdict: Verdict::FailsToStart,
    },
    Row {
        case: "a client waiting on ListTasks alone: isolated subnets where the launch asked for \
               a public address anyway, pulling from a public registry",
        name: "listed",
        watch: Watch::StoppedListing,
        placement: Placement::Isolated(1),
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        revision: Revision::SingleService,
        verdict: Verdict::FailsToStart,
    },
    Row {
        case: "a client waiting on the service's runningCount: the routed subnet with a public \
               address, pulling from a public registry",
        name: "counted",
        watch: Watch::ServiceRunningCount,
        placement: Placement::Routed,
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        revision: Revision::SingleService,
        verdict: Verdict::Runs,
    },
];

impl Row {
    /// The task-definition family the row's revision is registered under.
    fn family(&self) -> String {
        format!("clockverdict-{}", self.name)
    }

    /// The service the row scales up.
    fn service_name(&self) -> String {
        format!("clockverdict-{}", self.name)
    }

    /// The status the row's task ENDS at — the `desiredStatus` it is listed under and the
    /// `lastStatus` it reports once it has arrived there.
    ///
    /// One value for both, because what a row waits for is the task having ARRIVED where it was
    /// wanted: a task Amazon ECS has turned away from running is wanted `STOPPED` and reports
    /// `STOPPED` at its end, and a task that is wanted `RUNNING` reports `RUNNING` at its own. The
    /// two members differ only while a task is still on its way there, which is what both waits
    /// below look through.
    fn terminal_status(&self) -> &'static str {
        match self.verdict {
            Verdict::Runs => RUNNING,
            Verdict::FailsToStart => STOPPED,
        }
    }

    /// `Service$runningCount` the row's service must report once its task reached its verdict.
    fn expected_running_count(&self) -> i64 {
        match self.verdict {
            Verdict::Runs => 1,
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

/// The REGISTRY an image reference is fetched from: the part before the first `/`, which every
/// reference below names explicitly. Panics on a reference of any other shape, so a row added
/// later cannot quietly compare against the wrong host.
fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((host, _)) if host.contains('.') => host,
        _ => panic!("every reference in this suite names its registry, got {image:?}"),
    }
}

/// The WHOLE `stoppedReason` a task whose placement cannot reach the registry of `image` must
/// carry: the container runtime's error class, the reference the revision named, and the registry
/// that reference resolves to in the condition no path to it produces. Spelled out here rather
/// than imported, so a wrong constant in the implementation cannot make this pass.
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
/// and the signature are not credentials: the router above runs with request authentication off,
/// so nothing is verified.
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
        "EC2 {action} must succeed, got {status} with body {body}"
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
    isolated_pairs: Vec<Vec<String>>,
}

impl Network {
    /// The subnets one row's service is placed in, as its `awsvpcConfiguration` names them.
    fn subnets_of(&self, placement: Placement) -> Vec<String> {
        match placement {
            Placement::Isolated(pair) => self.isolated_pairs[pair].clone(),
            Placement::Routed => vec![self.routed_subnet.clone()],
        }
    }
}

/// The repository of this account's registry the two-container revision's references address.
struct Registry {
    /// `Repository$repositoryUri`: "the URI for the repository. You can use this URI for container
    /// image push and pull operations".
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
/// subnet, the three pairs of isolated subnets and the security group every placement names — and
/// assert the precondition every row rests on: the table in force on each placement's subnets
/// carries a default route exactly where it should.
async fn seed_network(router: &MultiServiceRouter) -> (Network, String) {
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
    let mut isolated_pairs: Vec<Vec<String>> = Vec::new();
    for pair in ISOLATED_SUBNET_PAIRS {
        let mut created = Vec::new();
        for cidr_and_zone in pair {
            created.push(subnet(router, &vpc, cidr_and_zone).await);
        }
        isolated_pairs.push(created);
    }
    // The routed subnet's own table sends everything outside the VPC to the gateway; every
    // isolated subnet shares one table that sends it nowhere.
    let routed_table = route_table(
        router,
        &vpc,
        std::slice::from_ref(&routed_subnet),
        Some(internet_gateway.as_str()),
    )
    .await;
    let isolated: Vec<String> = isolated_pairs.iter().flatten().cloned().collect();
    let isolated_table = route_table(router, &vpc, &isolated, None).await;
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
                ("GroupName", "clockverdict-group"),
                ("GroupDescription", "the group every placement below names"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (
        Network {
            routed_subnet,
            isolated_pairs,
        },
        security_group,
    )
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

/// Create the repository the two-container revision's references address, through Amazon ECR's
/// own API, and answer what it published for it. Nothing is pushed into it.
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
        // The log router is NOT essential — "if the `essential` parameter of a container is marked
        // as `false`, its failure doesn't affect the rest of the containers in a task" — and
        // carries the `firelensConfiguration` that makes it the task's log router.
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

/// Register the revision one row's service runs — an `awsvpc` Fargate revision of the row's
/// containers, pulling as the execution role — and answer the ARN Amazon ECS minted for it.
async fn register_revision(
    router: &MultiServiceRouter,
    row: &Row,
    registry: &Registry,
    execution_role: &str,
) -> String {
    let (cpu, memory) = match row.revision {
        Revision::FirelensPair => (PAIR_CPU, PAIR_MEMORY),
        Revision::SingleService => (SMALL_CPU, SMALL_MEMORY),
    };
    let registered = ecs(
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
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("RegisterTaskDefinition must report the revision it stored, got {registered}")
        })
        .to_string()
}

/// The `awsvpcConfiguration` one row runs under.
fn network_configuration(row: &Row, network: &Network, security_group: &str) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": network.subnets_of(row.placement),
            "securityGroups": [security_group],
            "assignPublicIp": row.assign_public_ip,
        }
    })
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

/// The `taskArn` of one task as it was reported, which is the id a re-read addresses it by.
fn task_arn_of(task: &Value, case: &str) -> String {
    task["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: every reported task names itself, got {task}"))
        .to_string()
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

/// `Service$runningCount` of one row's service.
async fn running_count(router: &MultiServiceRouter, row: &Row) -> i64 {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name()] }),
    )
    .await;
    described["services"][0]["runningCount"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!(
                "DescribeServices must report a runningCount for {}, got {described}",
                row.service_name()
            )
        })
}

/// The one `ElasticNetworkInterface` attachment of `task`, asserting an `awsvpc` task carries
/// exactly one.
fn interface_attachment(task: &Value, case: &str) -> Value {
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
    interfaces[0].clone()
}

/// The subnets the one `ElasticNetworkInterface` attachment of `task` names.
fn attachment_subnets(task: &Value, case: &str) -> Vec<String> {
    let attachment = interface_attachment(task, case);
    attachment["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|detail| detail["name"].as_str() == Some(SUBNET_ID_DETAIL))
        .filter_map(|detail| detail["value"].as_str().map(str::to_string))
        .collect()
}

/// Assert that `document` carries NO value for `member` — absent, or present as `null`.
///
/// Read as "absence", not as "no value of the type I expected": a member present with a value of
/// another JSON type would satisfy a typed read of `None`, so a wrongly typed `stopCode` or
/// `startedAt` would slip through such a check.
fn assert_absent(document: &Value, member: &str, case: &str) {
    assert!(
        document.get(member).is_none_or(Value::is_null),
        "case {case:?}: expected no {member} at all, got {document}"
    );
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

/// Wait on the ONE read the row's client watches, until it shows the task has reached its
/// verdict — and on nothing else, so each row is the rule read through a different window.
async fn wait_on_the_rows_own_read(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    match row.watch {
        // The read a client holding a task ARN polls: the task's own reported status.
        Watch::DescribedTask => {
            poll_until(
                || async {
                    for arn in task_arns(router, row, row.terminal_status()).await {
                        let task = describe_task(router, &arn).await;
                        // A task marked for the stop a failed launch earns is on its way out like
                        // any other stopping task, so what is waited for is the ARRIVAL.
                        if task["lastStatus"].as_str() == Some(row.terminal_status()) {
                            return Some(());
                        }
                    }
                    None
                },
                &format!("case {case:?}: a described task at {}", row.terminal_status()),
            )
            .await;
        }
        // The listing alone: a client that polls only for the tasks Amazon ECS has turned away
        // from running learns of the failure without describing anything.
        Watch::StoppedListing => {
            poll_until(
                || async {
                    (!task_arns(router, row, STOPPED).await.is_empty()).then_some(())
                },
                &format!("case {case:?}: ListTasks naming a task wanted {STOPPED}"),
            )
            .await;
        }
        // The count alone: the member a client watching a rollout reads to see its tasks come up.
        Watch::ServiceRunningCount => {
            poll_until(
                || async {
                    (running_count(router, row).await == row.expected_running_count())
                        .then_some(())
                },
                &format!(
                    "case {case:?}: runningCount reaching {}",
                    row.expected_running_count()
                ),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn every_scaled_up_task_reaches_its_verdict_whichever_read_the_client_waits_on() {
    let router = emulator();
    let (network, security_group) = seed_network(&router).await;
    let execution_role = seed_execution_role(&router).await;
    let registry = seed_registry(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    for row in &ROWS {
        let case = row.case;
        let placed = network.subnets_of(row.placement);
        let revision_arn = register_revision(&router, row, &registry, &execution_role).await;
        // Every row's service is created wanting NO tasks and then asked for one, which is the
        // sequence a client scaling a settled service up makes.
        let created = ecs(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name(),
                "taskDefinition": row.family(),
                "desiredCount": 0,
                "launchType": "FARGATE",
                "networkConfiguration": network_configuration(row, &network, &security_group),
            }),
        )
        .await;
        assert_eq!(
            created["service"]["desiredCount"].as_i64(),
            Some(0),
            "case {case:?}: the created service must want no tasks yet, got {created}"
        );
        let updated = ecs(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service_name(),
                "desiredCount": 1,
            }),
        )
        .await;
        assert_eq!(
            updated["service"]["desiredCount"].as_i64(),
            Some(1),
            "case {case:?}: the scale-up must be accepted, got {updated}"
        );

        wait_on_the_rows_own_read(&router, row).await;

        // Whatever the client waited on, the verdict is there to be read: the task Amazon ECS
        // launched is listed under the `desiredStatus` its end belongs to, and reports that end.
        let arns = task_arns(&router, row, row.terminal_status()).await;
        assert!(
            !arns.is_empty(),
            "case {case:?}: expected at least one task wanted {}, got {arns:?}",
            row.terminal_status()
        );
        let task = poll_until(
            || async {
                for arn in &arns {
                    let task = describe_task(&router, arn).await;
                    if task["lastStatus"].as_str() == Some(row.terminal_status()) {
                        return Some(task);
                    }
                }
                None
            },
            &format!("case {case:?}: a task at {}", row.terminal_status()),
        )
        .await;
        assert_eq!(
            task["taskDefinitionArn"].as_str(),
            Some(revision_arn.as_str()),
            "case {case:?}: the task runs the revision the service deploys, got {task}"
        );
        let subnets = attachment_subnets(&task, case);
        assert!(
            subnets.len() == 1 && placed.contains(&subnets[0]),
            "case {case:?}: expected the interface in one of {placed:?}, got {subnets:?} from \
             {task}"
        );
        assert_eq!(
            running_count(&router, row).await,
            row.expected_running_count(),
            "case {case:?}: expected the service to run {} task(s), got {task}",
            row.expected_running_count()
        );

        match row.verdict {
            // The images can be fetched from this placement, so the task RAN: it reports when it
            // started and carries no stop of any kind.
            Verdict::Runs => {
                assert!(
                    task["startedAt"].as_f64().is_some(),
                    "case {case:?}: a running task reports when it started, got {task}"
                );
                for member in ["stopCode", "stoppedReason"] {
                    assert_absent(&task, member, case);
                }
                // And reading it again moves nothing: the start was owed to the clock, so a
                // reporting call materialises what was already due rather than taking the task
                // a step further — the direction where an extra read could plausibly advance a
                // task, since a running one still has moves ahead of it.
                let again = describe_task(&router, &task_arn_of(&task, case)).await;
                for member in ["lastStatus", "desiredStatus", "startedAt", "version"] {
                    assert_eq!(
                        again[member], task[member],
                        "case {case:?}: a second read of a running task must report the same \
                         {member}, got {again}"
                    );
                }
                continue;
            }
            Verdict::FailsToStart => {}
        }

        // The images cannot be fetched from this placement, so the task STOPPED before it ever
        // ran: the stop names the class of failure and the reference it could not resolve, and
        // nothing reports a start or an exit code.
        assert_eq!(
            task["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose image could not be fetched never started, got {task}"
        );
        let first_image = images_of(row.revision, &registry)
            .first()
            .cloned()
            .expect("every revision here declares a container");
        let expected = expected_stopped_reason(&first_image);
        assert_eq!(
            task["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {task}"
        );
        assert_absent(&task, "startedAt", case);
        let containers = task["containers"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            containers
                .iter()
                .filter_map(|container| container["name"].as_str())
                .collect::<Vec<&str>>(),
            row.containers(),
            "case {case:?}: expected the containers the revision declares, got {task}"
        );
        for container in &containers {
            assert_eq!(
                container["lastStatus"].as_str(),
                Some(STOPPED),
                "case {case:?}: every container of a stopped task is stopped, got {container}"
            );
            assert_absent(container, "exitCode", case);
        }

        // And reading it again changes nothing: the move was owed to the clock, so a reporting
        // call materialises what was already due rather than taking the task a step further.
        let again = describe_task(&router, &task_arn_of(&task, case)).await;
        assert_eq!(
            again, task,
            "case {case:?}: a second read of a task that has stopped must report it unchanged"
        );
    }
}
