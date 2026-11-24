// SPDX-License-Identifier: CC0-1.0

//! A NAT gateway address reports the Availability Zone of the Elastic IP address it holds —
//! and only when it holds one.
//!
//! THE RULE: `NatGatewayAddress$AvailabilityZone` is "The Availability Zone where this Elastic
//! IP address (EIP) is being used to handle outbound NAT traffic" and `$AvailabilityZoneId`
//! "the ID of the Availability Zone where this Elastic IP address (EIP) is being used"
//! (`smithy_query(query="shape",shape="NatGatewayAddress")`), so both members are facts of the
//! ELASTIC IP ADDRESS rather than of the gateway or of its subnet. An address that associates
//! no Elastic IP address — every address of a private NAT gateway, a private address assigned
//! beside a public gateway's Elastic IP ones, and an address whose Elastic IP has been given
//! up — reports neither member, exactly as it reports none of the `allocationId`,
//! `associationId` and `publicIp` members whose own docs read "[Public NAT gateway only]".
//!
//! So the invariant every row below is asserted against is a BICONDITIONAL: an address
//! reports the zone members if and only if it reports the `allocationId` of an Elastic IP
//! address, and where it does, the zone is the one the gateway's own subnet sits in.
//!
//! The rows differ along what the rule generalises over — the gateway's `ConnectivityType`,
//! whether an address holds an Elastic IP address, whether that address arrived with the
//! create or through `AssociateNatGatewayAddress` / `AssignPrivateNatGatewayAddress`, and
//! whether the Elastic IP has since been given up — and every row is asserted on BOTH the
//! answer of the call that made the address and the later `DescribeNatGateways` read, so the
//! two cannot disagree.

use std::time::{Duration, Instant};

use ec2::state::{CONNECTIVITY_TYPE_PRIVATE, CONNECTIVITY_TYPE_PUBLIC, NAT_GATEWAY_STATE_AVAILABLE};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The Availability Zone the subnets — and so the gateways — of this suite sit in.
const ZONE: &str = "us-east-1a";

/// How long a poll is given before it reports that the condition never held: generously past
/// the one transition span a gateway coming up takes, so a loaded machine fails nothing while
/// a condition that never holds still ends the test instead of hanging.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between reads, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// One `natGatewayAddressSet` item, as the members this rule is about: the Elastic IP
/// address's own id and the zone it is reported as being used in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAddress {
    allocation_id: Option<String>,
    private_ip: Option<String>,
    availability_zone: Option<String>,
    availability_zone_id: Option<String>,
}

/// Every `natGatewayAddressSet` item of `body`, in document order.
///
/// Read with the XML parser rather than by substring, so a member of any other element of the
/// response can never be read as one of a gateway's addresses — a subnet's own
/// `availabilityZone` is the case this rule would otherwise confuse.
fn reported_addresses(case: &str, body: &str) -> Vec<ReportedAddress> {
    let parsed = document(case, body);
    let addresses: Vec<ReportedAddress> = parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "natGatewayAddressSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .map(|item| ReportedAddress {
            allocation_id: child_text(item, "allocationId"),
            private_ip: child_text(item, "privateIp"),
            availability_zone: child_text(item, "availabilityZone"),
            availability_zone_id: child_text(item, "availabilityZoneId"),
        })
        .collect();
    assert!(
        !addresses.is_empty(),
        "case {case:?}: expected a reported NAT gateway address, got {body}"
    );
    addresses
}

/// The addresses `DescribeNatGateways` reports for `nat_gateway_id` — what a LATER caller is
/// told, as against what one call answered.
async fn described_addresses(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
) -> Vec<ReportedAddress> {
    let body = ok(
        case,
        router,
        "DescribeNatGateways",
        &[param("NatGatewayId.1", nat_gateway_id)],
    )
    .await;
    reported_addresses(case, &body)
}

/// The network one row's gateway sits in: a VPC and one subnet of its own, as the subnet's id
/// and the zone name and zone id that subnet reports for itself — the zone an Elastic IP
/// address of a gateway in it is used in.
async fn network(case: &str, router: &MultiServiceRouter, index: u8) -> (String, String, String) {
    let body = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("10.{index}.0.0/16"))],
    )
    .await;
    let vpc_id = member(case, &body, "vpcId");
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", &format!("10.{index}.1.0/24")),
            param("AvailabilityZone", ZONE),
        ],
    )
    .await;
    (
        member(case, &body, "subnetId"),
        member(case, &body, "availabilityZone"),
        member(case, &body, "availabilityZoneId"),
    )
}

/// One Elastic IP address out of Amazon's pool, as its allocation id.
async fn allocation(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "AllocateAddress", &[]).await;
    member(case, &body, "allocationId")
}

/// Poll `DescribeNatGateways` until the gateway is `available`, exactly as a client waiting
/// for its NAT gateway does: nothing but the clock moves it.
async fn poll_until_available(case: &str, router: &MultiServiceRouter, nat_gateway_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = ok(
            case,
            router,
            "DescribeNatGateways",
            &[param("NatGatewayId.1", nat_gateway_id)],
        )
        .await;
        let reported = element(&body, "state").unwrap_or_default();
        if reported == NAT_GATEWAY_STATE_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the NAT gateway {nat_gateway_id} is still in {reported:?} rather \
             than {NAT_GATEWAY_STATE_AVAILABLE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rule itself, over every address of one report: the zone members are reported if and
/// only if the address reports the `allocationId` of an Elastic IP address, and where they
/// are, they name the zone the gateway's subnet sits in.
fn assert_zone_follows_the_elastic_ip(
    case: &str,
    where_from: &str,
    addresses: &[ReportedAddress],
    zone: (&str, &str),
) {
    let (zone_name, zone_id) = zone;
    for address in addresses {
        let expected = match address.allocation_id.is_some() {
            true => (Some(zone_name.to_string()), Some(zone_id.to_string())),
            false => (None, None),
        };
        assert_eq!(
            (
                address.availability_zone.clone(),
                address.availability_zone_id.clone()
            ),
            expected,
            "case {case:?}: {where_from} must report the zone of an address exactly when the \
             address holds an Elastic IP address, got {address:?}"
        );
    }
}

/// One row of the table: how the gateway is made and which call adds an address to it after
/// the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A private NAT gateway, whose addresses hold no Elastic IP address at all.
    Private,
    /// A private NAT gateway given a second private address through
    /// `AssignPrivateNatGatewayAddress`.
    PrivateWithAssignedAddress,
    /// A public NAT gateway, created holding one Elastic IP address.
    Public,
    /// A public NAT gateway given a second Elastic IP address through
    /// `AssociateNatGatewayAddress`.
    PublicWithAssociatedAddress,
    /// A public NAT gateway created holding a SECOND Elastic IP address as well, so the same
    /// pair of addresses arrives through the create rather than through an association.
    PublicWithSecondaryAtCreate,
}

impl Row {
    fn case(self) -> &'static str {
        match self {
            Self::Private => "a private NAT gateway",
            Self::PrivateWithAssignedAddress => "a private NAT gateway with an assigned address",
            Self::Public => "a public NAT gateway",
            Self::PublicWithAssociatedAddress => {
                "a public NAT gateway with a second Elastic IP address"
            }
            Self::PublicWithSecondaryAtCreate => {
                "a public NAT gateway created with a secondary Elastic IP address"
            }
        }
    }

    /// `NatGateway$ConnectivityType` this row's gateway is created with.
    fn connectivity_type(self) -> &'static str {
        match self {
            Self::Private | Self::PrivateWithAssignedAddress => CONNECTIVITY_TYPE_PRIVATE,
            Self::Public
            | Self::PublicWithAssociatedAddress
            | Self::PublicWithSecondaryAtCreate => CONNECTIVITY_TYPE_PUBLIC,
        }
    }
}

/// Every row of the table, each in a network of its own.
const ROWS: [Row; 5] = [
    Row::Private,
    Row::PrivateWithAssignedAddress,
    Row::Public,
    Row::PublicWithAssociatedAddress,
    Row::PublicWithSecondaryAtCreate,
];

/// An address reports the Availability Zone of its Elastic IP address, and an address holding
/// none reports no zone — on the answer of the call that made it and on the read after it.
#[tokio::test]
async fn an_address_reports_a_zone_exactly_when_it_holds_an_elastic_ip() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case();
        let (subnet_id, zone_name, zone_id) = network(
            case,
            &router,
            u8::try_from(index).expect("one network per table row") + 1,
        )
        .await;
        let zone = (zone_name.as_str(), zone_id.as_str());

        let mut params = vec![
            param("SubnetId", &subnet_id),
            param("ConnectivityType", row.connectivity_type()),
        ];
        if row.connectivity_type() == CONNECTIVITY_TYPE_PUBLIC {
            let allocation_id = allocation(case, &router).await;
            params.push(param("AllocationId", &allocation_id));
        }
        if *row == Row::PublicWithSecondaryAtCreate {
            let secondary = allocation(case, &router).await;
            params.push(param("SecondaryAllocationId.1", &secondary));
        }
        let created = ok(case, &router, "CreateNatGateway", &params).await;
        let nat_gateway_id = member(case, &created, "natGatewayId");
        assert_zone_follows_the_elastic_ip(
            case,
            "CreateNatGateway",
            &reported_addresses(case, &created),
            zone,
        );

        // The address calls below need a gateway that is serving, which is a move the clock
        // owes the create.
        poll_until_available(case, &router, &nat_gateway_id).await;

        match row {
            Row::PrivateWithAssignedAddress => {
                let answered = ok(
                    case,
                    &router,
                    "AssignPrivateNatGatewayAddress",
                    &[
                        param("NatGatewayId", &nat_gateway_id),
                        param("PrivateIpAddressCount", "1"),
                    ],
                )
                .await;
                assert_zone_follows_the_elastic_ip(
                    case,
                    "AssignPrivateNatGatewayAddress",
                    &reported_addresses(case, &answered),
                    zone,
                );
            }
            Row::PublicWithAssociatedAddress => {
                let allocation_id = allocation(case, &router).await;
                let answered = ok(
                    case,
                    &router,
                    "AssociateNatGatewayAddress",
                    &[
                        param("NatGatewayId", &nat_gateway_id),
                        param("AllocationId.1", &allocation_id),
                    ],
                )
                .await;
                assert_zone_follows_the_elastic_ip(
                    case,
                    "AssociateNatGatewayAddress",
                    &reported_addresses(case, &answered),
                    zone,
                );
            }
            Row::Private | Row::Public | Row::PublicWithSecondaryAtCreate => {}
        }

        // The read AFTER the call answers the same thing about every address the gateway now
        // holds, and the row's own shape is what the report has to carry: a private gateway
        // holds no Elastic IP address at all, a public one holds at least its primary.
        let described = described_addresses(case, &router, &nat_gateway_id).await;
        assert_zone_follows_the_elastic_ip(case, "DescribeNatGateways", &described, zone);
        let holding_elastic_ips = described
            .iter()
            .filter(|address| address.allocation_id.is_some())
            .count();
        let expected_elastic_ips = match row {
            Row::Private | Row::PrivateWithAssignedAddress => 0,
            Row::Public => 1,
            Row::PublicWithAssociatedAddress | Row::PublicWithSecondaryAtCreate => 2,
        };
        assert_eq!(
            holding_elastic_ips, expected_elastic_ips,
            "case {case:?}: expected {expected_elastic_ips} address(es) holding an Elastic IP \
             address, got {described:?}"
        );
        // Every address of the gateway is a real one: each carries the private address it
        // handles traffic through, so no row's expectation is met by an empty projection.
        for address in &described {
            assert!(
                address
                    .private_ip
                    .as_deref()
                    .is_some_and(|private_ip| !private_ip.is_empty()),
                "case {case:?}: every address reports its private address, got {address:?}"
            );
        }
    }
}

/// An address whose Elastic IP address has been GIVEN UP reports no zone either: the member
/// does not outlive the Elastic IP it describes.
///
/// A delete is what gives the Elastic IP addresses of a public gateway up — the gateway stops
/// handling outbound NAT traffic from them — so the same gateway's addresses report the zone
/// before the delete and none after it.
#[tokio::test]
async fn an_address_reports_no_zone_once_its_elastic_ip_is_given_up() {
    let router = emulator();
    let case = "a deleted public NAT gateway";
    let (subnet_id, zone_name, zone_id) = network(case, &router, 9).await;
    let zone = (zone_name.as_str(), zone_id.as_str());
    let allocation_id = allocation(case, &router).await;
    let created = ok(
        case,
        &router,
        "CreateNatGateway",
        &[
            param("SubnetId", &subnet_id),
            param("ConnectivityType", CONNECTIVITY_TYPE_PUBLIC),
            param("AllocationId", &allocation_id),
        ],
    )
    .await;
    let nat_gateway_id = member(case, &created, "natGatewayId");
    poll_until_available(case, &router, &nat_gateway_id).await;

    let before = described_addresses(case, &router, &nat_gateway_id).await;
    assert_zone_follows_the_elastic_ip(case, "DescribeNatGateways", &before, zone);
    assert!(
        before
            .iter()
            .any(|address| address.availability_zone.is_some()),
        "case {case:?}: a serving public gateway reports the zone of its Elastic IP address, \
         got {before:?}"
    );

    ok(
        case,
        &router,
        "DeleteNatGateway",
        &[param("NatGatewayId", &nat_gateway_id)],
    )
    .await;

    let after = described_addresses(case, &router, &nat_gateway_id).await;
    assert_zone_follows_the_elastic_ip(case, "DescribeNatGateways after the delete", &after, zone);
    for address in &after {
        assert_eq!(
            (
                address.allocation_id.clone(),
                address.availability_zone.clone(),
                address.availability_zone_id.clone()
            ),
            (None, None, None),
            "case {case:?}: an address that has given up its Elastic IP address reports \
             neither the address nor its zone, got {address:?}"
        );
    }
}
