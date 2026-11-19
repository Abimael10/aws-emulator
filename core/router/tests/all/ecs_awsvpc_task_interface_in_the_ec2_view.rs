// SPDX-License-Identifier: CC0-1.0

//! The elastic network interface an `awsvpc` task runs on is ONE resource, and Amazon EC2 is
//! the service that holds it.
//!
//! RULE: the interface such a task is allocated
//! (`RegisterTaskDefinition$networkMode`: "If the network mode is `awsvpc`, the task is
//! allocated an elastic network interface") is an Amazon VPC resource created in the subnet
//! the launch named, so the `networkInterfaceId` Amazon ECS reports in the task's
//! `ElasticNetworkInterface` attachment names an interface Amazon EC2's own
//! `DescribeNetworkInterfaces` returns, and the two views of that one interface agree member
//! for member: `PrivateIpAddress` is the attachment's `privateIPv4Address`, `SubnetId` its
//! `subnetId` (the subnet the launch named), `MacAddress` its `macAddress`, `VpcId` the VPC
//! that subnet belongs to, `Status` is `in-use` with `Attachment.Status` `attached`, `Groups`
//! is exactly the security-group set the launch gave it (the VPC's own default group for a
//! launch that named none, which is the group such a launch runs under), `RequesterManaged`
//! is true ("Indicates whether the network interface is being managed by Amazon Web
//! Services") and `InterfaceType` is `interface`. Because the interface exists and holds one
//! of the subnet's addresses, that subnet's `AvailableIpAddressCount` is lower by exactly one
//! per running task while the tasks run, `DeleteNetworkInterface` on it is refused — HTTP 400
//! `InvalidParameterValue`, "Network interface '<id>' is currently in use." — while the same
//! call on an interface the account itself created in that subnet succeeds, and once the task
//! has STOPPED the interface is gone from Amazon EC2 and its address is back in the subnet.
//!
//! The network the tasks ran in can then be TORN DOWN: `DetachInternetGateway` and
//! `DeleteInternetGateway` (neither of which models an error) answer 200 once the gateway has
//! no dependencies, and `DeleteVpc` then succeeds and the VPC stops being describable —
//! `DependencyViolation` being the answer only for a VPC that still holds something, which an
//! emptied one does not.
//!
//! This suite lives beside the router because the rule spans two services: Amazon ECS places
//! the task and reports the attachment, Amazon EC2 owns the interface, the address pool and
//! the VPC, and both are mounted on ONE shared engine here, exactly as the production router
//! mounts them. Every id asserted on is one of the two services minted.
//!
//! The rows differ along what the rule generalises over: how many security groups the launch
//! names (one, two, none at all), whether it asks for a public address (`DISABLED` named,
//! `ENABLED`, the member left out), how many tasks one launch places (one, two) and the size
//! and zone of the subnet the interface is created in (a `/24` and a `/28`, in two zones).
//! The network gives every subnet a path to the registry the containers' image is held in —
//! the internet gateway for the subnet whose tasks get a public address, a public NAT gateway
//! for the others — so a row's `assignPublicIp` varies the interface under test rather than
//! whether the task's containers can be fetched at all.
//!
//! Sources: `smithy_query(query="doc", shape="Attachment", member="details")` and
//! `member="status"` on the ecs model; `smithy_query(query="shape",
//! shape="NetworkInterface")` on the ec2 model for the members joined below and
//! `smithy_query(query="operation", operation="DetachInternetGateway,DeleteInternetGateway")`
//! for the two gateway calls that model no error.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_contains, block_of, element, elements, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of Amazon ECS's `awsJson1_1` operations.
const TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The cluster and the Fargate-capable `awsvpc` family every launch below runs.
const CLUSTER: &str = "awsvpctaskeni";
const FAMILY: &str = "awsvpctaskenifamily";

/// The image the one container starts from; irrelevant to this rule, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `NetworkInterface$Status` of an interface something holds, and `Attachment$Status` of the
/// attachment that holds it (`smithy_query(query="shape", shape="NetworkInterface")`).
const INTERFACE_STATUS_IN_USE: &str = "in-use";
const ATTACHMENT_STATUS_ATTACHED: &str = "attached";

/// `NetworkInterface$InterfaceType` of an ordinary elastic network interface, as opposed to
/// the `efa`, `trunk` and `branch` kinds the same member reports.
const INTERFACE_TYPE: &str = "interface";

/// The refusal class Amazon EC2 answers for an action on an interface something still holds,
/// and the code its envelope carries.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The code a `DescribeNetworkInterfaces` for an id the account holds no interface for
/// answers.
const INTERFACE_NOT_FOUND: &str = "InvalidNetworkInterfaceID.NotFound";

/// How often a poll looks again at the condition it is waiting for, and how long it keeps
/// looking. Amazon ECS creates the task's interface and attaches it one documented step at a
/// time, so what is waited for is the CONDITION, with generous margin for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// One emulator carrying Amazon EC2 and Amazon ECS over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action, as its HTTP status and its XML body.
async fn ec2_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One ec2Query action that must be served, as its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "EC2 {action} must answer 200, got {status} with {body}"
    );
    body
}

/// One Amazon ECS call over its `awsJson1_1` wire, as its HTTP status and parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that must be served, as its body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ECS {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One subnet of the network, with the block it was created with: the block the interface's
/// address has to come out of.
struct Subnet {
    id: String,
    cidr_block: String,
}

/// The network every launch below places into, as Amazon EC2 holds it.
struct Network {
    vpc: String,
    internet_gateway: String,
    /// The public NAT gateway the subnets with no public address reach their registry
    /// through, which is what lets a row vary `assignPublicIp` without varying whether the
    /// task's containers can be fetched at all.
    nat_gateway: String,
    subnets: Vec<Subnet>,
    groups: Vec<String>,
    /// The VPC's own default security group: the group a launch that names none runs under.
    default_group: String,
}

/// The blocks and zones the subnets are carved out of: two sizes and two zones over the
/// three the rows place into, plus the one the NAT gateway itself sits in.
const SUBNET_BLOCKS: [(&str, &str); 4] = [
    ("10.90.1.0/24", "us-west-2a"),
    ("10.90.2.0/28", "us-west-2b"),
    ("10.90.3.0/24", "us-west-2a"),
    ("10.90.4.0/24", "us-west-2b"),
];

/// Which of [`SUBNET_BLOCKS`] the NAT gateway is created in: its own, so no row's address
/// bookkeeping is a question about the gateway's address too.
const NAT_SUBNET: usize = 3;

/// The subnets whose traffic leaves through the NAT gateway rather than through the internet
/// gateway: the ones the rows that ask for no public address place into ("with a public NAT
/// gateway, internet-bound traffic from a private subnet can be routed to the NAT gateway" —
/// `CreateNatGateway`).
const PRIVATE_SUBNETS: [usize; 2] = [0, 2];

/// Build the network through Amazon EC2's OWN operations: a VPC with an internet gateway
/// attached and a default route to it, a public NAT gateway with a route table of its own for
/// the private subnets, the subnets the rows place into, and two security groups of that VPC.
async fn ec2_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
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
    let route_table = element(
        &ec2(
            router,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc.as_str())],
        )
        .await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", route_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", internet_gateway.as_str()),
        ],
    )
    .await;
    let mut subnets = Vec::new();
    for (cidr_block, zone) in SUBNET_BLOCKS {
        let id = element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr_block),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        );
        subnets.push(Subnet {
            id,
            cidr_block: cidr_block.to_string(),
        });
    }
    let mut groups = Vec::new();
    for name in ["awsvpc-task-one", "awsvpc-task-two"] {
        groups.push(element(
            &ec2(
                router,
                "CreateSecurityGroup",
                &[
                    ("VpcId", vpc.as_str()),
                    ("GroupName", name),
                    ("GroupDescription", "the tasks of an awsvpc launch"),
                ],
            )
            .await,
            "groupId",
        ));
    }
    let default_group = element(
        &ec2(
            router,
            "DescribeSecurityGroups",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", vpc.as_str()),
                ("Filter.2.Name", "group-name"),
                ("Filter.2.Value.1", "default"),
            ],
        )
        .await,
        "groupId",
    );
    // The path a task with no public address reaches its registry by: a public NAT gateway in
    // its own subnet, and a route table of its own carrying the private subnets' default
    // route to it.
    let allocation = element(
        &ec2(router, "AllocateAddress", &[]).await,
        "allocationId",
    );
    let nat_gateway = element(
        &ec2(
            router,
            "CreateNatGateway",
            &[
                ("SubnetId", subnets[NAT_SUBNET].id.as_str()),
                ("AllocationId", allocation.as_str()),
            ],
        )
        .await,
        "natGatewayId",
    );
    let private_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", private_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("NatGatewayId", nat_gateway.as_str()),
        ],
    )
    .await;
    for index in PRIVATE_SUBNETS {
        ec2(
            router,
            "AssociateRouteTable",
            &[
                ("RouteTableId", private_table.as_str()),
                ("SubnetId", subnets[index].id.as_str()),
            ],
        )
        .await;
    }
    Network {
        vpc,
        internet_gateway,
        nat_gateway,
        subnets,
        groups,
        default_group,
    }
}

/// Create the cluster and register the one Fargate-capable `awsvpc` revision every launch
/// below places.
async fn seed_ecs(router: &MultiServiceRouter) {
    ecs_ok(
        router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "setup",
    )
    .await;
    ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "web",
                "image": IMAGE,
                "memory": 128,
            }],
        }),
        "setup",
    )
    .await;
}

/// One row of the table: the launch's own shape.
struct Row {
    case: &'static str,
    /// Which of the network's subnets the launch places into.
    subnet: usize,
    /// How many of the network's security groups the launch names, or `None` when it leaves
    /// the member out entirely.
    security_groups: Option<usize>,
    /// The `assignPublicIp` the launch names, or `None` when it leaves the member out.
    assign_public_ip: Option<&'static str>,
    /// How many tasks this one launch places.
    tasks: i64,
}

/// The rows: they differ along the number of security groups the launch names, whether it
/// asks for a public address, how many tasks one launch places, and the subnet (size, zone
/// and the way traffic leaves it) the interface is created in.
const ROWS: [Row; 3] = [
    Row {
        case: "one task in a /24 behind the NAT gateway, one security group, no public address",
        subnet: 0,
        security_groups: Some(1),
        assign_public_ip: Some("DISABLED"),
        tasks: 1,
    },
    Row {
        case: "one task in a /28 of another zone, two security groups, a public address",
        subnet: 1,
        security_groups: Some(2),
        assign_public_ip: Some("ENABLED"),
        tasks: 1,
    },
    Row {
        case: "two tasks in one launch that names no security group",
        subnet: 2,
        security_groups: None,
        assign_public_ip: None,
        tasks: 2,
    },
];

impl Row {
    /// The security groups the interface must sit in: the ones the launch named, and the
    /// VPC's own default group for a launch that named none.
    fn expected_groups(&self, network: &Network) -> Vec<String> {
        match self.security_groups {
            Some(count) => network.groups[..count].to_vec(),
            None => vec![network.default_group.clone()],
        }
    }

    /// Whether this launch asked for a public IPv4 address on the task's interface.
    fn wants_public_ip(&self) -> bool {
        self.assign_public_ip == Some("ENABLED")
    }

    /// The `networkConfiguration` this launch carries, with only the members it names.
    fn network_configuration(&self, network: &Network) -> Value {
        let mut awsvpc = json!({ "subnets": [network.subnets[self.subnet].id.clone()] });
        if self.security_groups.is_some() {
            awsvpc["securityGroups"] = json!(self.expected_groups(network));
        }
        if let Some(assign) = self.assign_public_ip {
            awsvpc["assignPublicIp"] = json!(assign);
        }
        json!({ "awsvpcConfiguration": awsvpc })
    }
}

/// The `availableIpAddressCount` Amazon EC2 reports for one subnet.
async fn available_addresses(router: &MultiServiceRouter, subnet: &str) -> i64 {
    let body = ec2(router, "DescribeSubnets", &[("SubnetId.1", subnet)]).await;
    let reported = element(&body, "availableIpAddressCount");
    reported.parse().unwrap_or_else(|_| {
        panic!("expected an integer availableIpAddressCount for {subnet}, got {reported:?}")
    })
}

/// One `Attachment$details` entry of a task, by name.
fn detail(task: &Value, name: &str) -> Option<String> {
    task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|attachment| attachment["type"].as_str() == Some("ElasticNetworkInterface"))
        .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
        .find(|entry| entry["name"].as_str() == Some(name))
        .and_then(|entry| entry["value"].as_str())
        .map(str::to_string)
}

/// The `ElasticNetworkInterface` attachment's own `status`.
fn attachment_status(task: &Value) -> Option<String> {
    task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|attachment| attachment["type"].as_str() == Some("ElasticNetworkInterface"))
        .and_then(|attachment| attachment["status"].as_str())
        .map(str::to_string)
}

/// `DescribeTasks` for the tasks `arns`, as the task views it reports.
async fn describe_tasks(router: &MultiServiceRouter, arns: &[String], case: &str) -> Vec<Value> {
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    described["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Look again at `DescribeTasks` until every task satisfies `holds`, and answer the views it
/// reported then. The CONDITION is polled for; nothing waits out a fixed span.
async fn poll_tasks_until(
    router: &MultiServiceRouter,
    arns: &[String],
    case: &str,
    description: &str,
    holds: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let tasks = describe_tasks(router, arns, case).await;
        if tasks.len() == arns.len() && tasks.iter().all(&holds) {
            return tasks;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {description} never came about for {} task(s); the last \
             DescribeTasks reported {tasks:?}",
            arns.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One network interface as Amazon EC2 describes it, reduced to the members this rule joins.
#[derive(Debug)]
struct Interface {
    private_ip_address: String,
    subnet_id: String,
    mac_address: String,
    vpc_id: String,
    status: String,
    attachment_status: String,
    groups: Vec<String>,
    requester_managed: String,
    interface_type: String,
    public_ip: Option<String>,
}

/// `DescribeNetworkInterfaces` for ONE id: the interface it reports, or `None` when EC2 holds
/// no such interface (which it says with [`INTERFACE_NOT_FOUND`] rather than an empty 200).
///
/// Read one interface at a time on purpose: an ec2Query answer nests `<item>` lists inside
/// each interface (its groups, its addresses), so a reader over the whole answer would tear
/// one interface into several. The interface's OWN `status` and `privateIpAddress` are read
/// past the attachment block, which carries members of those names too.
async fn describe_interface(
    router: &MultiServiceRouter,
    network_interface_id: &str,
    case: &str,
) -> Option<Interface> {
    let (status, body) = ec2_call(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    if status != 200 {
        assert_eq!(
            (status, element(&body, "Code").as_str()),
            (400, INTERFACE_NOT_FOUND),
            "case {case:?}: an id EC2 holds no interface for is refused {INTERFACE_NOT_FOUND}, \
             got {status} with {body}"
        );
        return None;
    }
    let item = block_of(&body, "networkInterfaceSet").unwrap_or_else(|| {
        panic!("case {case:?}: no <networkInterfaceSet> in the describe answer {body}")
    });
    let attachment = block_of(item, "attachment").unwrap_or_else(|| {
        panic!(
            "case {case:?}: the interface {network_interface_id} reports no attachment in {item}"
        )
    });
    // Everything past the attachment block is the interface's own, which is where its
    // `status` and `privateIpAddress` are read from.
    let own = item
        .split_once("</attachment>")
        .map(|(_, rest)| rest)
        .unwrap_or(item);
    Some(Interface {
        private_ip_address: opt_element(own, "privateIpAddress").unwrap_or_default(),
        subnet_id: opt_element(own, "subnetId").unwrap_or_default(),
        mac_address: opt_element(own, "macAddress").unwrap_or_default(),
        vpc_id: opt_element(own, "vpcId").unwrap_or_default(),
        status: opt_element(own, "status").unwrap_or_default(),
        attachment_status: opt_element(attachment, "status").unwrap_or_default(),
        groups: elements(own, "groupId"),
        requester_managed: opt_element(own, "requesterManaged").unwrap_or_default(),
        interface_type: opt_element(own, "interfaceType").unwrap_or_default(),
        public_ip: opt_element(item, "publicIp"),
    })
}

#[tokio::test]
async fn an_awsvpc_tasks_attachment_names_the_interface_ec2_holds_and_releases_it_on_stop() {
    let router = emulator();
    let network = ec2_network(&router).await;
    seed_ecs(&router).await;

    // What each subnet had free before anything ran in it: the count every assertion below
    // is relative to, so no row depends on how many addresses a subnet's block holds.
    let mut free_before: Vec<i64> = Vec::new();
    for subnet in &network.subnets {
        free_before.push(available_addresses(&router, &subnet.id).await);
    }

    // Every row's launch is made first, so the interfaces are created and attached
    // concurrently rather than one row's wait after another's.
    let mut launched: Vec<Vec<String>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let run = ecs_ok(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "count": row.tasks,
                "launchType": "FARGATE",
                "networkConfiguration": row.network_configuration(&network),
                "startedBy": format!("eni-row-{index}"),
            }),
            case,
        )
        .await;
        let failures = run["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "case {case:?}: expected the launch to place every task, got failures {failures:?}"
        );
        let arns: Vec<String> = run["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|task| task["taskArn"].as_str())
            .map(str::to_string)
            .collect();
        assert_eq!(
            i64::try_from(arns.len()).unwrap_or_default(),
            row.tasks,
            "case {case:?}: expected {} task(s) to be placed, got {run}",
            row.tasks
        );
        launched.push(arns);
    }

    // Then every task is waited for until it RUNS with its interface attached, and the EC2
    // view of that interface is joined to what the attachment reported.
    let mut interfaces_of_row: Vec<Vec<String>> = Vec::new();
    let mut every_address: Vec<String> = Vec::new();
    let mut every_interface: Vec<String> = Vec::new();
    for (row, arns) in ROWS.iter().zip(&launched) {
        let case = row.case;
        let subnet = &network.subnets[row.subnet];
        let tasks = poll_tasks_until(
            &router,
            arns,
            case,
            "every task running with its interface attached",
            |task| {
                task["lastStatus"].as_str() == Some("RUNNING")
                    && attachment_status(task).as_deref() == Some("ATTACHED")
                    && detail(task, "networkInterfaceId").is_some()
            },
        )
        .await;

        let mut row_interfaces = Vec::new();
        for task in &tasks {
            let reported_id = detail(task, "networkInterfaceId").unwrap_or_default();
            let reported_subnet = detail(task, "subnetId").unwrap_or_default();
            let reported_mac = detail(task, "macAddress").unwrap_or_default();
            let reported_address = detail(task, "privateIPv4Address").unwrap_or_default();
            assert_eq!(
                reported_subnet, subnet.id,
                "case {case:?}: the attachment reports the subnet the launch named, got \
                 {reported_subnet:?}"
            );

            // The join: the id in the attachment is an interface EC2 returns.
            let held = describe_interface(&router, &reported_id, case)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: ECS reports networkInterfaceId {reported_id:?} for the \
                         RUNNING task, but EC2 has no such interface"
                    )
                });
            assert_eq!(
                (
                    held.private_ip_address.as_str(),
                    held.subnet_id.as_str(),
                    held.mac_address.as_str(),
                ),
                (
                    reported_address.as_str(),
                    reported_subnet.as_str(),
                    reported_mac.as_str(),
                ),
                "case {case:?}: EC2's view of {reported_id} must agree with the attachment's \
                 three interface details, got {held:?}"
            );
            assert_eq!(
                held.vpc_id, network.vpc,
                "case {case:?}: the interface sits in the VPC the subnet belongs to, got {held:?}"
            );
            assert_eq!(
                (held.status.as_str(), held.attachment_status.as_str()),
                (INTERFACE_STATUS_IN_USE, ATTACHMENT_STATUS_ATTACHED),
                "case {case:?}: an interface a running task holds is in use and attached, got \
                 {held:?}"
            );
            assert_eq!(
                held.groups,
                row.expected_groups(&network),
                "case {case:?}: expected the groups {:?} the launch runs under, got {held:?}",
                row.expected_groups(&network)
            );
            assert_eq!(
                (held.requester_managed.as_str(), held.interface_type.as_str()),
                ("true", INTERFACE_TYPE),
                "case {case:?}: the interface is service-managed and of the ordinary kind, got \
                 {held:?}"
            );
            assert!(
                block_contains(&subnet.cidr_block, &held.private_ip_address),
                "case {case:?}: expected an address out of the subnet's block {}, got {held:?}",
                subnet.cidr_block
            );
            assert_eq!(
                held.public_ip.is_some(),
                row.wants_public_ip(),
                "case {case:?}: a public address is held exactly when the launch asked for \
                 one, got {held:?}"
            );
            every_address.push(held.private_ip_address.clone());
            every_interface.push(reported_id.clone());
            row_interfaces.push(reported_id);
        }
        interfaces_of_row.push(row_interfaces);
    }

    // No two tasks were handed one interface or one address: each is its own resource.
    let distinct_interfaces: BTreeSet<&String> = every_interface.iter().collect();
    assert_eq!(
        distinct_interfaces.len(),
        every_interface.len(),
        "case \"every task\": each task holds its own interface, got {every_interface:?}"
    );
    let distinct_addresses: BTreeSet<&String> = every_address.iter().collect();
    assert_eq!(
        distinct_addresses.len(),
        every_address.len(),
        "case \"every task\": each interface holds its own address, got {every_address:?}"
    );

    // The addresses those interfaces hold came OUT of their subnets: one per running task.
    let mut running_in_subnet: BTreeMap<usize, i64> = BTreeMap::new();
    for row in &ROWS {
        *running_in_subnet.entry(row.subnet).or_default() += row.tasks;
    }
    for (index, subnet) in network.subnets.iter().enumerate() {
        let expected = free_before[index] - running_in_subnet.get(&index).copied().unwrap_or(0);
        let free_now = available_addresses(&router, &subnet.id).await;
        assert_eq!(
            free_now, expected,
            "case \"the subnet {} holding {} task(s)\": expected {expected} addresses free \
             while they run, got {free_now}",
            subnet.id,
            running_in_subnet.get(&index).copied().unwrap_or(0)
        );
    }

    // An interface a task holds cannot be deleted by the account, and the refusal names the
    // interface the request named — while an interface the account created in the SAME subnet
    // deletes cleanly, so what is refused is the holding and not the subnet.
    let case = ROWS[0].case;
    let held_id = interfaces_of_row[0][0].clone();
    let (status, body) = ec2_call(
        &router,
        "DeleteNetworkInterface",
        &[("NetworkInterfaceId", held_id.as_str())],
    )
    .await;
    // The message is read through the reader that undoes the serializer's escaping, so the
    // sentence compares as EC2 worded it rather than as its XML encoding.
    let refusal_message = elements(&body, "Message").into_iter().next();
    assert_eq!(
        (
            status,
            element(&body, "Code").as_str(),
            refusal_message.as_deref()
        ),
        (
            400,
            INVALID_PARAMETER_VALUE,
            Some(format!("Network interface '{held_id}' is currently in use.").as_str())
        ),
        "case {case:?}: expected the in-use refusal for {held_id}, got {status} with {body}"
    );
    let own_id = element(
        &ec2(
            &router,
            "CreateNetworkInterface",
            &[
                ("SubnetId", network.subnets[0].id.as_str()),
                ("Description", "an interface the account created"),
            ],
        )
        .await,
        "networkInterfaceId",
    );
    ec2(
        &router,
        "DeleteNetworkInterface",
        &[("NetworkInterfaceId", own_id.as_str())],
    )
    .await;
    assert!(
        describe_interface(&router, &own_id, case).await.is_none(),
        "case {case:?}: the account's own interface {own_id} is gone once deleted"
    );
    // And the task's interface is still there: the refused delete removed nothing.
    assert!(
        describe_interface(&router, &held_id, case).await.is_some(),
        "case {case:?}: the refused delete leaves {held_id} in place"
    );

    // Then the tasks are stopped, and the interfaces created for them go with them: the ids
    // stop resolving in EC2 and the addresses are back in their subnets.
    for (row, arns) in ROWS.iter().zip(&launched) {
        for arn in arns {
            ecs_ok(
                &router,
                "StopTask",
                json!({ "cluster": CLUSTER, "task": arn, "reason": "the suite is done" }),
                row.case,
            )
            .await;
        }
    }
    for (row, arns) in ROWS.iter().zip(&launched) {
        poll_tasks_until(&router, arns, row.case, "every task stopped", |task| {
            task["lastStatus"].as_str() == Some("STOPPED")
        })
        .await;
    }
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut still_held = Vec::new();
        for id in &every_interface {
            if describe_interface(&router, id, "the stopped tasks")
                .await
                .is_some()
            {
                still_held.push(id.clone());
            }
        }
        if still_held.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the stopped tasks\": expected every task interface to be deleted, EC2 still \
             holds {still_held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
        // The ECS clock is what tears the interfaces down, so it is driven while waiting.
        for arns in &launched {
            describe_tasks(&router, arns, "the stopped tasks").await;
        }
    }
    for (index, subnet) in network.subnets.iter().enumerate() {
        let free_now = available_addresses(&router, &subnet.id).await;
        assert_eq!(
            free_now, free_before[index],
            "case \"the stopped tasks\": expected {} to have its addresses back, got {free_now} \
             against {} before the tasks ran",
            subnet.id, free_before[index]
        );
    }

    // Nothing of the tasks is left in the network either: a VPC whose dependencies are gone
    // is removable, which is only true if the interfaces really left EC2's records.
    ec2(
        &router,
        "DeleteNatGateway",
        &[("NatGatewayId", network.nat_gateway.as_str())],
    )
    .await;
    for group in &network.groups {
        ec2(
            &router,
            "DeleteSecurityGroup",
            &[("GroupId", group.as_str())],
        )
        .await;
    }
    for subnet in &network.subnets {
        ec2(&router, "DeleteSubnet", &[("SubnetId", subnet.id.as_str())]).await;
    }
    ec2(
        &router,
        "DetachInternetGateway",
        &[
            ("InternetGatewayId", network.internet_gateway.as_str()),
            ("VpcId", network.vpc.as_str()),
        ],
    )
    .await;
    ec2(
        &router,
        "DeleteInternetGateway",
        &[("InternetGatewayId", network.internet_gateway.as_str())],
    )
    .await;
    ec2(&router, "DeleteVpc", &[("VpcId", network.vpc.as_str())]).await;
    let described = ec2(&router, "DescribeVpcs", &[]).await;
    assert!(
        !elements(&described, "vpcId").contains(&network.vpc),
        "case \"the torn-down network\": expected {} to stop being describable, got {described}",
        network.vpc
    );
}

/// One row of the teardown table: what the VPC holds besides its internet gateway.
struct TeardownRow {
    case: &'static str,
    /// The second octet of the VPC's own `/16`, so no two rows share an address.
    block: u8,
    /// Whether a default route to the gateway was created in the VPC's route table.
    route_to_gateway: bool,
    /// How many subnets the VPC holds, which the caller deletes before the VPC.
    subnets: usize,
    /// How many security groups of its own the VPC holds, deleted the same way.
    groups: usize,
}

/// The rows differ along what else the VPC holds when its gateway is taken down: a route
/// pointing at the gateway, subnets, and security groups of its own.
const TEARDOWN_ROWS: [TeardownRow; 4] = [
    TeardownRow {
        case: "a VPC holding nothing but its gateway",
        block: 70,
        route_to_gateway: false,
        subnets: 0,
        groups: 0,
    },
    TeardownRow {
        case: "a VPC with a default route to its gateway",
        block: 71,
        route_to_gateway: true,
        subnets: 0,
        groups: 0,
    },
    TeardownRow {
        case: "a VPC with a routed gateway and one subnet",
        block: 72,
        route_to_gateway: true,
        subnets: 1,
        groups: 0,
    },
    TeardownRow {
        case: "a VPC with a routed gateway, two subnets and two groups of its own",
        block: 73,
        route_to_gateway: true,
        subnets: 2,
        groups: 2,
    },
];

#[tokio::test]
async fn a_vpc_with_an_internet_gateway_is_torn_down_by_its_own_api() {
    let router = emulator();

    for row in &TEARDOWN_ROWS {
        let case = row.case;
        let block = row.block;
        let vpc = element(
            &ec2(
                &router,
                "CreateVpc",
                &[("CidrBlock", &format!("10.{block}.0.0/16"))],
            )
            .await,
            "vpcId",
        );
        let gateway = element(
            &ec2(&router, "CreateInternetGateway", &[]).await,
            "internetGatewayId",
        );
        ec2(
            &router,
            "AttachInternetGateway",
            &[
                ("InternetGatewayId", gateway.as_str()),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await;
        if row.route_to_gateway {
            let route_table = element(
                &ec2(
                    &router,
                    "DescribeRouteTables",
                    &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc.as_str())],
                )
                .await,
                "routeTableId",
            );
            ec2(
                &router,
                "CreateRoute",
                &[
                    ("RouteTableId", route_table.as_str()),
                    ("DestinationCidrBlock", "0.0.0.0/0"),
                    ("GatewayId", gateway.as_str()),
                ],
            )
            .await;
        }
        let mut subnets = Vec::new();
        for index in 0..row.subnets {
            subnets.push(element(
                &ec2(
                    &router,
                    "CreateSubnet",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("CidrBlock", &format!("10.{block}.{index}.0/24")),
                        ("AvailabilityZone", &format!("{REGION}a")),
                    ],
                )
                .await,
                "subnetId",
            ));
        }
        let mut groups = Vec::new();
        for index in 0..row.groups {
            groups.push(element(
                &ec2(
                    &router,
                    "CreateSecurityGroup",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("GroupName", &format!("teardown-{block}-{index}")),
                        ("GroupDescription", "a group of this network"),
                    ],
                )
                .await,
                "groupId",
            ));
        }

        // A VPC that still holds its gateway is the one case `DependencyViolation` is for, so
        // the refusal is read here rather than being what the teardown runs into.
        let (status, body) = ec2_call(&router, "DeleteVpc", &[("VpcId", vpc.as_str())]).await;
        assert_eq!(
            (status, element(&body, "Code").as_str()),
            (400, "DependencyViolation"),
            "case {case:?}: a VPC whose gateway is still attached is refused, got {status} with \
             {body}"
        );

        for group in &groups {
            ec2(
                &router,
                "DeleteSecurityGroup",
                &[("GroupId", group.as_str())],
            )
            .await;
        }
        for subnet in &subnets {
            ec2(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
        }
        // The two gateway calls are served, and neither reports a refusal in its answer.
        for (action, params) in [
            (
                "DetachInternetGateway",
                vec![
                    ("InternetGatewayId", gateway.clone()),
                    ("VpcId", vpc.clone()),
                ],
            ),
            (
                "DeleteInternetGateway",
                vec![("InternetGatewayId", gateway.clone())],
            ),
        ] {
            let borrowed: Vec<(&str, &str)> = params
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();
            let (status, body) = ec2_call(&router, action, &borrowed).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {action} to answer 200, got {status} with {body}"
            );
            assert_eq!(
                opt_element(&body, "Code"),
                None,
                "case {case:?}: expected {action} to answer no refusal, got {body}"
            );
        }
        let (status, body) = ec2_call(&router, "DeleteVpc", &[("VpcId", vpc.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the emptied VPC to be deleted, got {status} with {body}"
        );
        let described = ec2(&router, "DescribeVpcs", &[]).await;
        assert!(
            !elements(&described, "vpcId").contains(&vpc),
            "case {case:?}: expected {vpc} to stop being describable, got {described}"
        );
    }
}
