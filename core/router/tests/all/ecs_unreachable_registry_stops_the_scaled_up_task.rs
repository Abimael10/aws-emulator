// SPDX-License-Identifier: CC0-1.0

//! Scaling a load-balanced Amazon ECS service UP into a placement that cannot reach the
//! registry its image is held in stops the task the scheduler launches and registers NOTHING
//! in the target group the service binds.
//!
//! RULE: a container runs an image, and the image is fetched from its registry before the
//! container starts, so a task whose placement has no path to that registry never runs: it is
//! reported `STOPPED` with the `TaskStopCode` member for a task that failed before its
//! containers started (`Task$stopCode`, "the stop code indicating why a task was stopped") and
//! a `Task$stoppedReason` ("The reason that the task was stopped") carrying the container
//! runtime's `CannotPullContainerError` for the reference it could not resolve. The
//! registrations Amazon ECS owns in a target group are the ones its RUNNING tasks need —
//! `LoadBalancer$targetGroupArn` is the group "associated with a service", and a task of an
//! `awsvpc` service is a target at the private address of the interface it was allocated — so
//! a task that never ran is a target of nothing, and `DescribeTargetHealth` reports the group
//! empty however long a client polls it. Whether the placement reaches the registry is Amazon
//! VPC's answer, off the route table in force on the placement's subnet: a default route to an
//! internet gateway carries the traffic of a task that was given a public address
//! (`AwsVpcConfiguration$assignPublicIp`), a public NAT gateway carries it for a task without
//! one ("with a public NAT gateway, internet-bound traffic from a private subnet can be routed
//! to the NAT gateway ... with a private NAT gateway, internet-bound traffic ... can't be
//! routed to the gateway", `CreateNatGateway`), and a subnet with no route off the VPC reaches
//! no public registry at all.
//!
//! The suite lives beside the router because three services have to agree for the rule to
//! hold: Amazon EC2 owns the subnets, route tables and gateways the path is read off, Amazon
//! ECS owns the service, the scale-up and the task it launches, and Elastic Load Balancing v2
//! owns the registration and the health it reports. Every fact is created through the owning
//! service's own API and every value asserted is read back through `DescribeTasks`,
//! `DescribeServices` and `DescribeTargetHealth` — the three reads a client diagnosing "did my
//! task fail to start, or is it still coming up?" has.
//!
//! The rows differ along what the rule generalises over: where the placement's default route
//! goes (nowhere, an internet gateway, a public NAT gateway, a private one), whether the task
//! asked for a public address, the registry the image reference resolves to (one the reference
//! names, and Docker Hub for a reference that names none) and how many tasks the scale-up
//! asks for. The recorded configuration — one task in a subnet with no route off the VPC — is
//! a single row.
//!
//! The path itself is tabled over a single launch in the ECS crate's own
//! `tests/all/image_pull_placement.rs`; what this suite adds is the SERVICE half: the scale-up
//! that schedules the task, and the target group that must stay empty because of it.

use std::collections::BTreeSet;
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
const CLUSTER: &str = "unreachableregistry";
const CONTAINER: &str = "app";

/// The port the container declares a `portMapping` for, and the port every binding names.
const CONTAINER_PORT: i64 = 80;

/// The network every row runs in: one VPC, two subnets for the load balancer in two
/// Availability Zones, a public subnet the public NAT gateway sits in and a private one the
/// private NAT gateway sits in.
const VPC_CIDR: &str = "10.91.0.0/16";
const LOAD_BALANCER_SUBNETS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-east-1a"),
    ("10.91.2.0/24", "us-east-1b"),
];
const PUBLIC_NAT_SUBNET_CIDR: &str = "10.91.3.0/24";
const PRIVATE_NAT_SUBNET_CIDR: &str = "10.91.4.0/24";

/// The Availability Zone every row's own subnet is created in.
const ZONE: &str = "us-east-1a";

/// The destination of the route that carries traffic OFF a VPC: "a route table contains a set
/// of rules, called routes" and the one for everything outside the VPC is the default route
/// (Amazon VPC User Guide, "Route tables").
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// An image in a PUBLIC container registry, whose registry is no AWS service and so is
/// reachable only over the internet, with the registry its reference names.
const NAMED_IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const NAMED_REGISTRY: &str = "public.ecr.aws";

/// An image reference that names no registry at all, which a container runtime resolves
/// against Docker Hub — the row that proves the reason names the registry the reference
/// RESOLVES to rather than its text.
const HUB_IMAGE: &str = "nginx:1.27";
const HUB_REGISTRY: &str = "registry-1.docker.io";

/// `Task$lastStatus` of a task that is running, and of one that has stopped.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `AwsVpcConfiguration$assignPublicIp` as each row names it.
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

/// Where a row's route table sends [`DEFAULT_ROUTE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Egress {
    /// An internet gateway attached to the VPC.
    Internet,
    /// A NAT gateway created with an Elastic IP address.
    PublicNat,
    /// A NAT gateway created with none, which carries no internet-bound traffic.
    PrivateNat,
}

/// One row: the network the service's tasks are placed in, the image they run, and whether
/// that placement can fetch it.
struct Row {
    case: &'static str,
    /// The name every resource of the row is built from: its subnet's route table, its target
    /// group, its revision's family and its service.
    name: &'static str,
    /// The row's own /24 inside [`VPC_CIDR`].
    cidr: &'static str,
    /// The listener port that makes the row's target group one the load balancer forwards to.
    listener_port: &'static str,
    /// Where the row's subnet sends traffic leaving the VPC, or `None` for a subnet with no
    /// route off it.
    egress: Option<Egress>,
    /// `AwsVpcConfiguration$assignPublicIp` of the service's placement.
    assign_public_ip: &'static str,
    /// The image the row's revision runs, and the registry that reference resolves to.
    image: &'static str,
    registry: &'static str,
    /// How many tasks the scale-up asks for.
    desired: i64,
    /// Whether the placement can fetch that image, so the tasks RUN and are registered.
    fetchable: bool,
}

const ROWS: [Row; 7] = [
    Row {
        case: "the recorded configuration: an isolated subnet with no route off the VPC",
        name: "isolated",
        cidr: "10.91.11.0/24",
        listener_port: "8081",
        egress: None,
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        image: NAMED_IMAGE,
        registry: NAMED_REGISTRY,
        desired: 1,
        fetchable: false,
    },
    Row {
        case: "an isolated subnet, where the task asked for a public address anyway",
        name: "isolated-public-ip",
        cidr: "10.91.12.0/24",
        listener_port: "8082",
        egress: None,
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        image: NAMED_IMAGE,
        registry: NAMED_REGISTRY,
        desired: 1,
        fetchable: false,
    },
    Row {
        case: "an isolated subnet and a reference that names no registry",
        name: "isolated-bare-ref",
        cidr: "10.91.13.0/24",
        listener_port: "8083",
        egress: None,
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        image: HUB_IMAGE,
        registry: HUB_REGISTRY,
        desired: 1,
        fetchable: false,
    },
    Row {
        case: "an internet gateway, for a task given no public address",
        name: "igw-no-public-ip",
        cidr: "10.91.14.0/24",
        listener_port: "8084",
        egress: Some(Egress::Internet),
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        image: NAMED_IMAGE,
        registry: NAMED_REGISTRY,
        desired: 1,
        fetchable: false,
    },
    Row {
        case: "an internet gateway and a public address, scaled up to two tasks",
        name: "igw-public-ip",
        cidr: "10.91.15.0/24",
        listener_port: "8085",
        egress: Some(Egress::Internet),
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        image: NAMED_IMAGE,
        registry: NAMED_REGISTRY,
        desired: 2,
        fetchable: true,
    },
    Row {
        case: "a private NAT gateway, which carries no internet-bound traffic",
        name: "private-nat",
        cidr: "10.91.16.0/24",
        listener_port: "8086",
        egress: Some(Egress::PrivateNat),
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        image: NAMED_IMAGE,
        registry: NAMED_REGISTRY,
        desired: 1,
        fetchable: false,
    },
    Row {
        case: "a public NAT gateway, for a task given no public address",
        name: "public-nat",
        cidr: "10.91.17.0/24",
        listener_port: "8087",
        egress: Some(Egress::PublicNat),
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        image: HUB_IMAGE,
        registry: HUB_REGISTRY,
        desired: 1,
        fetchable: true,
    },
];

impl Row {
    /// The task-definition family the row's revision is registered under.
    fn family(&self) -> String {
        format!("ecsnt-{}", self.name)
    }

    /// The target group the row's service binds.
    fn group_name(&self) -> String {
        format!("ecsnt-{}", self.name)
    }

    /// The service the row scales up.
    fn service_name(&self) -> String {
        format!("ecsnt-{}", self.name)
    }

    /// How many registrations the row's group holds once its service has settled.
    fn registrations(&self) -> usize {
        if self.fetchable {
            usize::try_from(self.desired).unwrap_or_default()
        } else {
            0
        }
    }

    /// The WHOLE `stoppedReason` a task of this row must carry when its image cannot be
    /// fetched: the container runtime's error class, the reference the revision named and the
    /// registry that reference resolves to, in the condition a placement with no path to it
    /// produces. Spelled out here rather than imported, so a wrong constant in the
    /// implementation cannot make this test pass.
    fn expected_reason(&self) -> String {
        format!(
            "CannotPullContainerError: failed to resolve ref {}: failed to do request: \
             Get \"https://{}/v2/\": dial tcp {}:443: i/o timeout",
            self.image, self.registry, self.registry
        )
    }
}

/// One registration as `DescribeTargetHealth` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    /// `TargetDescription$Id`: the address the load balancer reaches the target at.
    id: String,
    /// `TargetDescription$Port`: the port it receives traffic on.
    port: String,
}

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
    assert_eq!(
        status, 200,
        "ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every row's resources are created in, every id of it minted by Amazon EC2.
struct Network {
    vpc: String,
    internet_gateway: String,
    load_balancer_subnets: Vec<String>,
    public_nat_gateway: String,
    private_nat_gateway: String,
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

/// Create a route table of this suite's VPC, associate it with `subnet_id`, and send
/// [`DEFAULT_ROUTE`] where `egress` says — or nowhere, for a subnet with no route off the VPC.
///
/// Answers the table's id, which the caller reads the stored routes back out of.
async fn route_table(
    router: &MultiServiceRouter,
    network: &Network,
    subnet_id: &str,
    egress: Option<Egress>,
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
    let target: Option<(&str, &str)> = match egress {
        None => None,
        Some(Egress::Internet) => Some(("GatewayId", network.internet_gateway.as_str())),
        Some(Egress::PublicNat) => Some(("NatGatewayId", network.public_nat_gateway.as_str())),
        Some(Egress::PrivateNat) => Some(("NatGatewayId", network.private_nat_gateway.as_str())),
    };
    if let Some((parameter, value)) = target {
        ec2(
            router,
            "CreateRoute",
            &[
                ("RouteTableId", table.as_str()),
                ("DestinationCidrBlock", DEFAULT_ROUTE),
                (parameter, value),
            ],
        )
        .await;
    }
    table
}

/// Create the network every row runs in: the VPC, an attached internet gateway, the load
/// balancer's two subnets, the public subnet the public NAT gateway sits in, the private
/// subnet the private one sits in, and the security group every placement names.
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
                ("GroupName", "ecsnt-group"),
                ("GroupDescription", "the group this suites resources are in"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );

    let mut network = Network {
        vpc,
        internet_gateway,
        load_balancer_subnets,
        public_nat_gateway: String::new(),
        private_nat_gateway: String::new(),
        security_group,
    };

    // A public NAT gateway reaches the internet through an Elastic IP address — "[Public NAT
    // gateways only] ... You cannot specify an Elastic IP address with a private NAT gateway"
    // (`CreateNatGatewayRequest$AllocationId`) — and sits in a subnet that routes to the
    // internet gateway, while the private one sits in a subnet that routes nowhere.
    let public_nat_subnet = subnet(router, &network.vpc, PUBLIC_NAT_SUBNET_CIDR, ZONE).await;
    route_table(
        router,
        &network,
        &public_nat_subnet,
        Some(Egress::Internet),
    )
    .await;
    let allocation = element(
        &ec2(router, "AllocateAddress", &[("Domain", "vpc")]).await,
        "allocationId",
    );
    network.public_nat_gateway = element(
        &ec2(
            router,
            "CreateNatGateway",
            &[
                ("SubnetId", public_nat_subnet.as_str()),
                ("ConnectivityType", "public"),
                ("AllocationId", allocation.as_str()),
            ],
        )
        .await,
        "natGatewayId",
    );
    let private_nat_subnet = subnet(router, &network.vpc, PRIVATE_NAT_SUBNET_CIDR, ZONE).await;
    network.private_nat_gateway = element(
        &ec2(
            router,
            "CreateNatGateway",
            &[
                ("SubnetId", private_nat_subnet.as_str()),
                ("ConnectivityType", "private"),
            ],
        )
        .await,
        "natGatewayId",
    );
    network
}

/// Register the revision one row's service runs: an `awsvpc`, Fargate-compatible revision
/// whose one container runs the row's image and listens on the port every binding names.
async fn seed_revision(router: &MultiServiceRouter, row: &Row) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family(),
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": row.image,
                "memory": 128,
                "portMappings": [{ "containerPort": CONTAINER_PORT }],
            }],
        }),
    )
    .await;
}

/// Create one row's `ip` target group — the target type a service whose tasks use the `awsvpc`
/// network mode must bind ("you must choose `ip` ... because tasks that use the `awsvpc`
/// network mode are associated with an elastic network interface",
/// `LoadBalancer$targetGroupArn`) — and answer its ARN.
async fn target_group(router: &MultiServiceRouter, network: &Network, row: &Row) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", row.group_name().as_str()),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
        ],
    )
    .await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {:?}: no TargetGroupArn in {created}", row.case))
}

/// Every registration `arn` reports.
async fn health(router: &MultiServiceRouter, arn: &str) -> Vec<Reported> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body)
        .into_iter()
        .map(|description| {
            let text = |tag: &str| {
                elements(description, tag)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            Reported {
                id: text("Id"),
                port: text("Port"),
            }
        })
        .collect()
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

/// One task of one row's service, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, arn: &str) -> Value {
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
    )
    .await;
    described["tasks"][0].clone()
}

/// How many tasks of one row's service are RUNNING, as `DescribeServices` reports it.
async fn running_count(router: &MultiServiceRouter, row: &Row) -> i64 {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name()] }),
    )
    .await;
    described["services"][0]["runningCount"]
        .as_i64()
        .unwrap_or_default()
}

/// The private IPv4 address of one task's own elastic network interface, as `DescribeTasks`
/// reports it among the task's attachment details.
fn interface_address(task: &Value) -> Option<String> {
    task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|attachment| attachment["details"].as_array())
        .flatten()
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str())
        .map(str::to_string)
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
async fn scaling_a_service_whose_placement_cannot_reach_the_registry_registers_no_target() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "ecsnt-alb"),
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
    let mut groups: Vec<String> = Vec::new();
    for row in &ROWS {
        let subnet_id = subnet(&router, &network.vpc, row.cidr, ZONE).await;
        let table = route_table(&router, &network, &subnet_id, row.egress).await;
        // The precondition every assertion below rests on, read back out of Amazon EC2: the
        // table in force on the placement's subnet carries a default route exactly when the
        // row says it does.
        let tables = ec2(
            &router,
            "DescribeRouteTables",
            &[("RouteTableId.1", table.as_str())],
        )
        .await;
        assert_eq!(
            elements(&tables, "destinationCidrBlock").contains(&DEFAULT_ROUTE.to_string()),
            row.egress.is_some(),
            "case {:?}: expected a default route {}, got {tables}",
            row.case,
            if row.egress.is_some() {
                "in the subnet's table"
            } else {
                "in no table of the subnet"
            }
        );

        seed_revision(&router, row).await;
        let group = target_group(&router, &network, row).await;
        // A listener forwarding to the group is what makes it one the load balancer sends
        // traffic to, which is what a service may bind.
        elbv2(
            &router,
            "CreateListener",
            &[
                ("LoadBalancerArn", load_balancer.as_str()),
                ("Protocol", "HTTP"),
                ("Port", row.listener_port),
                ("DefaultActions.member.1.Type", "forward"),
                ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
            ],
        )
        .await;
        let created = ecs(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name(),
                "taskDefinition": row.family(),
                "desiredCount": 0,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [subnet_id],
                        "securityGroups": [network.security_group],
                        "assignPublicIp": row.assign_public_ip,
                    }
                },
                "loadBalancers": [{
                    "targetGroupArn": group,
                    "containerName": CONTAINER,
                    "containerPort": CONTAINER_PORT,
                }],
            }),
        )
        .await;
        assert_eq!(
            created["service"]["desiredCount"].as_i64(),
            Some(0),
            "case {:?}: the created service wants no tasks yet, got {created}",
            row.case
        );
        let reported = health(&router, &group).await;
        assert!(
            reported.is_empty(),
            "case {:?}: a service with no tasks registers nothing, got {reported:?}",
            row.case
        );
        groups.push(group);
    }

    // The one call the scenario is about: every row's service is scaled UP, so the clock-owed
    // placements of all of them fall due together rather than one row's budget after another's.
    for row in &ROWS {
        let updated = ecs(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service_name(),
                "desiredCount": row.desired,
            }),
        )
        .await;
        assert_eq!(
            updated["service"]["desiredCount"].as_i64(),
            Some(row.desired),
            "case {:?}: the scaled service must want {} task(s), got {updated}",
            row.case,
            row.desired
        );
    }

    for (group, row) in groups.iter().zip(&ROWS) {
        let case = row.case;
        if row.fetchable {
            // The image can be fetched from this placement, so the tasks RUN and each is a
            // target of the bound group at its own interface's address. BOTH sides are
            // settled before anything is compared — the registrations the group holds and
            // the tasks ECS reports running — so a registration observable one read before
            // its task is reported running is waited through rather than read as a
            // violation.
            let wanted = row.registrations();
            let (reported, tasks) = poll_until(
                || async {
                    let reported = health(&router, group).await;
                    if reported.len() != wanted {
                        return None;
                    }
                    let mut tasks: Vec<Value> = Vec::new();
                    for arn in task_arns(&router, row, RUNNING).await {
                        let task = describe_task(&router, &arn).await;
                        if task["lastStatus"].as_str() == Some(RUNNING) {
                            tasks.push(task);
                        }
                    }
                    (tasks.len() == wanted).then_some((reported, tasks))
                },
                &format!(
                    "case {case:?}: {wanted} running task(s) of {}, and {wanted} \
                     registration(s) in {group}",
                    row.service_name()
                ),
            )
            .await;
            let mut addresses: BTreeSet<String> = BTreeSet::new();
            for task in &tasks {
                assert_eq!(
                    task["stopCode"].as_str(),
                    None,
                    "case {case:?}: a task that started carries no stop code, got {task}"
                );
                addresses.insert(interface_address(task).unwrap_or_else(|| {
                    panic!("case {case:?}: a running awsvpc task reports its address, got {task}")
                }));
            }
            assert_eq!(
                reported
                    .iter()
                    .map(|description| description.id.clone())
                    .collect::<BTreeSet<String>>(),
                addresses,
                "case {case:?}: expected the group to hold exactly the tasks' own addresses, \
                 got {reported:?}"
            );
            for description in &reported {
                assert_eq!(
                    description.port,
                    CONTAINER_PORT.to_string(),
                    "case {case:?}: expected the registration on the bound port \
                     {CONTAINER_PORT}, got {description:?}"
                );
            }
            continue;
        }

        // The image cannot be fetched from this placement, so the task the scale-up scheduled
        // is STOPPED before it ever ran — and the group holds nothing at any instant on the
        // way there, which is the half a client cannot otherwise tell from "still coming up".
        let stopped = poll_until(
            || async {
                let reported = health(&router, group).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: no target may be registered for a task that never ran, got \
                     {reported:?}"
                );
                for arn in task_arns(&router, row, STOPPED).await {
                    let task = describe_task(&router, &arn).await;
                    // A task Amazon ECS marks for the stop a failed launch earns is on its
                    // way out like any other stopping task, so the one read here is the task
                    // that has ARRIVED at `STOPPED`.
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
            stopped["lastStatus"].as_str(),
            Some(STOPPED),
            "case {case:?}: expected the task reported stopped, got {stopped}"
        );
        assert_eq!(
            stopped["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose image could not be fetched never started, got {stopped}"
        );
        let expected = row.expected_reason();
        assert_eq!(
            stopped["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {stopped}"
        );
        // The fetch was ATTEMPTED — `Task$pullStartedAt` is "the Unix timestamp for the time
        // when the container image pull began" — and nothing ran: no container of the task
        // reports an exit code and the service counts no running task.
        assert!(
            stopped["pullStartedAt"].as_f64().is_some(),
            "case {case:?}: the stopped task reports when its image pull began, got {stopped}"
        );
        assert_eq!(
            stopped["startedAt"].as_f64(),
            None,
            "case {case:?}: a task that never ran has no startedAt, got {stopped}"
        );
        let containers = stopped["containers"].as_array().cloned().unwrap_or_default();
        assert!(
            !containers.is_empty(),
            "case {case:?}: the stopped task reports its containers, got {stopped}"
        );
        for container in containers {
            assert_eq!(
                container["exitCode"].as_i64(),
                None,
                "case {case:?}: a container that never started reports no exit code, got \
                 {container}"
            );
        }
        assert_eq!(
            running_count(&router, row).await,
            0,
            "case {case:?}: a service whose tasks cannot start runs none"
        );
        let reported = health(&router, group).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected the bound group to hold no registration, got {reported:?}"
        );
    }
}
