// SPDX-License-Identifier: CC0-1.0

//! The endpoint network interfaces an interface VPC endpoint stands up, across the two
//! services that own them: EC2, which places them, and Elastic Load Balancing, which
//! resolves one of their addresses as an `ip` target.
//!
//! The rule under test: an interface VPC endpoint is answered in the non-terminal `pending`
//! state and reaches `available` from the CLOCK alone, and the interfaces it then reports
//! are REAL interfaces of the account — "An endpoint network interface is created … in an
//! Availability Zone" (`CreateVpcEndpointRequest$SubnetIds`), one per subnet the create
//! named, each reported by `DescribeNetworkInterfaces` in that subnet, in that subnet's
//! zone, in the endpoint's VPC, carrying the endpoint's own security groups and one private
//! address out of that subnet's block, and each selected by the `subnet-id` and `vpc-id`
//! filters that operation documents. Those addresses are therefore registrable as the `ip`
//! targets a load balancer forwards to, and Elastic Load Balancing reports each of them in
//! the zone of the subnet EC2 placed it in.
//!
//! Why this suite lives here rather than in the EC2 crate: the second half of the rule is
//! Elastic Load Balancing's — it resolves an address EC2 assigned, against EC2's own
//! network — so the two services have to share one engine for that resolution to be the one
//! a client sees.
//!
//! The rows span what the rule generalises over: how many subnets the create names (one, two
//! and three, in as many zones), which endpoint service of Amazon Web Services the endpoint
//! is created to, and whether the create names a security group of the VPC or leaves the
//! endpoint the VPC's default one. None of those decides where an interface is placed or
//! what the endpoint's state becomes.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{
    ec2_ok, register, target_group, target_zones, BOOT_STACK_SIZE, REGION,
};
use crate::query_wire::{block_of, element, elements, items_of};

/// The state `CreateVpcEndpoint` answers for an endpoint whose interfaces are still being
/// provisioned, in the spelling the filter that selects on it publishes: the
/// `vpc-endpoint-state` filter's documented values are "`pendingAcceptance` | `pending` |
/// `available` | `deleting` | `deleted` | `rejected` | `failed`"
/// (`DescribeVpcEndpointsRequest$Filters`).
const PENDING: &str = "pending";

/// The state the clock owes such an endpoint, in the same spelling.
const AVAILABLE: &str = "available";

/// The `NetworkInterfaceType` an endpoint network interface is — a published member of that
/// enum, so it is also what the `interface-type` filter selects on.
const ENDPOINT_INTERFACE_TYPE: &str = "vpc_endpoint";

/// How long a readiness poll here is given before the endpoint is called a
/// non-convergence — a generous ceiling on a loaded machine, never a span this suite waits
/// out: the loop below stops as soon as the state holds.
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a poll waits between reads, which only bounds how often the store is asked.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row of the table: one network, one endpoint, and the subnets it is placed in.
struct Row {
    case: &'static str,
    /// The block of the row's OWN VPC — each row builds its own network, so one row's
    /// interfaces can never be read as another's.
    vpc_cidr: &'static str,
    /// The endpoint service, without the `com.amazonaws.<region>.` prefix.
    service: &'static str,
    /// `(subnet block, zone)` for each subnet the create names.
    subnets: &'static [(&'static str, &'static str)],
    /// Whether the create names a security group of the VPC; a create that names none
    /// leaves the endpoint its VPC's default group
    /// (`CreateVpcEndpointRequest$SecurityGroupIds`).
    names_group: bool,
}

/// The rows: the `execute-api` endpoint a private REST API is reached through, in one subnet
/// and then in two, and an AWS Systems Manager endpoint in three. Both service names are
/// ones `DescribeVpcEndpointServices` publishes for the Region as interface-only.
const ROWS: &[Row] = &[
    Row {
        case: "an execute-api endpoint in one subnet, on the VPC's default group",
        vpc_cidr: "10.71.0.0/16",
        service: "execute-api",
        subnets: &[("10.71.1.0/24", "us-west-2a")],
        names_group: false,
    },
    Row {
        case: "an execute-api endpoint in two subnets, on a group the create names",
        vpc_cidr: "10.72.0.0/16",
        service: "execute-api",
        subnets: &[("10.72.1.0/24", "us-west-2a"), ("10.72.2.0/24", "us-west-2b")],
        names_group: true,
    },
    Row {
        case: "a Systems Manager endpoint in three subnets, one per zone",
        vpc_cidr: "10.73.0.0/16",
        service: "ssm",
        subnets: &[
            ("10.73.1.0/24", "us-west-2a"),
            ("10.73.2.0/24", "us-west-2b"),
            ("10.73.3.0/24", "us-west-2c"),
        ],
        names_group: true,
    },
];

/// One subnet of a row's network, as EC2 reported it.
struct Placement {
    subnet_id: String,
    /// The block the subnet was created with, which every address in it starts with.
    cidr_block: &'static str,
    /// The zone EC2 reported for the subnet — not the one the request asked for.
    zone: String,
}

/// The leading octets every address of a `/24` shares, `10.71.1.0/24` -> `10.71.1.`.
///
/// Every row above places its subnets in a `/24`, which is what makes a prefix comparison the
/// whole block: a wider block would share fewer octets, so the width is checked here rather
/// than weakening the assertion silently.
fn block_prefix(cidr_block: &str) -> String {
    assert!(
        cidr_block.ends_with("/24"),
        "this reader covers a /24 block, got {cidr_block:?}"
    );
    let (leading, _) = cidr_block
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("a dotted-quad block, got {cidr_block:?}"));
    format!("{leading}.")
}

/// Poll `read` until `settled` holds of what it answers, and answer that.
///
/// The client-side half of the rule: nothing is nudged between reads, so a value that only
/// ever changed because somebody called again never satisfies the predicate here either.
fn poll_until<R>(case: &str, mut read: impl FnMut() -> R, settled: impl Fn(&R) -> bool) -> R
where
    R: std::fmt::Debug,
{
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        let answer = read();
        if settled(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock owed this move and it never came; last read {answer:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The ids of every `<item>` of the one `name` set of an ec2Query answer.
fn set_items(case: &str, body: &str, name: &str) -> Vec<String> {
    let block =
        block_of(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"));
    items_of(block).into_iter().map(str::to_string).collect()
}

/// Every `groupId` of the one `groupSet` of an ec2Query answer.
fn group_ids(body: &str) -> Vec<String> {
    block_of(body, "groupSet")
        .map(|block| elements(block, "groupId"))
        .unwrap_or_default()
}

/// The network every row builds for itself: its VPC, its subnets as EC2 reported them, and
/// the security group the create will name, when the row names one.
struct Network {
    vpc: String,
    placements: Vec<Placement>,
    group: Option<String>,
}

/// Build one row's network through EC2 itself, so every value asserted below is one EC2
/// reported rather than one this suite made up.
fn seed_network(
    row: &Row,
    index: usize,
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
) -> Network {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", row.vpc_cidr)]),
        "vpcId",
    );
    let placements: Vec<Placement> = row
        .subnets
        .iter()
        .map(|(cidr_block, zone)| {
            let created = ec2_ok(
                router,
                runtime,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr_block),
                    ("AvailabilityZone", zone),
                ],
            );
            Placement {
                subnet_id: element(&created, "subnetId"),
                cidr_block,
                zone: element(&created, "availabilityZone"),
            }
        })
        .collect();
    let group = row.names_group.then(|| {
        let name = format!("endpoint-interfaces-{index}");
        element(
            &ec2_ok(
                router,
                runtime,
                "CreateSecurityGroup",
                &[
                    ("GroupName", name.as_str()),
                    ("GroupDescription", "the group the endpoint interfaces carry"),
                    ("VpcId", vpc.as_str()),
                ],
            ),
            "groupId",
        )
    });
    Network {
        vpc,
        placements,
        group,
    }
}

#[test]
fn an_endpoints_interfaces_are_placed_per_subnet_and_registrable_as_ip_targets() {
    std::thread::Builder::new()
        .name("ec2-endpoint-interface-targets".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            for (index, row) in ROWS.iter().enumerate() {
                let case = row.case;
                let network = seed_network(row, index, &router, &runtime);

                // --------------------------------------------------------------
                // The endpoint: answered pending, available on the clock alone.
                // --------------------------------------------------------------
                let service = format!("com.amazonaws.{REGION}.{}", row.service);
                let subnet_keys: Vec<String> = (1..=network.placements.len())
                    .map(|position| format!("SubnetId.{position}"))
                    .collect();
                let mut params: Vec<(&str, &str)> = vec![
                    ("VpcId", network.vpc.as_str()),
                    ("ServiceName", service.as_str()),
                    ("VpcEndpointType", "Interface"),
                    // This suite is about WHERE the interfaces are placed, so no row asks
                    // for a private hosted zone: "To use private DNS, the VPC must have DNS
                    // hostnames and DNS resolution enabled"
                    // (`CreateVpcEndpointRequest$PrivateDnsEnabled`), which is a different
                    // rule and the sibling EC2 suites' subject.
                    ("PrivateDnsEnabled", "false"),
                ];
                for (key, placement) in subnet_keys.iter().zip(&network.placements) {
                    params.push((key.as_str(), placement.subnet_id.as_str()));
                }
                if let Some(group) = network.group.as_deref() {
                    params.push(("SecurityGroupId.1", group));
                }
                let created = ec2_ok(&router, &runtime, "CreateVpcEndpoint", &params);
                let endpoint = element(&created, "vpcEndpointId");
                assert_eq!(
                    element(&created, "state"),
                    PENDING,
                    "case {case:?}: an interface endpoint is answered in its non-terminal \
                     state, got {created}"
                );
                let answered_interfaces = set_items(case, &created, "networkInterfaceIdSet");

                let describe_endpoint = || {
                    let body = ec2_ok(
                        &router,
                        &runtime,
                        "DescribeVpcEndpoints",
                        &[("VpcEndpointId.1", endpoint.as_str())],
                    );
                    (element(&body, "state"), body)
                };
                let (state, described) =
                    poll_until(case, describe_endpoint, |(state, _)| state == AVAILABLE);
                assert_eq!(
                    state, AVAILABLE,
                    "case {case:?}: expected the clock alone to carry the endpoint to \
                     {AVAILABLE}, got {described}"
                );

                // The move writes the STATE and nothing else: the interfaces the endpoint
                // reports once it is available are the ones its create stood up.
                let interfaces = set_items(case, &described, "networkInterfaceIdSet");
                // Which interfaces, not in which order: the rule is the SET the endpoint
                // holds, so the two readings are compared as sets.
                assert_eq!(
                    interfaces.iter().cloned().collect::<BTreeSet<String>>(),
                    answered_interfaces
                        .iter()
                        .cloned()
                        .collect::<BTreeSet<String>>(),
                    "case {case:?}: the materialised endpoint must report the interfaces its \
                     create answered, got {described}"
                );
                assert_eq!(
                    interfaces.len(),
                    network.placements.len(),
                    "case {case:?}: expected one endpoint network interface per subnet, got \
                     {described}"
                );
                assert_eq!(
                    interfaces.iter().collect::<BTreeSet<&String>>().len(),
                    interfaces.len(),
                    "case {case:?}: expected a distinct interface per subnet, got \
                     {interfaces:?}"
                );
                let endpoint_groups: BTreeSet<String> = group_ids(&described).into_iter().collect();
                assert!(
                    !endpoint_groups.is_empty(),
                    "case {case:?}: an interface endpoint carries at least its VPC's default \
                     group, got {described}"
                );

                // --------------------------------------------------------------
                // Each interface, as the account's own interface store reports it.
                // --------------------------------------------------------------
                // (subnet id -> the address the interface in that subnet holds)
                let mut placed: Vec<(String, String)> = Vec::new();
                for network_interface in &interfaces {
                    let reported = ec2_ok(
                        &router,
                        &runtime,
                        "DescribeNetworkInterfaces",
                        &[("NetworkInterfaceId.1", network_interface.as_str())],
                    );
                    let subnet_id = element(&reported, "subnetId");
                    let placement = network
                        .placements
                        .iter()
                        .find(|placement| placement.subnet_id == subnet_id)
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: {network_interface} reports {subnet_id}, which \
                                 the create never named: {reported}"
                            )
                        });
                    assert_eq!(
                        element(&reported, "vpcId"),
                        network.vpc,
                        "case {case:?}: expected the endpoint's VPC, got {reported}"
                    );
                    assert_eq!(
                        element(&reported, "availabilityZone"),
                        placement.zone,
                        "case {case:?}: expected the zone EC2 reported for \
                         {subnet_id}, got {reported}"
                    );
                    assert_eq!(
                        element(&reported, "interfaceType"),
                        ENDPOINT_INTERFACE_TYPE,
                        "case {case:?}: expected an endpoint interface, got {reported}"
                    );
                    assert_eq!(
                        group_ids(&reported).into_iter().collect::<BTreeSet<String>>(),
                        endpoint_groups,
                        "case {case:?}: expected the endpoint's own security groups on \
                         {network_interface}, got {reported}"
                    );
                    // "The IPv4 address of the network interface within the subnet"
                    // (`NetworkInterface$PrivateIpAddress`): out of the block of the subnet
                    // it was placed in, not merely of the VPC.
                    let address = element(&reported, "privateIpAddress");
                    let prefix = block_prefix(placement.cidr_block);
                    assert!(
                        address.starts_with(&prefix),
                        "case {case:?}: expected an address out of {block}, got {address:?}",
                        block = placement.cidr_block
                    );

                    // The documented selectors for this report — "group-id, mac-address,
                    // private-dns-name, private-ip-address, subnet-id, or vpc-id"
                    // (`DescribeNetworkInterfaces`) — find the same interface.
                    let by_subnet = ec2_ok(
                        &router,
                        &runtime,
                        "DescribeNetworkInterfaces",
                        &[
                            ("Filter.1.Name", "subnet-id"),
                            ("Filter.1.Value.1", subnet_id.as_str()),
                        ],
                    );
                    assert_eq!(
                        elements(&by_subnet, "networkInterfaceId"),
                        vec![network_interface.clone()],
                        "case {case:?}: expected the subnet-id filter to select the one \
                         interface placed in {subnet_id}, got {by_subnet}"
                    );
                    placed.push((subnet_id, address));
                }
                assert_eq!(
                    placed
                        .iter()
                        .map(|(subnet_id, _)| subnet_id.clone())
                        .collect::<BTreeSet<String>>(),
                    network
                        .placements
                        .iter()
                        .map(|placement| placement.subnet_id.clone())
                        .collect::<BTreeSet<String>>(),
                    "case {case:?}: expected every named subnet to hold exactly one of the \
                     endpoint's interfaces, got {placed:?}"
                );

                // The whole set is reachable through the VPC the endpoint is in: the row's
                // network holds no other interface.
                let by_vpc = ec2_ok(
                    &router,
                    &runtime,
                    "DescribeNetworkInterfaces",
                    &[
                        ("Filter.1.Name", "vpc-id"),
                        ("Filter.1.Value.1", network.vpc.as_str()),
                    ],
                );
                assert_eq!(
                    elements(&by_vpc, "networkInterfaceId")
                        .into_iter()
                        .collect::<BTreeSet<String>>(),
                    interfaces.iter().cloned().collect::<BTreeSet<String>>(),
                    "case {case:?}: expected the vpc-id filter to select the endpoint's \
                     interfaces, got {by_vpc}"
                );

                // --------------------------------------------------------------
                // Elastic Load Balancing resolves those addresses, in EC2's network.
                // --------------------------------------------------------------
                let group_name = format!("endpoint-eni-targets-{index}");
                let targets = target_group(&router, &runtime, &group_name, "ip", &network.vpc);
                for (subnet_id, address) in &placed {
                    // No zone is named: a load balancer registering an endpoint interface
                    // knows the address, and the zone is the one the address's own subnet is
                    // in — which only EC2 can answer.
                    let registered = register(&router, &runtime, &targets, address, None);
                    assert_eq!(
                        registered.status, 200,
                        "case {case:?}: expected the address EC2 placed in {subnet_id} to be \
                         registrable, got {registered:?}"
                    );
                }
                let expected: BTreeSet<(String, String)> = placed
                    .iter()
                    .map(|(subnet_id, address)| {
                        let zone = network
                            .placements
                            .iter()
                            .find(|placement| &placement.subnet_id == subnet_id)
                            .map(|placement| placement.zone.clone())
                            .expect("the subnet is one of the row's");
                        (address.clone(), zone)
                    })
                    .collect();
                assert_eq!(
                    target_zones(&router, &runtime, &targets)
                        .into_iter()
                        .collect::<BTreeSet<(String, String)>>(),
                    expected,
                    "case {case:?}: expected every endpoint interface to be reported in the \
                     zone of its own subnet"
                );
            }
        })
        .expect("spawn the endpoint-interface thread")
        .join()
        .expect("an endpoint's interfaces must be placed per subnet and registrable");
}
