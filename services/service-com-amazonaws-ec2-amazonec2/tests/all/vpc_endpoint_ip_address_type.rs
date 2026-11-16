// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS FAMILIES a VPC endpoint is reached over, driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: "the IP address type for the endpoint" (`IpAddressType`) names the
//! address families the endpoint's own endpoint network interfaces carry addresses of, and
//! each interface takes its addresses out of the CIDR blocks of the SUBNET it sits in — so
//! every subnet the endpoint is created in has to have a block of each of those families, and
//! one that does not is refused with `InvalidSubnet` naming the subnet and the address type.
//! An endpoint the request serves then holds, per subnet, an IPv4 address for a type that
//! includes IPv4 and an IPv6 address out of that subnet's IPv6 range for one that includes
//! IPv6.
//!
//! The rows differ along what that generalises over: which of the three address types the
//! request names, whether the subnet has an IPv6 range, and whether the request names the
//! interface's address itself. The addresses the SERVICE chooses are asserted as properties —
//! inside the subnet's block — rather than as fixed text.

use std::net::Ipv6Addr;

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The endpoint service every endpoint here connects to: an interface endpoint service of the
/// Region, since only an interface endpoint has network interfaces to carry addresses.
const SERVICE: &str = "com.amazonaws.us-east-1.logs";

/// The IPv6 block the fixture VPC brings from the account's own address space.
const VPC_IPV6_BLOCK: &str = "2001:db8:b::/56";

/// The IPv6 address pool the fixture VPC's block is brought from.
const IPV6_POOL: &str = "ipv6pool-ec2-0123456789abcdef0";

/// The IPv6 block of the dual-stack subnet: a `/64` of [`VPC_IPV6_BLOCK`].
const SUBNET_IPV6_BLOCK: &str = "2001:db8:b:1::/64";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
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

/// The text of every `<item>` of the first element named `set` — how ec2Query renders a list
/// member such as `networkInterfaceIdSet`.
fn set_texts(case: &str, body: &str, set: &str) -> Vec<String> {
    let parsed = super::wire::document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == set)
        .map(|node| {
            node.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .map(|item| item.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The one endpoint network interface id `body` reports, or a panic naming the case.
fn one_interface(case: &str, body: &str) -> String {
    let ids = set_texts(case, body, "networkInterfaceIdSet");
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: expected exactly one endpoint network interface, got {ids:?} in {body}"
    );
    ids[0].clone()
}

/// The fixtures every test here drives.
struct Fixtures {
    router: MultiServiceRouter,
    vpc_id: String,
    /// A subnet with both an IPv4 and an IPv6 range.
    dual_stack_subnet: String,
    /// A subnet of the same VPC with an IPv4 range only.
    ipv4_only_subnet: String,
}

/// A VPC with an IPv6 block, a subnet holding a `/64` of it, and a subnet holding none.
async fn fixtures(case: &str) -> Fixtures {
    let (router, _engine) = super::ec2_query::emulator();
    let created = ok(
        case,
        &router,
        "CreateVpc",
        &[
            param("CidrBlock", "10.160.0.0/16"),
            param("Ipv6CidrBlock", VPC_IPV6_BLOCK),
            param("Ipv6Pool", IPV6_POOL),
        ],
    )
    .await;
    let vpc_id = member(case, &created, "vpcId");
    // A VPC you create has DNS hostnames OFF, and an interface endpoint's private hosted zone
    // needs them ON — so the fixture turns them on, leaving every row free to be about the
    // endpoint's address type rather than about the VPC's DNS attributes.
    ok(
        case,
        &router,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
    let dual_stack_subnet = subnet(case, &router, &vpc_id, "10.160.1.0/24", "us-east-1a").await;
    let ipv4_only_subnet = subnet(case, &router, &vpc_id, "10.160.2.0/24", "us-east-1b").await;
    ok(
        case,
        &router,
        "AssociateSubnetCidrBlock",
        &[
            param("SubnetId", &dual_stack_subnet),
            param("Ipv6CidrBlock", SUBNET_IPV6_BLOCK),
        ],
    )
    .await;
    Fixtures {
        router,
        vpc_id,
        dual_stack_subnet,
        ipv4_only_subnet,
    }
}

/// A subnet of `vpc_id` in `zone`, as its id.
async fn subnet(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
    zone: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", vpc_id),
            param("CidrBlock", cidr_block),
            param("AvailabilityZone", zone),
        ],
    )
    .await;
    member(case, &body, "subnetId")
}

/// Whether `address` lies inside the IPv6 block `block` — the test's own arithmetic.
fn inside_ipv6_block(case: &str, block: &str, address: &str) -> bool {
    let (base, prefix_length) = block
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {block:?} must be a CIDR block"));
    let prefix_length: u32 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {block:?} must carry a prefix length"));
    let number = |value: &str| -> u128 {
        u128::from(
            value
                .parse::<Ipv6Addr>()
                .unwrap_or_else(|_| panic!("case {case:?}: {value:?} must be an IPv6 address")),
        )
    };
    let first = number(base);
    let last = first + (1u128 << (128 - prefix_length)) - 1;
    (first..=last).contains(&number(address))
}

/// THE RULE: an endpoint of each address type its subnet serves is created, and its interface
/// holds an address of each family that type names.
#[tokio::test]
async fn an_endpoints_interfaces_carry_an_address_of_each_family_its_type_names() {
    // (case, the IpAddressType the request names or None for the default, whether the subnet
    // must be the dual-stack one, whether the interface must hold an IPv4 address, whether it
    // must hold an IPv6 address)
    let rows: Vec<(&str, Option<&str>, bool, bool, bool)> = vec![
        ("the default address type", None, false, true, false),
        ("ipv4 in an IPv4-only subnet", Some("ipv4"), false, true, false),
        ("ipv4 in a dual-stack subnet", Some("ipv4"), true, true, false),
        ("dualstack in a dual-stack subnet", Some("dualstack"), true, true, true),
        ("ipv6 in a dual-stack subnet", Some("ipv6"), true, false, true),
    ];

    for (case, ip_address_type, dual_stack, wants_ipv4, wants_ipv6) in rows {
        let fixtures = fixtures(case).await;
        let subnet_id = if dual_stack {
            fixtures.dual_stack_subnet.clone()
        } else {
            fixtures.ipv4_only_subnet.clone()
        };
        let mut params = vec![
            param("VpcId", &fixtures.vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &subnet_id),
        ];
        if let Some(ip_address_type) = ip_address_type {
            params.push(param("IpAddressType", ip_address_type));
        }
        let created = ok(case, &fixtures.router, "CreateVpcEndpoint", &params).await;
        let endpoint_id = member(case, &created, "vpcEndpointId");
        let reported_type = member(case, &created, "ipAddressType");
        assert_eq!(
            reported_type,
            ip_address_type.unwrap_or("ipv4"),
            "case {case:?}: expected the endpoint to report its address type, got {created}"
        );

        // The endpoint reads back through the describe with that type, and its interface
        // holds the addresses the type names.
        let described = ok(
            case,
            &fixtures.router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[&endpoint_id]),
        )
        .await;
        assert_eq!(
            element(&described, "ipAddressType").as_deref(),
            Some(reported_type.as_str()),
            "case {case:?}: expected the describe to report {reported_type:?}, got {described}"
        );
        let network_interface_id = one_interface(case, &described);
        let interface = ok(
            case,
            &fixtures.router,
            "DescribeNetworkInterfaces",
            &indexed("NetworkInterfaceId", &[&network_interface_id]),
        )
        .await;
        let private = element(&interface, "privateIpAddress");
        assert_eq!(
            private.is_some(),
            wants_ipv4,
            "case {case:?}: expected an IPv4 address on the interface: {wants_ipv4}, got \
             {private:?} from {interface}"
        );
        let ipv6 = elements(&interface, "ipv6Address");
        assert_eq!(
            ipv6.len(),
            usize::from(wants_ipv6),
            "case {case:?}: expected {} IPv6 address(es) on the interface, got {ipv6:?} \
             from {interface}",
            usize::from(wants_ipv6)
        );
        for address in &ipv6 {
            assert!(
                inside_ipv6_block(case, SUBNET_IPV6_BLOCK, address),
                "case {case:?}: expected {address:?} inside {SUBNET_IPV6_BLOCK}"
            );
        }
    }
}

/// THE OTHER HALF: a subnet with no block of a family the type names cannot hold one of the
/// endpoint's interfaces, so the create is refused and stores nothing.
#[tokio::test]
async fn a_subnet_without_the_types_address_family_is_refused() {
    let fixtures = fixtures("the fixtures").await;
    for ip_address_type in ["ipv6", "dualstack"] {
        let case = format!("{ip_address_type} in a subnet with no IPv6 range");
        let before = elements(
            &ok(&case, &fixtures.router, "DescribeVpcEndpoints", &[]).await,
            "vpcEndpointId",
        );
        let (status, body) = call(
            &fixtures.router,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &fixtures.vpc_id),
                param("ServiceName", SERVICE),
                param("VpcEndpointType", "Interface"),
                param("SubnetId.1", &fixtures.ipv4_only_subnet),
                param("IpAddressType", ip_address_type),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            "InvalidSubnet",
            "case {case:?}: expected InvalidSubnet, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "Subnet {} is not compatible with {ip_address_type} addressType.",
                fixtures.ipv4_only_subnet
            ),
            "case {case:?}: expected the sentence naming the subnet and the type, got {body}"
        );
        let after = elements(
            &ok(&case, &fixtures.router, "DescribeVpcEndpoints", &[]).await,
            "vpcEndpointId",
        );
        assert_eq!(
            after, before,
            "case {case:?}: the refusal must have created no endpoint"
        );
    }
}

/// An address the request names for a subnet is the address that subnet's interface holds —
/// and one of a family the endpoint is not reached over names an address no interface of it
/// would hold, which is refused.
#[tokio::test]
async fn an_address_the_request_names_is_held_only_for_a_family_the_type_covers() {
    let fixtures = fixtures("the fixtures").await;
    let case = "an IPv6 address the request names for the subnet";
    let created = ok(
        case,
        &fixtures.router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &fixtures.vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &fixtures.dual_stack_subnet),
            param("IpAddressType", "dualstack"),
            param("SubnetConfiguration.1.SubnetId", &fixtures.dual_stack_subnet),
            param("SubnetConfiguration.1.Ipv6", "2001:db8:b:1::50"),
        ],
    )
    .await;
    let network_interface_id = one_interface(case, &created);
    let interface = ok(
        case,
        &fixtures.router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[&network_interface_id]),
    )
    .await;
    assert_eq!(
        element(&interface, "ipv6Address").as_deref(),
        Some("2001:db8:b:1::50"),
        "case {case:?}: expected the interface to hold the address the request named, got \
         {interface}"
    );

    let case = "an IPv6 address for an endpoint reached over IPv4 alone";
    let (status, body) = call(
        &fixtures.router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &fixtures.vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &fixtures.dual_stack_subnet),
            param("SubnetConfiguration.1.SubnetId", &fixtures.dual_stack_subnet),
            param("SubnetConfiguration.1.Ipv6", "2001:db8:b:1::60"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        "InvalidParameterCombination",
        "case {case:?}: expected InvalidParameterCombination, got {body}"
    );

    let case = "an IPv4 address for an endpoint reached over IPv6 alone";
    let (status, body) = call(
        &fixtures.router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &fixtures.vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &fixtures.dual_stack_subnet),
            param("IpAddressType", "ipv6"),
            param("SubnetConfiguration.1.SubnetId", &fixtures.dual_stack_subnet),
            param("SubnetConfiguration.1.Ipv4", "10.160.1.50"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        "InvalidParameterCombination",
        "case {case:?}: expected InvalidParameterCombination, got {body}"
    );
}

/// A subnet ADDED to an endpoint later is held to the same rule, and the interface it gets
/// carries the same families: `ModifyVpcEndpoint` is the other writer of an endpoint's
/// interfaces.
#[tokio::test]
async fn a_subnet_added_later_is_held_to_the_endpoints_address_type() {
    let case = "a dual-stack endpoint in one subnet";
    let fixtures = fixtures(case).await;
    let third = subnet(
        case,
        &fixtures.router,
        &fixtures.vpc_id,
        "10.160.3.0/24",
        "us-east-1c",
    )
    .await;
    ok(
        case,
        &fixtures.router,
        "AssociateSubnetCidrBlock",
        &[
            param("SubnetId", &third),
            param("Ipv6CidrBlock", "2001:db8:b:3::/64"),
        ],
    )
    .await;
    let created = ok(
        case,
        &fixtures.router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &fixtures.vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &fixtures.dual_stack_subnet),
            param("IpAddressType", "dualstack"),
        ],
    )
    .await;
    let endpoint_id = member(case, &created, "vpcEndpointId");

    // A subnet with no IPv6 range cannot hold an interface of this endpoint.
    let case = "an IPv4-only subnet added to a dual-stack endpoint";
    let (status, body) = call(
        &fixtures.router,
        "ModifyVpcEndpoint",
        &[
            param("VpcEndpointId", &endpoint_id),
            param("AddSubnetId.1", &fixtures.ipv4_only_subnet),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        "InvalidSubnet",
        "case {case:?}: expected InvalidSubnet, got {body}"
    );

    // A subnet that has one does, and its interface holds an IPv6 address out of that
    // subnet's own range.
    let case = "a dual-stack subnet added to a dual-stack endpoint";
    ok(
        case,
        &fixtures.router,
        "ModifyVpcEndpoint",
        &[
            param("VpcEndpointId", &endpoint_id),
            param("AddSubnetId.1", &third),
        ],
    )
    .await;
    let described = ok(
        case,
        &fixtures.router,
        "DescribeNetworkInterfaces",
        &super::ec2_query::filters(&[("subnet-id", &[third.as_str()])]),
    )
    .await;
    let addresses = elements(&described, "ipv6Address");
    assert_eq!(
        addresses.len(),
        1,
        "case {case:?}: expected the added interface to hold one IPv6 address, got \
         {addresses:?} from {described}"
    );
    assert!(
        inside_ipv6_block(case, "2001:db8:b:3::/64", &addresses[0]),
        "case {case:?}: expected {:?} inside the added subnet's own range",
        addresses[0]
    );
}
