// SPDX-License-Identifier: CC0-1.0

//! Which LAUNCHES are given a public IPv4 address of Amazon's pool, and what the instance then
//! reports about it — driven over the wire the service speaks (`ec2Query`: an
//! `application/x-www-form-urlencoded` POST answered with XML, README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! ONE RULE is under test: a launch gives its instance's PRIMARY network interface a public
//! IPv4 address out of Amazon's pool exactly when the launch's own interface specification
//! asks for one, and — for a launch that asks for nothing either way — exactly when the subnet
//! the instance is placed in has `MapPublicIpOnLaunch` true AS IT STANDS at that launch
//! ("Indicates whether instances launched in this subnet receive a public IPv4 address",
//! `Subnet$MapPublicIpOnLaunch`, which is the attribute "if launching into a default subnet,
//! the default value is `true`" (`InstanceNetworkInterfaceSpecification$AssociatePublicIpAddress`)
//! reads off). An instance given one reports it as `Instance$PublicIpAddress` (wire
//! `ipAddress`), the public hostname of that address as `Instance$PublicDnsName` (wire
//! `dnsName`), and an association Amazon owns on the primary entry of
//! `Instance$NetworkInterfaces` — one record read three ways, so a client that formats a
//! connection string from any of them gets the same address. An instance given none reports no
//! `ipAddress` member at all, an empty `dnsName`, and a primary interface holding no
//! association.
//!
//! The rows differ along what that generalises over: where the SUBNET's answer comes from (the
//! account's default subnet, a subnet left as `CreateSubnet` made it, one the caller turned the
//! attribute ON for, one turned on and then OFF again) and what the LAUNCH itself asks
//! (nothing at all, an interface entry asking nothing, one asking for an address, one refusing
//! it) — the two rows where those disagree being what makes the launch's own answer the
//! deciding one. One further row is the restriction the same member states: "you cannot specify
//! more than one network interface in the request", so a launch naming two receives none even
//! in a subnet that maps them.
//!
//! Nothing here waits a fixed span of the clock: the move to `running` is a stored due instant,
//! so each row LOOKS AGAIN until the instance has moved or a deadline far past the service's
//! own transition span has passed, and no assertion is made about elapsed time.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names: `RunInstances` only has to accept the id, and which image an
/// instance was launched from has no bearing on its public address.
const AMI: &str = "ami-0123456789abcdef0";

/// The network this suite creates, outside the `172.31.0.0/16` of the account's default VPC so
/// a reported private address says which network it came from, and the four subnets it holds —
/// all in ONE Availability Zone, so an interface a launch names is never outside its
/// instance's zone.
const VPC_CIDR: &str = "10.76.0.0/16";
const PLAIN_CIDR: &str = "10.76.1.0/24";
const MAPPING_CIDR: &str = "10.76.2.0/24";
const TOGGLED_CIDR: &str = "10.76.3.0/24";
const SECOND_CIDR: &str = "10.76.4.0/24";

/// The `IpOwnerId` an association carries for an address AMAZON assigned, as the service
/// reports it — read from the crate rather than written out here, so the suite and the service
/// cannot spell it two ways.
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

/// What one instance reports about its state and the public address of its primary interface,
/// read from the INSTANCE's own report alone — `Instance$PublicIpAddress`,
/// `Instance$PublicDnsName` and the association on its `Instance$NetworkInterfaces` entry at
/// device index 0 — which is what a client reads after a launch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceView {
    /// `instanceState/name`.
    state: String,
    /// `ipAddress` — `None` when the member is ABSENT, which is what an instance holding no
    /// public address reports.
    public_ip: Option<String>,
    /// `dnsName` — reported either way, EMPTY for an instance that holds no public address.
    public_dns: Option<String>,
    /// The `(publicIp, ipOwnerId)` of the association on the primary interface, or `None`
    /// when that interface holds no association at all.
    primary_association: Option<(Option<String>, Option<String>)>,
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
    // The reported instance is the element carrying an `instanceId` of its own, so a member is
    // never read out of a shape that merely sits beside it.
    let instance = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: {instance_id} must be reported: {body}"));
    // "Each instance has a default network interface, called the primary network interface"
    // at device index 0, so an instance report that carries none at all is a failure of the
    // case rather than an instance holding no association.
    let primary = set_items(instance, "networkInterfaceSet")
        .into_iter()
        .find(|item| {
            child(*item, "attachment")
                .and_then(|attachment| child_text(attachment, "deviceIndex"))
                .as_deref()
                == Some("0")
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: {instance_id} must report a primary interface: {body}")
        });
    InstanceView {
        state: child(instance, "instanceState")
            .and_then(|state| child_text(state, "name"))
            .unwrap_or_default(),
        public_ip: child_text(instance, "ipAddress"),
        public_dns: child_text(instance, "dnsName"),
        primary_association: child(primary, "association").map(|association| {
            (
                child_text(association, "publicIp"),
                child_text(association, "ipOwnerId"),
            )
        }),
    }
}

/// Look again until `instance_id` reports `running`, or a generous deadline passes.
async fn until_running(case: &str, router: &MultiServiceRouter, instance_id: &str) -> InstanceView {
    let deadline = poll_deadline();
    loop {
        let view = described(case, router, instance_id).await;
        if view.state == "running" {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must reach \"running\", got {view:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The `NetworkInterface.{position}.*` form fields of one interface entry, as ec2Query flattens
/// the launch's `NetworkInterfaces` list into indexed keys.
fn interface(position: usize, members: &[(&str, &str)]) -> Vec<Param> {
    members
        .iter()
        .map(|(name, value)| param(&format!("NetworkInterface.{position}.{name}"), value))
        .collect()
}

/// What the rule says about one row's instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The launch is given a public IPv4 address of Amazon's pool.
    Address,
    /// The launch is given none at all.
    NoAddress,
}

/// One row: the case, the launch parameters beyond the image and the counts, and what the rule
/// says about the instance it mints.
struct Row {
    case: &'static str,
    launch: Vec<Param>,
    expected: Expected,
}

/// RULE: a launch's own interface entry decides whether its instance is given a public IPv4
/// address, and a launch that asks nothing takes the answer of the subnet it is placed in as
/// that subnet's `MapPublicIpOnLaunch` stands at the launch; the instance reports the address,
/// its public hostname and the association on its primary interface together.
#[tokio::test]
async fn a_launch_is_given_a_public_address_when_it_or_its_subnet_asks_for_one() {
    let (router, _engine) = super::ec2_query::emulator();
    let zone = ec2::state::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("a Region reports Availability Zones");

    let vpc_id = {
        let body = ok("the VPC", &router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
        element(&body, "vpcId").unwrap_or_default()
    };
    // The public DNS name of a public address is the VPC's to give — "only available if you've
    // enabled DNS hostnames for your VPC" (`Instance$PublicDnsName`), which a VPC a caller
    // creates does not do until asked — so this VPC is asked to hand them out and the rows can
    // read the name beside the address each one holds.
    ok(
        "the VPC's DNS hostnames",
        &router,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
    let subnet = async |case: &str, cidr_block: &str| -> String {
        let body = ok(
            case,
            &router,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param("CidrBlock", cidr_block),
                param("AvailabilityZone", &zone),
            ],
        )
        .await;
        element(&body, "subnetId").unwrap_or_default()
    };
    // `CreateSubnet` maps no public address on launch unless something turns it on, so these
    // three subnets differ only in what the caller then did to that one attribute.
    let plain = subnet("the subnet left as created", PLAIN_CIDR).await;
    let mapping = subnet("the subnet turned on", MAPPING_CIDR).await;
    let toggled = subnet("the subnet turned on and off again", TOGGLED_CIDR).await;
    let second = subnet("the subnet of a second interface", SECOND_CIDR).await;
    let map_on_launch = async |case: &str, subnet_id: &str, value: &str| {
        ok(
            case,
            &router,
            "ModifySubnetAttribute",
            &[
                param("SubnetId", subnet_id),
                param("MapPublicIpOnLaunch.Value", value),
            ],
        )
        .await;
    };
    map_on_launch("the subnet turned on", &mapping, "true").await;
    map_on_launch("the subnet turned on and off again", &toggled, "true").await;
    map_on_launch("the subnet turned on and off again", &toggled, "false").await;

    let rows: Vec<Row> = vec![
        Row {
            // The account's default subnet maps public IPv4 addresses on launch, which is what
            // "if launching into a default subnet, the default value is true" rests on.
            case: "a launch that asks nothing, in the account's default subnet",
            launch: Vec::new(),
            expected: Expected::Address,
        },
        Row {
            case: "a launch that asks nothing, in a subnet left as created",
            launch: vec![param("SubnetId", &plain)],
            expected: Expected::NoAddress,
        },
        Row {
            case: "a launch that asks nothing, in a subnet the caller turned the attribute on \
                   for",
            launch: vec![param("SubnetId", &mapping)],
            expected: Expected::Address,
        },
        Row {
            case: "a launch that asks nothing, in a subnet turned on and then off again",
            launch: vec![param("SubnetId", &toggled)],
            expected: Expected::NoAddress,
        },
        Row {
            case: "an eth0 entry that asks nothing, in a subnet that maps one",
            launch: interface(1, &[("DeviceIndex", "0"), ("SubnetId", &mapping)]),
            expected: Expected::Address,
        },
        Row {
            case: "an eth0 entry that asks for one, in a subnet that maps none",
            launch: interface(
                1,
                &[
                    ("DeviceIndex", "0"),
                    ("SubnetId", &plain),
                    ("AssociatePublicIpAddress", "true"),
                ],
            ),
            expected: Expected::Address,
        },
        Row {
            // The two disagree the other way: an entry that refuses the address is honoured
            // over the subnet's own answer.
            case: "an eth0 entry that refuses one, in a subnet that maps one",
            launch: interface(
                1,
                &[
                    ("DeviceIndex", "0"),
                    ("SubnetId", &mapping),
                    ("AssociatePublicIpAddress", "false"),
                ],
            ),
            expected: Expected::NoAddress,
        },
        Row {
            case: "an eth0 entry that asks for one, in a subnet that maps one",
            launch: interface(
                1,
                &[
                    ("DeviceIndex", "0"),
                    ("SubnetId", &mapping),
                    ("AssociatePublicIpAddress", "true"),
                ],
            ),
            expected: Expected::Address,
        },
        Row {
            // "You cannot specify more than one network interface in the request"
            // (`AssociatePublicIpAddress`): the restriction binds the answer the subnet gives
            // as well as an explicit one, so a launch naming two interfaces receives none.
            case: "two interface entries that ask nothing, in a subnet that maps one",
            launch: {
                let mut fields = interface(1, &[("DeviceIndex", "0"), ("SubnetId", &mapping)]);
                fields.extend(interface(
                    2,
                    &[("DeviceIndex", "1"), ("SubnetId", &second)],
                ));
                fields
            },
            expected: Expected::NoAddress,
        },
    ];

    // Every instance is launched first, so the moves the clock owes them all run at once
    // rather than one row at a time.
    let mut launched: Vec<String> = Vec::new();
    for row in &rows {
        let mut params = vec![
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        params.extend(row.launch.iter().cloned());
        let body = ok(row.case, &router, "RunInstances", &params).await;
        launched.push(element(&body, "instanceId").unwrap_or_else(|| {
            panic!(
                "case {case:?}: a launch reports its instance: {body}",
                case = row.case
            )
        }));
    }

    let mut assigned: BTreeSet<String> = BTreeSet::new();
    for (row, instance_id) in rows.iter().zip(&launched) {
        let running = until_running(row.case, &router, instance_id).await;
        match row.expected {
            Expected::Address => {
                let public_ip = running.public_ip.clone().unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected an ipAddress on the running instance, got \
                         {running:?}",
                        case = row.case
                    )
                });
                assert!(
                    public_ip.parse::<Ipv4Addr>().is_ok(),
                    "case {case:?}: expected an IPv4 address, got {public_ip:?}",
                    case = row.case
                );
                // The public hostname of that very address, built as EC2 builds one for the
                // Region the instance runs in — read from the crate, so the suite and the
                // service cannot build the name two ways.
                assert_eq!(
                    running.public_dns.as_deref(),
                    Some(ec2::state::public_dns_name(&public_ip, REGION).as_str()),
                    "case {case:?}: expected the public hostname of {public_ip}, got {running:?}",
                    case = row.case
                );
                assert_eq!(
                    running.primary_association,
                    Some((
                        Some(public_ip.clone()),
                        Some(amazon_owner().to_string())
                    )),
                    "case {case:?}: expected the primary interface to hold {public_ip} as an \
                     address Amazon assigned, got {running:?}",
                    case = row.case
                );
                assert!(
                    assigned.insert(public_ip.clone()),
                    "case {case:?}: {public_ip} was assigned to another instance too, got \
                     {assigned:?}",
                    case = row.case
                );
            }
            Expected::NoAddress => {
                assert_eq!(
                    running.public_ip, None,
                    "case {case:?}: expected NO ipAddress member at all, got {running:?}",
                    case = row.case
                );
                assert_eq!(
                    running.public_dns.as_deref().unwrap_or_default(),
                    "",
                    "case {case:?}: expected an empty dnsName beside no address, got {running:?}",
                    case = row.case
                );
                assert_eq!(
                    running.primary_association, None,
                    "case {case:?}: expected the primary interface to hold no association, got \
                     {running:?}",
                    case = row.case
                );
            }
        }
    }
}
