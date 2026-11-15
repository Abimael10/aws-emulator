// SPDX-License-Identifier: CC0-1.0

//! An ALB-fronted service placed in the account's DEFAULT network converges: its task runs,
//! the target group it binds holds that task's address, and the health check settles on a
//! verdict.
//!
//! RULE: a service placed in a default subnet of the account's default VPC needs no network
//! of the caller's own — "when you create an AWS account, we create a default VPC for you in
//! each AWS Region" with "a default subnet in each Availability Zone", an internet gateway
//! attached to it and a main route table routing internet-bound traffic to that gateway
//! (Amazon VPC User Guide, "Default VPCs") — so a task of such a service fetches its image
//! from a public registry, reaches `RUNNING`, is registered as an `ip` target of every group
//! its `loadBalancers` bind, and the service reports its deployment `COMPLETED` once it runs
//! the count it was asked for.
//!
//! Three stores have to agree for that to hold, so all three services are mounted on one
//! engine: Amazon EC2 owns the default VPC, its gateway, its route table and its subnets;
//! this service places the task, decides its image fetch against those records and registers
//! it; Elastic Load Balancing v2 holds the registration and reports its health. Everything is
//! driven through the real operations, so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: whether the placement is a default
//! subnet of the account's own default VPC or a subnet of a VPC the caller built and gave an
//! internet gateway, and whether the task is given the public address an internet gateway's
//! route carries traffic for — the last row being the one that must NOT converge, so a row
//! above it cannot pass by a path being assumed for every placement.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{count, create_cluster, ec2_element, ec2_form, list, ok, string, AWSVPC_START};
use crate::load_balancer_target_groups::{all_texts, elb_call, first_text};

/// The cluster every row's service runs in.
const CLUSTER: &str = "defaultnet";

/// The container every row's revision runs, and the port it listens on.
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// An image held in a PUBLIC container registry: its registry is no AWS service, so it is
/// reachable only over the internet.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The CIDR blocks of the VPC the caller-built row runs in, and of its two subnets.
const OWN_VPC_CIDR: &str = "10.60.0.0/16";
const OWN_SUBNET_CIDRS: [&str; 2] = ["10.60.1.0/24", "10.60.2.0/24"];

/// The destination of the route that carries traffic off a VPC.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// How often a poll re-reads the surface it waits on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll keeps trying: the whole start of an `awsvpc` task
/// ([`AWSVPC_START`]), the registration that start owes, the checks the accelerated
/// health-check schedule then runs, and for the negative row the two further spans a launch
/// failure takes — with generous margin, so a loaded machine cannot fail a row the emulator
/// would satisfy. The deadline is only here so a failure is a failure rather than a hang.
const POLL_BUDGET: Duration = AWSVPC_START.saturating_add(Duration::from_secs(30));

/// Where a row places its service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// A default subnet of the account's own default VPC, which the row creates nothing in.
    DefaultSubnet,
    /// A subnet of a VPC the row created, with an internet gateway and a default route to it.
    OwnSubnetWithAGateway,
}

/// One row: where its service is placed, the public address the placement asks for, and
/// whether the service must then converge.
struct Row {
    case: &'static str,
    /// The target group's name, the listener port and the service name this row uses.
    group_name: &'static str,
    listener_port: &'static str,
    service_name: &'static str,
    placement: Placement,
    /// `AwsVpcConfiguration$assignPublicIp` the placement declares.
    assign_public_ip: &'static str,
    /// Whether the task must RUN — so the service converges and the group holds its address.
    converges: bool,
}

const ROWS: [Row; 3] = [
    Row {
        case: "a default subnet of the account's default VPC, with a public address",
        group_name: "defaultsubnet",
        listener_port: "80",
        service_name: "defaultsubnet",
        placement: Placement::DefaultSubnet,
        assign_public_ip: "ENABLED",
        converges: true,
    },
    Row {
        case: "a subnet of a VPC the caller built and gave an internet gateway",
        group_name: "ownsubnet",
        listener_port: "8081",
        service_name: "ownsubnet",
        placement: Placement::OwnSubnetWithAGateway,
        assign_public_ip: "ENABLED",
        converges: true,
    },
    Row {
        case: "a default subnet, for a task given no public address",
        group_name: "nopublicip",
        listener_port: "8082",
        service_name: "nopublicip",
        placement: Placement::DefaultSubnet,
        assign_public_ip: "DISABLED",
        converges: false,
    },
];

/// A router carrying this service, Amazon EC2 and Elastic Load Balancing v2 over ONE shared
/// engine — the three stores this rule reads across, mounted the way the production router
/// mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One ELBv2 call that must succeed, as the body it answered.
async fn elb_ok(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
    case: &str,
) -> String {
    let (status, body) = elb_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {action} to answer 200, got {status} with {body}"
    );
    body
}

/// The network one row places its service in: the VPC, its two subnets, and the two security
/// groups — the load balancer's own, and the task's, which admits the checked port from it.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    load_balancer_group: String,
    task_group: String,
}

/// The account's DEFAULT VPC and two of its default subnets, as the account's own records
/// report them. Nothing is created here.
async fn default_network(router: &MultiServiceRouter, case: &str) -> (String, Vec<String>) {
    let vpcs = ec2_form(
        router,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
        case,
    )
    .await;
    let vpc = ec2_element(&vpcs, "vpcId", case);
    let subnets = ec2_form(
        router,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", &vpc),
            ("Filter.2.Name", "default-for-az"),
            ("Filter.2.Value.1", "true"),
        ],
        case,
    )
    .await;
    let ids: Vec<String> = all_texts(&subnets, "subnetId").into_iter().take(2).collect();
    assert_eq!(
        ids.len(),
        2,
        "case {case:?}: the default VPC holds a default subnet per zone, got {subnets}"
    );
    (vpc, ids)
}

/// A VPC of the caller's own with two subnets in two zones, an internet gateway attached to
/// it and a default route to that gateway in its main route table — every one created through
/// the operations a caller calls.
async fn own_network(router: &MultiServiceRouter, case: &str) -> (String, Vec<String>) {
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", OWN_VPC_CIDR)], case).await,
        "vpcId",
        case,
    );
    let mut subnets = Vec::new();
    for (index, cidr) in OWN_SUBNET_CIDRS.iter().enumerate() {
        let zone = format!("{}{}", crate::ecs_wire::SIGNED_REGION, ["a", "b"][index]);
        subnets.push(ec2_element(
            &ec2_form(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", &vpc),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", &zone),
                ],
                case,
            )
            .await,
            "subnetId",
            case,
        ));
    }
    let gateway = ec2_element(
        &ec2_form(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2_form(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway), ("VpcId", &vpc)],
        case,
    )
    .await;
    let table = ec2_element(
        &ec2_form(
            router,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
            case,
        )
        .await,
        "routeTableId",
        case,
    );
    ec2_form(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", &table),
            ("DestinationCidrBlock", INTERNET_DESTINATION),
            ("GatewayId", &gateway),
        ],
        case,
    )
    .await;
    (vpc, subnets)
}

/// One security group of `vpc`, as its id.
async fn security_group(
    router: &MultiServiceRouter,
    vpc: &str,
    name: &str,
    description: &str,
    case: &str,
) -> String {
    ec2_element(
        &ec2_form(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", name),
                ("GroupDescription", description),
                ("VpcId", vpc),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    )
}

/// The whole network one row runs in, with the inbound rule that lets the load balancer's
/// health check reach the task: "a security group controls the traffic that is allowed to
/// reach ... the resources that it is associated with" (Amazon VPC User Guide).
async fn network_of(router: &MultiServiceRouter, row: &Row) -> Network {
    let case = row.case;
    let (vpc, subnets) = match row.placement {
        Placement::DefaultSubnet => default_network(router, case).await,
        Placement::OwnSubnetWithAGateway => own_network(router, case).await,
    };
    let load_balancer_group = security_group(
        router,
        &vpc,
        &format!("{}-alb", row.group_name),
        "the load balancers own group",
        case,
    )
    .await;
    let task_group = security_group(
        router,
        &vpc,
        &format!("{}-task", row.group_name),
        "the tasks own group",
        case,
    )
    .await;
    ec2_form(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", &task_group),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", "80"),
            ("IpPermissions.1.ToPort", "80"),
            ("IpPermissions.1.Groups.1.GroupId", &load_balancer_group),
        ],
        case,
    )
    .await;
    Network {
        vpc,
        subnets,
        load_balancer_group,
        task_group,
    }
}

/// The `ip` target group one row binds, with a load balancer in its network forwarding to it
/// — a group nothing forwards to is a group whose targets nothing checks.
async fn load_balanced_target_group(
    router: &MultiServiceRouter,
    row: &Row,
    network: &Network,
) -> String {
    let case = row.case;
    let created = elb_ok(
        router,
        "CreateTargetGroup",
        &[
            ("Name", row.group_name),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("TargetType", "ip"),
            ("VpcId", &network.vpc),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
        ],
        case,
    )
    .await;
    let group = first_text(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn in {created}"));
    let load_balancer = first_text(
        &elb_ok(
            router,
            "CreateLoadBalancer",
            &[
                ("Name", row.group_name),
                ("Subnets.member.1", &network.subnets[0]),
                ("Subnets.member.2", &network.subnets[1]),
                ("SecurityGroups.member.1", &network.load_balancer_group),
            ],
            case,
        )
        .await,
        "LoadBalancerArn",
    )
    .unwrap_or_else(|| panic!("case {case:?}: the load balancer must be created"));
    elb_ok(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", &load_balancer),
            ("Protocol", "HTTP"),
            ("Port", row.listener_port),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", &group),
        ],
        case,
    )
    .await;
    group
}

/// Register the revision every row's service runs: an `awsvpc`, Fargate-compatible revision
/// whose one container listens on [`CONTAINER_PORT`] and runs an image held in a public
/// registry.
async fn register_revision(router: &MultiServiceRouter, family: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "portMappings": [{ "containerPort": CONTAINER_PORT }],
            }],
        }),
        case,
    )
    .await;
}

/// Create one row's service, bound to `group`.
async fn create_service(
    router: &MultiServiceRouter,
    row: &Row,
    network: &Network,
    family: &str,
    group: &str,
) {
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "taskDefinition": family,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0]],
                    "securityGroups": [network.task_group],
                    "assignPublicIp": row.assign_public_ip,
                }
            },
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        }),
        row.case,
    )
    .await;
}

/// One row's service as `DescribeServices` reports it now.
async fn described_service(router: &MultiServiceRouter, row: &Row) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name] }),
        row.case,
    )
    .await;
    list(&body, "services", row.case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {:?}: the service must read back", row.case))
}

/// `(target id, state)` for every registration one group reports.
async fn health(router: &MultiServiceRouter, group: &str, case: &str) -> Vec<(String, String)> {
    let body = elb_ok(
        router,
        "DescribeTargetHealth",
        &[("TargetGroupArn", group)],
        case,
    )
    .await;
    let ids = all_texts(&body, "Id");
    let states = all_texts(&body, "State");
    ids.into_iter().zip(states).collect()
}

/// The private IPv4 address of the one RUNNING task of one row's service.
async fn running_task_address(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let listed = ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": row.service_name }),
        case,
    )
    .await;
    let arns: Vec<Value> = list(&listed, "taskArns", case);
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    let task = list(&described, "tasks", case)
        .into_iter()
        .find(|task| task["lastStatus"].as_str() == Some("RUNNING"))
        .unwrap_or_else(|| panic!("case {case:?}: the service must run a task, got {described}"));
    list(&task, "attachments", case)
        .iter()
        .flat_map(|attachment| list(attachment, "details", case))
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str().map(str::to_string))
        .unwrap_or_else(|| panic!("case {case:?}: the task carries its interface's address"))
}

#[tokio::test]
async fn an_alb_fronted_service_in_the_default_network_converges_to_a_healthy_target() {
    let router = emulator();
    create_cluster(&router, CLUSTER, "the cluster").await;

    let mut built: Vec<(&Row, String)> = Vec::new();
    for row in &ROWS {
        let network = network_of(&router, row).await;
        let family = format!("{}family", row.group_name);
        register_revision(&router, &family, row.case).await;
        let group = load_balanced_target_group(&router, row, &network).await;
        create_service(&router, row, &network, &family, &group).await;
        built.push((row, group));
    }

    for (row, group) in &built {
        let case = row.case;
        let deadline = Instant::now() + POLL_BUDGET;
        loop {
            let service = described_service(&router, row).await;
            let running = count(&service, "runningCount", case);
            let primary = list(&service, "deployments", case)
                .into_iter()
                .find(|deployment| deployment["status"].as_str() == Some("PRIMARY"));
            let rollout = primary
                .as_ref()
                .and_then(|deployment| deployment["rolloutState"].as_str())
                .unwrap_or_default()
                .to_string();
            let reported = health(&router, group, case).await;
            if row.converges {
                let settled = running == 1
                    && rollout == "COMPLETED"
                    && reported.len() == 1
                    && reported[0].1 == "healthy";
                if settled {
                    let address = running_task_address(&router, row).await;
                    assert_eq!(
                        reported[0].0, address,
                        "case {case:?}: the healthy target is the task's own address, got \
                         {reported:?}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the service to run its task and the group to \
                     hold it healthy, got runningCount {running}, rolloutState {rollout:?} \
                     and the targets {reported:?} with the service {service}"
                );
            } else {
                // The negative row: no path to the registry, so no container ever runs and
                // nothing is ever a healthy target of the group.
                assert!(
                    !reported.iter().any(|(_, state)| state == "healthy"),
                    "case {case:?}: a task that never fetched its image is no healthy \
                     target, got {reported:?}"
                );
                let stopped = ok(
                    &router,
                    "ListTasks",
                    json!({
                        "cluster": CLUSTER,
                        "serviceName": row.service_name,
                        "desiredStatus": "STOPPED",
                    }),
                    case,
                )
                .await;
                if !list(&stopped, "taskArns", case).is_empty() {
                    let described = ok(
                        &router,
                        "DescribeTasks",
                        json!({
                            "cluster": CLUSTER,
                            "tasks": list(&stopped, "taskArns", case),
                        }),
                        case,
                    )
                    .await;
                    let failed = list(&described, "tasks", case).into_iter().find(|task| {
                        task["stopCode"].as_str() == Some("TaskFailedToStart")
                    });
                    if let Some(task) = failed {
                        let reason = string(&task, "stoppedReason", case);
                        assert!(
                            reason.contains("CannotPullContainerError"),
                            "case {case:?}: expected the runtime's failed-fetch reason, got \
                             {reason:?}"
                        );
                        break;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the task to be stopped for a failed image \
                     fetch, got {stopped}"
                );
            }
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
