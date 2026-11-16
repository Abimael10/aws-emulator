// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an `awsvpc` task's elastic network interface takes, for a task the SERVICE
//! scheduler placed, driven end to end over the wire.
//!
//! RULE: an elastic network interface is created IN the subnet the placement named and is
//! reachable at an address OF that subnet — `AwsVpcConfiguration$subnets` ("The IDs of the
//! subnets associated with the task or service") names it, `Subnet$cidrBlock` is "the IPv4
//! CIDR block assigned to the subnet", and AWS reserves the first four addresses and the
//! broadcast address of every subnet (Amazon VPC User Guide, "Subnet CIDR blocks"). So the
//! `privateIPv4Address` an attachment reports lies inside the CIDR block Amazon VPC holds for
//! the subnet, whichever call placed the task: `RunTask`, `StartTask` or the SCHEDULER of a
//! service, which places tasks with no call of the caller's there to resolve Amazon VPC for
//! it.
//!
//! Two stores have to agree for that to hold, so both services are mounted on one engine:
//! Amazon EC2 owns the VPC, the subnet and its CIDR block, and this service places the task
//! and reports the interface it allocated. Everything is driven through the real operations,
//! so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: the CIDR block of the subnet the
//! placement names — its network and its prefix length, from a wide `/22` to a `/28` holding
//! eleven assignable addresses — and which call places the task (a service's scheduler, and
//! `RunTask` beside it). One row's subnet is carved out of `172.31.0.0/16`, the block AWS
//! gives a default VPC, at an offset that block's own first addresses do not cover: a report
//! that took the address from that block rather than from the subnet passes no row, and that
//! row least of all.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok, AWSVPC_START,
    SIGNED_REGION,
};

/// The cluster every row's tasks run in.
const CLUSTER: &str = "subnetaddresses";

/// The container every row's revision runs. Held in a PUBLIC registry, reached over the
/// internet gateway each row's VPC carries, so the task's image fetch succeeds and the task
/// reaches `RUNNING` — which is when the interface's own details are reported.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The destination of the route that carries a VPC's traffic to its internet gateway.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// The addresses AWS reserves at the START of every subnet's CIDR block, which no network
/// interface may be given (Amazon VPC User Guide, "Subnet CIDR blocks").
const RESERVED_AT_START: u32 = 4;

/// How often a poll re-reads the surface it waits on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll keeps trying: the whole start of an `awsvpc` task ([`AWSVPC_START`]) with
/// generous margin, so a loaded machine cannot fail a row the emulator would satisfy. The
/// deadline is only here so a failure is a failure rather than a hang.
const POLL_BUDGET: Duration = AWSVPC_START.saturating_add(Duration::from_secs(30));

/// One row: the network it builds and the name every resource of it carries.
struct Row {
    case: &'static str,
    /// The name the row's VPC resources, revision and service are named after.
    label: &'static str,
    /// The CIDR block of the row's VPC, which holds its subnet's.
    vpc_cidr: &'static str,
    /// `Subnet$cidrBlock` of the subnet the placement names.
    subnet_cidr: &'static str,
}

const ROWS: [Row; 4] = [
    Row {
        case: "a /24 subnet",
        label: "slashtwentyfour",
        vpc_cidr: "10.111.0.0/16",
        subnet_cidr: "10.111.1.0/24",
    },
    Row {
        case: "a /28 subnet, the smallest a subnet may be",
        label: "slashtwentyeight",
        vpc_cidr: "172.20.0.0/16",
        subnet_cidr: "172.20.5.16/28",
    },
    Row {
        case: "a /22 subnet, wider than its VPC's other blocks",
        label: "slashtwentytwo",
        vpc_cidr: "192.168.0.0/16",
        subnet_cidr: "192.168.40.0/22",
    },
    Row {
        case: "a subnet carved out of the block a default VPC is given",
        label: "defaultblock",
        vpc_cidr: "172.31.0.0/16",
        subnet_cidr: "172.31.9.0/24",
    },
];

/// The network one row places into: a VPC of its own CIDR with one subnet, an internet
/// gateway attached to it and a default route to that gateway in its main route table —
/// every one created through the operations a caller calls.
struct Network {
    subnet: String,
    security_group: String,
}

async fn network_of(router: &MultiServiceRouter, row: &Row) -> Network {
    let case = row.case;
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", row.vpc_cidr)], case).await,
        "vpcId",
        case,
    );
    let subnet = ec2_element(
        &ec2_form(
            router,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", row.subnet_cidr),
                ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );
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
    let security_group = ec2_element(
        &ec2_form(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("{}-task", row.label)),
                ("GroupDescription", "the tasks own group"),
                ("VpcId", &vpc),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    );
    Network {
        subnet,
        security_group,
    }
}

/// Register the revision every row's tasks run: an `awsvpc`, Fargate-compatible revision
/// whose one container runs an image held in a public registry.
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
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "portMappings": [{ "containerPort": 80 }],
            }],
        }),
        case,
    )
    .await;
}

/// The `awsvpcConfiguration` a placement into `network` names.
fn placement(network: &Network) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": [network.subnet],
            "securityGroups": [network.security_group],
            "assignPublicIp": "ENABLED",
        }
    })
}

/// The `(subnetId, privateIPv4Address)` the one RUNNING task listed by `request` reports, or
/// `None` while no task of it is running yet or carries its interface's details.
async fn running_interface(
    router: &MultiServiceRouter,
    request: Value,
    case: &str,
) -> Option<(String, String)> {
    let listed = ok(router, "ListTasks", request, case).await;
    let arns: Vec<Value> = list(&listed, "taskArns", case);
    if arns.is_empty() {
        return None;
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    let task = list(&described, "tasks", case)
        .into_iter()
        .find(|task| task["lastStatus"].as_str() == Some("RUNNING"))?;
    let details: Vec<Value> = list(&task, "attachments", case)
        .iter()
        .flat_map(|attachment| list(attachment, "details", case))
        .collect();
    let detail = |name: &str| {
        details
            .iter()
            .find(|detail| detail["name"].as_str() == Some(name))
            .and_then(|detail| detail["value"].as_str().map(str::to_string))
    };
    Some((detail("subnetId")?, detail("privateIPv4Address")?))
}

/// Look again until the task the placement owes reports the interface it was allocated.
async fn settled_interface(
    router: &MultiServiceRouter,
    request: Value,
    case: &str,
) -> (String, String) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        if let Some(reported) = running_interface(router, request.clone(), case).await {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a RUNNING task reporting its interface's subnet and \
             address for {request}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The network address and the prefix length `cidr` names.
fn parsed_cidr(cidr: &str) -> (u32, u8) {
    let (network, prefix) = cidr
        .split_once('/')
        .unwrap_or_else(|| panic!("the CIDR block {cidr:?} names a prefix length"));
    let network: Ipv4Addr = network
        .parse()
        .unwrap_or_else(|error| panic!("the CIDR block {cidr:?} names an address: {error}"));
    let prefix: u8 = prefix
        .parse()
        .unwrap_or_else(|error| panic!("the CIDR block {cidr:?} names a prefix length: {error}"));
    (u32::from(network), prefix)
}

/// Assert that `address` is one a network interface of the subnet `cidr` may hold: inside the
/// block, past the four addresses AWS reserves at its start, and below its broadcast address.
fn assert_assignable_in(case: &str, cidr: &str, address: &str) {
    let (network, prefix) = parsed_cidr(cidr);
    let held: u64 = 1u64 << (u32::BITS - u32::from(prefix));
    let parsed: Ipv4Addr = address
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {address:?} must be an IPv4 address: {error}"));
    let offset = u64::from(u32::from(parsed).wrapping_sub(network));
    assert!(
        offset < held,
        "case {case:?}: expected an address inside the subnet block {cidr}, got {address}"
    );
    assert!(
        offset >= u64::from(RESERVED_AT_START),
        "case {case:?}: expected an address past the four AWS reserves in {cidr}, got {address}"
    );
    assert!(
        offset < held - 1,
        "case {case:?}: expected an address below the broadcast address of {cidr}, got {address}"
    );
}

#[tokio::test]
async fn a_service_placed_task_takes_its_address_out_of_its_own_subnet() {
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, "the cluster").await;

    // Everything built first, so the rows' tasks start together and the waits below overlap
    // rather than running one after the other.
    let mut built: Vec<(&Row, Network, String)> = Vec::new();
    for row in &ROWS {
        let network = network_of(&router, row).await;
        let family = format!("{}family", row.label);
        register_revision(&router, &family, row.case).await;
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.label,
                "taskDefinition": family,
                "desiredCount": 1,
                "launchType": "FARGATE",
                "networkConfiguration": placement(&network),
            }),
            row.case,
        )
        .await;
        // The same placement made by a call of the caller's own, which is the other way a
        // task reaches a subnet: the rule is about the subnet, not about who placed it.
        ok(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": family,
                "count": 1,
                "launchType": "FARGATE",
                "networkConfiguration": placement(&network),
                "startedBy": format!("{}-runtask", row.label),
            }),
            row.case,
        )
        .await;
        built.push((row, network, family));
    }

    let mut addresses: Vec<String> = Vec::new();
    for (row, network, _family) in &built {
        for (placer, request) in [
            (
                "the service scheduler",
                json!({ "cluster": CLUSTER, "serviceName": row.label }),
            ),
            (
                "RunTask",
                json!({
                    "cluster": CLUSTER,
                    "startedBy": format!("{}-runtask", row.label),
                }),
            ),
        ] {
            let case = format!("{}, placed by {placer}", row.case);
            let (subnet, address) = settled_interface(&router, request, &case).await;
            assert_eq!(
                subnet, network.subnet,
                "case {case:?}: expected the interface to be created in the subnet the \
                 placement named, got {subnet:?}"
            );
            assert_assignable_in(&case, row.subnet_cidr, &address);
            assert!(
                !addresses.contains(&address),
                "case {case:?}: expected an address no other interface holds, got {address} \
                 among {addresses:?}"
            );
            addresses.push(address);
        }
    }
}

#[tokio::test]
async fn an_update_that_moves_a_service_moves_its_tasks_address_too() {
    // `UpdateService$networkConfiguration` moves where the service places: a task of the new
    // deployment is created in the subnet the update named, so its address comes out of THAT
    // subnet's block. The two subnets are in different blocks, so an address of the first
    // cannot pass for one of the second.
    let router = emulator_with_ec2();
    let case = "a service moved into another subnet";
    create_cluster(&router, CLUSTER, case).await;
    let moved_from = Row {
        case,
        label: "movedfrom",
        vpc_cidr: "10.90.0.0/16",
        subnet_cidr: "10.90.1.0/24",
    };
    let moved_to = Row {
        case,
        label: "movedto",
        vpc_cidr: "10.91.0.0/16",
        subnet_cidr: "10.91.7.0/24",
    };
    let first = network_of(&router, &moved_from).await;
    let second = network_of(&router, &moved_to).await;
    let family = "movedfamily";
    register_revision(&router, family, case).await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "moved",
            "taskDefinition": family,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": placement(&first),
        }),
        case,
    )
    .await;
    let request = json!({ "cluster": CLUSTER, "serviceName": "moved" });
    let (subnet, address) = settled_interface(&router, request.clone(), case).await;
    assert_eq!(
        subnet, first.subnet,
        "case {case:?}: expected the first placement's subnet, got {subnet:?}"
    );
    assert_assignable_in(case, moved_from.subnet_cidr, &address);

    ok(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": "moved",
            "networkConfiguration": placement(&second),
            "forceNewDeployment": true,
        }),
        case,
    )
    .await;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let reported = running_interface(&router, request.clone(), case).await;
        if let Some((subnet, address)) = reported
            .as_ref()
            .filter(|(subnet, _)| *subnet == second.subnet)
        {
            assert_assignable_in(case, moved_to.subnet_cidr, address);
            assert_eq!(
                *subnet, second.subnet,
                "case {case:?}: expected the subnet the update named"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a RUNNING task in the subnet the update named, last \
             read {reported:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}
