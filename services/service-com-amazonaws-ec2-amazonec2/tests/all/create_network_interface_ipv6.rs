// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the IPv6 space `CreateNetworkInterface` gives the interface it
//! creates, driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries
//! `Action=CreateNetworkInterface`, answered with XML.
//!
//! The rule under test is that A CREATE TAKES THE INTERFACE'S IPv6 ADDRESSES AND PREFIX
//! DELEGATION PREFIXES OUT OF THE IPv6 CIDR BLOCK ASSOCIATED WITH ITS SUBNET, AND A SUBNET
//! THAT HAS NO SUCH BLOCK HAS NOTHING TO ASSIGN: the request either names the addresses and
//! prefixes or says how many, each one lies inside the assignable part of that block, no two
//! interfaces of the account are handed the same address, what the create reported reads back
//! through `DescribeNetworkInterfaces` — and a request that asks for IPv6 space in a subnet
//! with no IPv6 range is refused with `InvalidParameterValue`, having created nothing.
//!
//! Every table row is a distinct input along what the rule generalises over — a named address
//! against a count of them, how many, addresses against prefixes, the primary-IPv6 flag, which
//! subnet the interface sits in — so no single call is the specification. The values the
//! SERVICE chooses are asserted as properties (inside the block, above its reserved addresses,
//! distinct from everything already handed out) rather than as fixed text: which free address
//! it picks is its own choice.

use std::net::Ipv6Addr;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The IPv6 block the fixture VPC brings from the account's own address space, so every block
/// in this suite is a known value rather than one the emulator minted.
const VPC_IPV6_BLOCK: &str = "2001:db8:a::/56";

/// The IPv6 address pool the fixture VPC's block is brought from.
const IPV6_POOL: &str = "ipv6pool-ec2-0123456789abcdef0";

/// The IPv6 block of the dual-stack subnet: a `/64` of [`VPC_IPV6_BLOCK`], the one published
/// subnet block size ([`ec2::state::SUBNET_IPV6_PREFIX_LENGTH`]).
const SUBNET_IPV6_BLOCK: &str = "2001:db8:a:1::/64";

/// One `action` call carrying `params`, as its HTTP status and XML body.
///
/// `pub(crate)`: the sibling suite that holds a REFUSED create to the pools it found
/// ([`super::create_network_interface_refusal_leaves_the_pools`]) drives the same surface
/// through the same harness rather than a second copy of it.
pub(crate) async fn call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
pub(crate) async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
pub(crate) fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The number an IPv6 address text names — the test's own arithmetic, so what the service
/// chose is judged against the block rather than against the service's own helpers.
fn address_number(case: &str, value: &str) -> u128 {
    u128::from(
        value
            .parse::<Ipv6Addr>()
            .unwrap_or_else(|_| panic!("case {case:?}: {value:?} must be an IPv6 address")),
    )
}

/// The first and last address of `block`, as numbers.
fn block_bounds(case: &str, block: &str) -> (u128, u128) {
    let (address, prefix_length) = block
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {block:?} must be a CIDR block"));
    let prefix_length: u32 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {block:?} must carry a prefix length"));
    let base = address_number(case, address);
    let size = 1u128 << (128 - prefix_length);
    (base, base + size - 1)
}

/// The fixtures every test here drives: the emulator, a subnet with an IPv6 CIDR block and one
/// without — "Amazon EC2 automatically selects the IPv6 addresses from the subnet range"
/// (`CreateNetworkInterfaceRequest$Ipv6AddressCount`), so a subnet with no IPv6 range is part
/// of the rule too.
pub(crate) struct Fixtures {
    pub(crate) router: MultiServiceRouter,
    /// The shared engine those calls are served from, so a test can read the pools the
    /// handler draws from off the store itself.
    pub(crate) engine: std::sync::Arc<engine::InMemoryEngine>,
    /// A subnet holding [`SUBNET_IPV6_BLOCK`].
    pub(crate) dual_stack_subnet: String,
    /// A subnet of the same VPC with no IPv6 block at all.
    pub(crate) ipv4_only_subnet: String,
}

/// A VPC with an IPv6 block, a subnet holding a `/64` of it, and a subnet holding none.
///
/// `pub(crate)` for the reason [`call`] is.
pub(crate) async fn fixtures(case: &str) -> Fixtures {
    let (router, engine) = super::ec2_query::emulator();
    let created = ok(
        case,
        &router,
        "CreateVpc",
        &[
            param("CidrBlock", "10.150.0.0/16"),
            param("Ipv6CidrBlock", VPC_IPV6_BLOCK),
            param("Ipv6Pool", IPV6_POOL),
        ],
    )
    .await;
    let vpc_id = member(case, &created, "vpcId");
    let dual_stack_subnet = create_subnet(case, &router, &vpc_id, "10.150.1.0/24").await;
    let ipv4_only_subnet = create_subnet(case, &router, &vpc_id, "10.150.2.0/24").await;
    let associated = ok(
        case,
        &router,
        "AssociateSubnetCidrBlock",
        &[
            param("SubnetId", &dual_stack_subnet),
            param("Ipv6CidrBlock", SUBNET_IPV6_BLOCK),
        ],
    )
    .await;
    let block = member(case, &associated, "ipv6CidrBlock");
    assert_eq!(
        block, SUBNET_IPV6_BLOCK,
        "case {case:?}: the fixture subnet must hold {SUBNET_IPV6_BLOCK}, got {block:?}"
    );
    Fixtures {
        router,
        engine,
        dual_stack_subnet,
        ipv4_only_subnet,
    }
}

/// A subnet of `vpc_id` with `cidr_block`, as its id.
async fn create_subnet(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", vpc_id), param("CidrBlock", cidr_block)],
    )
    .await;
    member(case, &body, "subnetId")
}

/// The form fields of one `Ipv6Addresses` entry: `Ipv6Addresses.N.Ipv6Address`, as ec2Query
/// flattens a list of structures under its `@ec2QueryName`
/// (`smithy_query(query="operation",operation="CreateNetworkInterface")`).
fn named_address(index: usize, address: &str) -> Param {
    param(&format!("Ipv6Addresses.{index}.Ipv6Address"), address)
}

/// The form fields of one `Ipv6Prefixes` entry. The member carries no `@ec2QueryName`, so
/// ec2Query spells it with its `@xmlName`: `Ipv6Prefix.N.Ipv6Prefix`.
fn named_prefix(index: usize, prefix: &str) -> Param {
    param(&format!("Ipv6Prefix.{index}.Ipv6Prefix"), prefix)
}

/// What one row asks for and what the interface it creates must then hold.
struct Row {
    /// The case name every assertion of the row names.
    case: &'static str,
    /// The members beside `SubnetId`.
    members: Vec<Param>,
    /// How many IPv6 addresses the interface must hold.
    addresses: usize,
    /// How many IPv6 prefixes it must hold.
    prefixes: usize,
    /// An address the request NAMED, which the interface must hold as named.
    named: Option<&'static str>,
    /// Whether the first IPv6 address must be reported as the primary one.
    primary: bool,
}

/// THE RULE: the IPv6 addresses and prefixes a create asks for come out of the subnet's own
/// IPv6 CIDR block, are recorded on the interface and read back through the describe.
#[tokio::test]
async fn an_interface_takes_its_ipv6_space_out_of_its_subnets_range() {
    let fixtures = fixtures("the fixtures").await;
    let (block_first, block_last) = block_bounds("the subnet block", SUBNET_IPV6_BLOCK);
    // "AWS reserves the first four addresses of a subnet's IPv6 CIDR block"
    // (`ec2::state::SUBNET_RESERVED_IPV6_ADDRESSES`), so no assignable address is below this.
    let first_assignable = block_first + ec2::state::SUBNET_RESERVED_IPV6_ADDRESSES;
    // A prefix of the size Prefix Delegation hands out
    // ([`ec2::state::IPV6_PREFIX_DELEGATION_PREFIX_LENGTH`]), inside the subnet's block and
    // above its reserved addresses.
    let delegated = format!(
        "2001:db8:a:1:9::/{}",
        ec2::state::IPV6_PREFIX_DELEGATION_PREFIX_LENGTH
    );

    let rows = vec![
        Row {
            case: "one address the service chooses",
            members: vec![param("Ipv6AddressCount", "1")],
            addresses: 1,
            prefixes: 0,
            named: None,
            primary: false,
        },
        Row {
            case: "three addresses the service chooses",
            members: vec![param("Ipv6AddressCount", "3")],
            addresses: 3,
            prefixes: 0,
            named: None,
            primary: false,
        },
        Row {
            case: "one address the request names",
            members: vec![named_address(1, "2001:db8:a:1::100")],
            addresses: 1,
            prefixes: 0,
            named: Some("2001:db8:a:1::100"),
            primary: false,
        },
        Row {
            case: "two addresses the request names",
            members: vec![
                named_address(1, "2001:db8:a:1::200"),
                named_address(2, "2001:db8:a:1::201"),
            ],
            addresses: 2,
            prefixes: 0,
            named: Some("2001:db8:a:1::201"),
            primary: false,
        },
        Row {
            case: "an address spelled in full",
            members: vec![named_address(
                1,
                "2001:0db8:000a:0001:0000:0000:0000:0300",
            )],
            addresses: 1,
            prefixes: 0,
            named: Some("2001:db8:a:1::300"),
            primary: false,
        },
        Row {
            case: "a primary IPv6 address and no count",
            members: vec![param("EnablePrimaryIpv6", "true")],
            addresses: 1,
            prefixes: 0,
            named: None,
            primary: true,
        },
        Row {
            case: "a primary IPv6 address beside two the service chooses",
            members: vec![
                param("EnablePrimaryIpv6", "true"),
                param("Ipv6AddressCount", "2"),
            ],
            addresses: 2,
            prefixes: 0,
            named: None,
            primary: true,
        },
        Row {
            case: "one prefix the service delegates",
            members: vec![param("Ipv6PrefixCount", "1")],
            addresses: 0,
            prefixes: 1,
            named: None,
            primary: false,
        },
        Row {
            case: "two prefixes the service delegates",
            members: vec![param("Ipv6PrefixCount", "2")],
            addresses: 0,
            prefixes: 2,
            named: None,
            primary: false,
        },
        Row {
            case: "a prefix the request names",
            members: vec![named_prefix(1, &delegated)],
            addresses: 0,
            prefixes: 1,
            named: None,
            primary: false,
        },
        Row {
            case: "no IPv6 space at all",
            members: Vec::new(),
            addresses: 0,
            prefixes: 0,
            named: None,
            primary: false,
        },
    ];

    // Every address and every prefix this suite has handed out, so no two interfaces of the
    // account are given the same space.
    let mut handed_out: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let mut params = vec![param("SubnetId", &fixtures.dual_stack_subnet)];
        params.extend(row.members.clone());
        let created = ok(case, &fixtures.router, "CreateNetworkInterface", &params).await;
        let network_interface_id = member(case, &created, "networkInterfaceId");

        for body in [
            created.clone(),
            described(case, &fixtures.router, &network_interface_id).await,
        ] {
            let addresses = elements(&body, "ipv6Address");
            let prefixes = elements(&body, "ipv6Prefix");
            assert_eq!(
                addresses.len(),
                row.addresses,
                "case {case:?}: expected {} IPv6 addresses, got {addresses:?} from {body}",
                row.addresses
            );
            assert_eq!(
                prefixes.len(),
                row.prefixes,
                "case {case:?}: expected {} IPv6 prefixes, got {prefixes:?} from {body}",
                row.prefixes
            );
            for address in &addresses {
                let number = address_number(case, address);
                assert!(
                    (first_assignable..=block_last).contains(&number),
                    "case {case:?}: expected {address:?} inside the assignable part of \
                     {SUBNET_IPV6_BLOCK}, got {address:?}"
                );
            }
            for prefix in &prefixes {
                let (first, last) = block_bounds(case, prefix);
                assert!(
                    first >= first_assignable && last <= block_last,
                    "case {case:?}: expected the prefix {prefix:?} inside the assignable \
                     part of {SUBNET_IPV6_BLOCK}, got {first}..={last}"
                );
            }
            if let Some(named) = row.named {
                assert!(
                    addresses.iter().any(|address| address == named),
                    "case {case:?}: expected the named address {named:?} among \
                     {addresses:?}"
                );
            }
            let primary = element(&body, "isPrimaryIpv6");
            assert_eq!(
                primary.as_deref() == Some("true"),
                row.primary,
                "case {case:?}: expected a primary IPv6 verdict of {}, got {primary:?} \
                 from {body}",
                row.primary
            );
        }

        for space in elements(&created, "ipv6Address")
            .into_iter()
            .chain(elements(&created, "ipv6Prefix"))
        {
            assert!(
                !handed_out.contains(&space),
                "case {case:?}: {space:?} was already handed out: {handed_out:?}"
            );
            handed_out.push(space);
        }
    }
    assert!(
        handed_out.len() > 10,
        "the rows above must have handed out space to assert on, got {handed_out:?}"
    );
}

/// THE OTHER HALF OF THE RULE: a subnet with no IPv6 CIDR block has nothing to assign, so
/// every way of asking for IPv6 space in one is refused — and nothing is created.
#[tokio::test]
async fn an_ipv6_request_in_a_subnet_with_no_ipv6_range_is_refused() {
    let fixtures = fixtures("the fixtures").await;
    let delegated = format!(
        "2001:db8:a:1:2::/{}",
        ec2::state::IPV6_PREFIX_DELEGATION_PREFIX_LENGTH
    );

    // (case, the members beside SubnetId) — one row per way of asking for IPv6 space.
    let rows: Vec<(&str, Vec<Param>)> = vec![
        ("a count of addresses", vec![param("Ipv6AddressCount", "1")]),
        (
            "a named address",
            vec![named_address(1, "2001:db8:a:1::100")],
        ),
        ("a count of prefixes", vec![param("Ipv6PrefixCount", "1")]),
        ("a named prefix", vec![named_prefix(1, &delegated)]),
        (
            "a primary IPv6 address",
            vec![param("EnablePrimaryIpv6", "true")],
        ),
    ];

    let before = described_ids(&fixtures.router).await;
    for (case, members) in rows {
        let mut params = vec![param("SubnetId", &fixtures.ipv4_only_subnet)];
        params.extend(members);
        let (status, body) = call(&fixtures.router, "CreateNetworkInterface", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        assert_eq!(
            error_message(&body),
            "Subnet does not contain any IPv6 CIDR block ranges",
            "case {case:?}: expected the sentence EC2 words for this, got {body}"
        );
    }
    let after = described_ids(&fixtures.router).await;
    assert_eq!(
        after, before,
        "the refused requests must have created no interface"
    );
}

/// The combination rules the four IPv6 members and the primary-IPv6 flag carry, and the
/// address rules the subnet's block imposes: each refused with the code that says why, with
/// nothing created.
#[tokio::test]
async fn an_ipv6_request_the_subnet_or_the_members_cannot_serve_is_refused() {
    let fixtures = fixtures("the fixtures").await;
    let delegated = format!(
        "2001:db8:a:1:2::/{}",
        ec2::state::IPV6_PREFIX_DELEGATION_PREFIX_LENGTH
    );
    // One address already held by an interface of the account, for the in-use row.
    let held = {
        let created = ok(
            "the held address",
            &fixtures.router,
            "CreateNetworkInterface",
            &[
                param("SubnetId", &fixtures.dual_stack_subnet),
                named_address(1, "2001:db8:a:1::900"),
            ],
        )
        .await;
        member("the held address", &created, "ipv6Address")
    };
    assert_eq!(
        held, "2001:db8:a:1::900",
        "the row below needs the address the create above took"
    );

    // (case, the members beside SubnetId, the code the refusal carries)
    let rows: Vec<(&str, Vec<Param>, &str)> = vec![
        (
            "named addresses beside a count of them",
            vec![
                named_address(1, "2001:db8:a:1::400"),
                param("Ipv6AddressCount", "1"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "named prefixes beside a count of them",
            vec![named_prefix(1, &delegated), param("Ipv6PrefixCount", "1")],
            "InvalidParameterCombination",
        ),
        (
            "addresses beside prefixes",
            vec![
                param("Ipv6AddressCount", "1"),
                param("Ipv6PrefixCount", "1"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "an address count below zero",
            vec![param("Ipv6AddressCount", "-1")],
            "InvalidParameterValue",
        ),
        (
            "a prefix count below zero",
            vec![param("Ipv6PrefixCount", "-2")],
            "InvalidParameterValue",
        ),
        (
            "more addresses than one interface holds",
            vec![param(
                "Ipv6AddressCount",
                &(ec2::state::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE + 1).to_string(),
            )],
            "InvalidParameterValue",
        ),
        (
            "an address outside the subnet's block",
            vec![named_address(1, "2001:db8:a:2::1")],
            "InvalidParameterValue",
        ),
        (
            "one of the addresses the subnet reserves",
            vec![named_address(1, "2001:db8:a:1::")],
            "InvalidParameterValue",
        ),
        (
            "a value that is no IPv6 address",
            vec![named_address(1, "10.150.1.5")],
            "InvalidParameterValue",
        ),
        (
            "an address another interface already holds",
            vec![named_address(1, &held)],
            "InvalidIPAddress.InUse",
        ),
        (
            "one address named twice",
            vec![
                named_address(1, "2001:db8:a:1::500"),
                named_address(2, "2001:db8:a:1::500"),
            ],
            "InvalidIPAddress.InUse",
        ),
        (
            "a prefix outside the subnet's block",
            vec![named_prefix(
                1,
                &format!(
                    "2001:db8:a:9::/{}",
                    ec2::state::IPV6_PREFIX_DELEGATION_PREFIX_LENGTH
                ),
            )],
            "InvalidParameterValue",
        ),
        (
            "IPv6 space on an interface type that carries no addresses",
            vec![
                param("InterfaceType", "efa-only"),
                param("Ipv6AddressCount", "1"),
            ],
            "InvalidParameterCombination",
        ),
    ];

    let before = described_ids(&fixtures.router).await;
    for (case, members, expected) in rows {
        let mut params = vec![param("SubnetId", &fixtures.dual_stack_subnet)];
        params.extend(members);
        let (status, body) = call(&fixtures.router, "CreateNetworkInterface", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            expected,
            "case {case:?}: expected the code {expected:?}, got {body}"
        );
    }
    let after = described_ids(&fixtures.router).await;
    assert_eq!(
        after, before,
        "the refused requests must have created no interface"
    );
}

/// The body `DescribeNetworkInterfaces` answers for the one interface `network_interface_id`
/// names.
async fn described(case: &str, router: &MultiServiceRouter, network_interface_id: &str) -> String {
    ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &[param("NetworkInterfaceId.1", network_interface_id)],
    )
    .await
}

/// Every network interface id the account holds, as the describe reports them.
async fn described_ids(router: &MultiServiceRouter) -> Vec<String> {
    let body = ok(
        "the interfaces the account holds",
        router,
        "DescribeNetworkInterfaces",
        &[],
    )
    .await;
    elements(&body, "networkInterfaceId")
}
