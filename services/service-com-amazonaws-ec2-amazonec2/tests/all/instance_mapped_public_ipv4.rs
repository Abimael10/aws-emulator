// SPDX-License-Identifier: CC0-1.0

//! The public IPv4 address AMAZON maps to a running instance, across a stop and a start —
//! driven over the wire the service speaks (`ec2Query`: an
//! `application/x-www-form-urlencoded` POST answered with XML, README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! ONE RULE is under test: a public IPv4 address out of Amazon's pool belongs to the RUNNING
//! instance, not to the instance record. "The public IPv4 address is released when you stop …
//! your instance" and a new one is assigned when it is started again (the EC2 User Guide's
//! "Public IPv4 addresses"), so an instance that has reached `stopped` reports NO public
//! address at all — the member is absent rather than empty — its interface holds no
//! association, and it is gone from the `instance-state-name=running` report; a later start
//! gives it a DIFFERENT address. An Elastic IP the account allocated is the other kind and
//! survives the stop: "an Elastic IP address … remains associated with your instance when you
//! stop it".
//!
//! The rows span what that generalises over — where the address came from (the subnet's own
//! `MapPublicIpOnLaunch`, a launch's own `AssociatePublicIpAddress`, an Elastic IP the account
//! associated) and an instance that was never given one at all — so no single recorded launch
//! is the specification.
//!
//! Every wait here is for the CONDITION rather than for a span of the clock: the poll looks
//! again until the instance has moved or a deadline far past the service's own transition span
//! has passed, so a loaded host only waits longer.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, filters, primary_network_interface};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names: `RunInstances` only has to accept the id, and which image an
/// instance was launched from has no bearing on its public address.
const AMI: &str = "ami-0123456789abcdef0";

/// The network this suite creates for the rows that must NOT be given an address by their
/// subnet: a VPC outside the `172.31.0.0/16` of the default VPC, and one subnet in it.
/// `CreateSubnet` maps no public address on launch unless something turns it on, which is what
/// makes such a subnet a row of its own.
const VPC_CIDR: &str = "10.74.0.0/16";
const SUBNET_CIDR: &str = "10.74.1.0/24";

/// The `IpOwnerId` of an address Amazon assigned itself, as the service reports it — read from
/// the crate rather than written out here, so the suite and the service cannot spell it two
/// ways.
fn amazon_owner() -> &'static str {
    ec2::ops::describe_network_interface_attribute::AMAZON_ASSIGNED_IP_OWNER_ID
}

/// How long a poll keeps looking before it gives up: far past the span the service moves an
/// instance over ([`ec2::state::TRANSITION_DELAY`]), so the deadline is generous on any host.
fn poll_deadline() -> Instant {
    Instant::now() + ec2::state::TRANSITION_DELAY * 40
}

/// How long the poll waits between two looks — a fraction of the service's own span, so a move
/// is seen soon after it falls due.
fn poll_interval() -> Duration {
    ec2::state::TRANSITION_DELAY / 20
}

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// What one instance reports about its state and its public address.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceView {
    /// `instanceState/name`.
    state: String,
    /// `ipAddress` — `None` when the member is ABSENT, which is what an instance with no
    /// public address reports.
    public_ip: Option<String>,
    /// `dnsName` — the member is reported either way, EMPTY for an instance that holds no
    /// public address, so what a row asserts is whether it carries a name at all.
    public_dns: Option<String>,
}

/// What `DescribeInstances` reports for one instance.
async fn described(case: &str, router: &MultiServiceRouter, instance_id: &str) -> InstanceView {
    let body = ok(
        case,
        router,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must be reported: {body}"));
    InstanceView {
        state: child(instance, "instanceState")
            .and_then(|state| child_text(state, "name"))
            .unwrap_or_default(),
        public_ip: child_text(instance, "ipAddress"),
        public_dns: child_text(instance, "dnsName"),
    }
}

/// Look again until `instance_id` reports `wanted`, or a generous deadline passes.
async fn until_state(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    wanted: &str,
) -> InstanceView {
    let deadline = poll_deadline();
    loop {
        let view = described(case, router, instance_id).await;
        if view.state == wanted {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must reach {wanted:?}, got {view:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The `association` the PRIMARY interface of `instance_id` holds, as
/// `(publicIp, ipOwnerId, allocationId)` — `None` when it holds none at all.
async fn primary_association(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let network_interface_id = primary_network_interface(router, REGION, case, instance_id).await;
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &[param("NetworkInterfaceId.1", &network_interface_id)],
    )
    .await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| {
            panic!("case {case:?}: {network_interface_id} must be reported: {body}")
        })
        .to_owned();
    child(interface, "association").map(|association| {
        (
            child_text(association, "publicIp"),
            child_text(association, "ipOwnerId"),
            child_text(association, "allocationId"),
        )
    })
}

/// The instance ids the `instance-state-name=running` report carries.
async fn running_instance_ids(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let body = ok(
        case,
        router,
        "DescribeInstances",
        &filters(&[("instance-state-name", &["running"])]),
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "instanceId").is_some())
        .filter_map(|node| child_text(node, "instanceId"))
        .collect()
}

/// Where one row's public address comes from, which is what decides what the stop does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// Amazon maps one to the instance — the subnet's own setting, or the launch's.
    Mapped,
    /// The account allocated one and associated it, so it outlives the running instance.
    Elastic,
    /// The instance was never given one.
    None,
}

/// One row: the case, the launch it is made by, and where its address comes from.
struct Row {
    case: &'static str,
    /// The parameters beyond the image and the counts.
    launch: Vec<Param>,
    address: Address,
}

/// RULE: a MAPPED public IPv4 address is released when the instance stops and a different one
/// is assigned when it starts again, while an Elastic IP the account associated survives the
/// stop and an instance that was given no address never reports one.
#[tokio::test]
async fn a_stop_releases_the_mapped_public_address_and_a_start_assigns_a_new_one() {
    let (router, _engine) = super::ec2_query::emulator();

    // The subnet of the rows whose address does not come from the subnet they are in:
    // `CreateSubnet` maps no public address on launch.
    let vpc_id = {
        let body = ok("the VPC", &router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
        element(&body, "vpcId").unwrap_or_default()
    };
    let subnet_id = {
        let body = ok(
            "the subnet",
            &router,
            "CreateSubnet",
            &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
        )
        .await;
        element(&body, "subnetId").unwrap_or_default()
    };

    let rows: Vec<Row> = vec![
        Row {
            // "Indicates whether instances launched in this subnet receive a public IPv4
            // address" (`Subnet$MapPublicIpOnLaunch`) — a default subnet does.
            case: "a launch into a subnet that maps one",
            launch: Vec::new(),
            address: Address::Mapped,
        },
        Row {
            case: "a launch into a subnet that maps none",
            launch: vec![param("SubnetId", &subnet_id)],
            address: Address::None,
        },
        Row {
            // "Indicates whether to assign a public IPv4 address to an instance you launch in
            // a VPC" (`InstanceNetworkInterfaceSpecification$AssociatePublicIpAddress`): the
            // launch asks for one in a subnet that maps none.
            case: "a launch whose own interface asks for one",
            launch: vec![
                param("NetworkInterface.1.DeviceIndex", "0"),
                param("NetworkInterface.1.SubnetId", &subnet_id),
                param("NetworkInterface.1.AssociatePublicIpAddress", "true"),
            ],
            address: Address::Mapped,
        },
        Row {
            // The account's own address, associated after the launch: associating one
            // displaces the mapped address the subnet gave the instance, and what it holds
            // then is an allocation of the account's rather than one of Amazon's.
            case: "an instance holding an Elastic IP of the account",
            launch: Vec::new(),
            address: Address::Elastic,
        },
    ];

    // Every instance is launched first, so the moves the clock owes them all run at once
    // rather than one row at a time.
    let mut launched: Vec<(usize, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut params = vec![
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        params.extend(row.launch.iter().cloned());
        let body = ok(row.case, &router, "RunInstances", &params).await;
        let instance_id = element(&body, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: a launch reports its instance: {body}", case = row.case));
        launched.push((index, instance_id));
    }

    // The address each row holds while it runs, and what it is to the account.
    let mut before: Vec<Option<String>> = vec![None; rows.len()];
    for (index, instance_id) in &launched {
        let row = &rows[*index];
        let running = until_state(row.case, &router, instance_id, "running").await;
        if row.address == Address::Elastic {
            // Allocated and associated AFTER the launch, which is how an account gives a
            // running instance an address of its own.
            let allocated = ok(row.case, &router, "AllocateAddress", &[]).await;
            let allocation_id = element(&allocated, "allocationId").unwrap_or_default();
            let elastic_ip = element(&allocated, "publicIp").unwrap_or_default();
            ok(
                row.case,
                &router,
                "AssociateAddress",
                &[
                    param("InstanceId", instance_id),
                    param("AllocationId", &allocation_id),
                ],
            )
            .await;
            let held = described(row.case, &router, instance_id).await;
            assert_eq!(
                held.public_ip.as_deref(),
                Some(elastic_ip.as_str()),
                "case {case:?}: the instance must report the Elastic IP it was associated \
                 with, got {held:?}",
                case = row.case,
            );
            before[*index] = Some(elastic_ip);
            continue;
        }
        match row.address {
            Address::Mapped => {
                let public_ip = running.public_ip.clone().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a running instance given a mapped address reports it, \
                         got {running:?}",
                        case = row.case,
                    )
                });
                assert!(
                    running
                        .public_dns
                        .as_deref()
                        .is_some_and(|name| !name.is_empty()),
                    "case {case:?}: the public DNS name follows the address, got {running:?}",
                    case = row.case,
                );
                let association = primary_association(row.case, &router, instance_id).await;
                assert_eq!(
                    association
                        .as_ref()
                        .and_then(|(ip, owner, _)| Some((ip.clone()?, owner.clone()?))),
                    Some((public_ip.clone(), amazon_owner().to_string())),
                    "case {case:?}: the primary interface must hold the address as one Amazon \
                     assigned, got {association:?}",
                    case = row.case,
                );
                before[*index] = Some(public_ip);
            }
            Address::None => assert_eq!(
                running.public_ip, None,
                "case {case:?}: an instance given no address must omit the member, got \
                 {running:?}",
                case = row.case,
            ),
            Address::Elastic => unreachable!("answered above"),
        }
    }

    // One stop for the whole set, so the `stopped` each row waits for falls due at once.
    let mut stop = Vec::new();
    for (position, (_, instance_id)) in launched.iter().enumerate() {
        stop.push(param(&format!("InstanceId.{}", position + 1), instance_id));
    }
    ok("the stop", &router, "StopInstances", &stop).await;

    for (index, instance_id) in &launched {
        let row = &rows[*index];
        let stopped = until_state(row.case, &router, instance_id, "stopped").await;
        let association = primary_association(row.case, &router, instance_id).await;
        match row.address {
            Address::Mapped => {
                assert_eq!(
                    stopped.public_ip, None,
                    "case {case:?}: a stopped instance's mapped address is released, so the \
                     member must be ABSENT, got {stopped:?}",
                    case = row.case,
                );
                assert_eq!(
                    stopped.public_dns.as_deref().unwrap_or_default(),
                    "",
                    "case {case:?}: the public DNS name goes with the address it was built \
                     from, got {stopped:?}",
                    case = row.case,
                );
                assert_eq!(
                    association, None,
                    "case {case:?}: the primary interface must hold no association once the \
                     mapped address is released, got {association:?}",
                    case = row.case,
                );
            }
            Address::Elastic => {
                assert_eq!(
                    stopped.public_ip.as_deref(),
                    before[*index].as_deref(),
                    "case {case:?}: an Elastic IP remains associated across a stop, got \
                     {stopped:?}",
                    case = row.case,
                );
                assert!(
                    association
                        .as_ref()
                        .is_some_and(|(_, _, allocation_id)| allocation_id.is_some()),
                    "case {case:?}: the interface must still hold the allocation, got \
                     {association:?}",
                    case = row.case,
                );
            }
            Address::None => assert_eq!(
                stopped.public_ip, None,
                "case {case:?}: an instance given no address still reports none, got {stopped:?}",
                case = row.case,
            ),
        }
    }

    // Every stopped instance is out of the running report, whatever address it had.
    let running_now = running_instance_ids("the running filter", &router).await;
    for (_, instance_id) in &launched {
        assert!(
            !running_now.contains(instance_id),
            "a stopped instance must be absent from the instance-state-name=running report, \
             got {running_now:?} for {instance_id}"
        );
    }

    // One start for the whole set, the mirror of the stop above.
    let mut start = Vec::new();
    for (position, (_, instance_id)) in launched.iter().enumerate() {
        start.push(param(&format!("InstanceId.{}", position + 1), instance_id));
    }
    ok("the start", &router, "StartInstances", &start).await;

    for (index, instance_id) in &launched {
        let row = &rows[*index];
        let running = until_state(row.case, &router, instance_id, "running").await;
        match row.address {
            Address::Mapped => {
                let public_ip = running.public_ip.clone().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a started instance is given a mapped address again, \
                         got {running:?}",
                        case = row.case,
                    )
                });
                assert_ne!(
                    Some(public_ip.as_str()),
                    before[*index].as_deref(),
                    "case {case:?}: the address a start assigns is a NEW one, got {public_ip}",
                    case = row.case,
                );
                let association = primary_association(row.case, &router, instance_id).await;
                assert_eq!(
                    association.as_ref().and_then(|(ip, _, _)| ip.clone()),
                    Some(public_ip),
                    "case {case:?}: the interface must hold the address the instance reports, \
                     got {association:?}",
                    case = row.case,
                );
            }
            Address::Elastic => assert_eq!(
                running.public_ip.as_deref(),
                before[*index].as_deref(),
                "case {case:?}: an Elastic IP is still the instance's address after a start, \
                 got {running:?}",
                case = row.case,
            ),
            Address::None => assert_eq!(
                running.public_ip, None,
                "case {case:?}: a start gives no address to an instance that gets none, got \
                 {running:?}",
                case = row.case,
            ),
        }
    }
}
