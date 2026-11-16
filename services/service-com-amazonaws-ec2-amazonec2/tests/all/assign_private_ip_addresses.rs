// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `AssignPrivateIpAddresses`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=AssignPrivateIpAddresses`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2` signing name in the
//! request's SigV4 credential scope, so every call here carries one.
//!
//! The rule under test is that AN ASSIGNMENT TAKES SECONDARY PRIVATE IPv4 ADDRESSES OUT OF
//! THE IPv4 CIDR BLOCK OF THE SUBNET THE NAMED NETWORK INTERFACE SITS IN AND RECORDS THEM
//! ON THAT INTERFACE BESIDE THE ONES IT ALREADY HELD: the request either names them or says
//! how many, each one lies inside the assignable part of that block, no two resources of
//! the account are handed the same address, and what a call assigned reads back through
//! `DescribeNetworkInterfaces` and comes off the subnet's `AvailableIpAddressCount`.
//! Everything else is refused with the code that says why, and nothing is recorded.
//!
//! Every table row is a distinct input along what that generalises over — named addresses
//! against a count of them, how many, where in the block the address sits, an address the
//! interface already holds against one another interface holds — so no single recorded call
//! is the specification. The values the SERVICE chooses are asserted as properties (inside
//! the block, assignable, distinct from everything already handed out) rather than as fixed
//! text: which free address it picks is its own choice.

use std::net::Ipv4Addr;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{Param, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The block the fixture VPC holds, out of which every subnet in this suite takes a `/24`
/// or a `/28`.
const VPC_BLOCK: &str = "10.80.0.0/16";

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

/// A VPC holding [`VPC_BLOCK`], as its id — the one every subnet of a test is cut out of.
async fn fixture_vpc(case: &str, router: &MultiServiceRouter) -> String {
    let created = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", VPC_BLOCK)],
    )
    .await;
    member(case, &created, "vpcId")
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

/// A network interface in `subnet_id`, as its id. `primary_address` is the primary private
/// IPv4 address the create NAMES, so a row's own fixture holds a known address and every
/// other address of its block is free for the row to name.
async fn create_interface(
    case: &str,
    router: &MultiServiceRouter,
    subnet_id: &str,
    primary_address: Option<&str>,
) -> String {
    let mut params = vec![param("SubnetId", subnet_id)];
    if let Some(address) = primary_address {
        params.push(param("PrivateIpAddress", address));
    }
    let body = ok(case, router, "CreateNetworkInterface", &params).await;
    member(case, &body, "networkInterfaceId")
}

/// The form fields of one `AssignPrivateIpAddresses` request: the addresses it names, or how
/// many secondary addresses it asks the service to choose, and whether it allows an address
/// something else holds to be reassigned. A zero count is left out, since a request either
/// names or counts.
///
/// `PrivateIpAddresses` carries the `@ec2QueryName` `PrivateIpAddress`, so ec2Query spells
/// the list `PrivateIpAddress.N`
/// (`smithy_query(query="operation",operation="AssignPrivateIpAddresses")`).
fn assign_params(
    network_interface_id: &str,
    named: &[&str],
    count: i32,
    allow_reassignment: bool,
) -> Vec<Param> {
    let mut params = vec![param("NetworkInterfaceId", network_interface_id)];
    params.extend(indexed("PrivateIpAddress", named));
    if count != 0 {
        params.push(param(
            "SecondaryPrivateIpAddressCount",
            &count.to_string(),
        ));
    }
    if allow_reassignment {
        params.push(param("AllowReassignment", "true"));
    }
    params
}

/// The addresses an `AssignPrivateIpAddresses` response says it assigned, in the order it
/// reported them.
fn answered_addresses(case: &str, body: &str) -> Vec<String> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "assignedPrivateIpAddressesSet")
        .into_iter()
        .filter_map(|item| child_text(item, "privateIpAddress"))
        .collect()
}

/// The one interface `network_interface_id` names, as `DescribeNetworkInterfaces` reports
/// it — what a LATER CALLER sees, read through the one implemented reader of these records.
async fn described_interface(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> String {
    ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[network_interface_id]),
    )
    .await
}

/// The private IPv4 addresses `DescribeNetworkInterfaces` reports for one interface, paired
/// with whether each is reported as the interface's primary address.
async fn described_addresses(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> Vec<(String, bool)> {
    let body = described_interface(router, case, network_interface_id).await;
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "networkInterfaceId").as_deref() == Some(network_interface_id)
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no {network_interface_id} in the describe body {body}")
        });
    set_items(interface, "privateIpAddressesSet")
        .into_iter()
        .filter_map(|item| {
            let address = child_text(item, "privateIpAddress")?;
            let primary = child_text(item, "primary").as_deref() == Some("true");
            Some((address, primary))
        })
        .collect()
}

/// Just the addresses of [`described_addresses`], in report order.
async fn described_address_list(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> Vec<String> {
    described_addresses(router, case, network_interface_id)
        .await
        .into_iter()
        .map(|(address, _)| address)
        .collect()
}

/// "The number of unused private IPv4 addresses in the subnet"
/// (`Subnet$AvailableIpAddressCount`) — the count an assignment has to take its addresses
/// out of.
async fn available_addresses(router: &MultiServiceRouter, case: &str, subnet_id: &str) -> i64 {
    let body = ok(
        case,
        router,
        "DescribeSubnets",
        &indexed("SubnetId", &[subnet_id]),
    )
    .await;
    member(case, &body, "availableIpAddressCount")
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: availableIpAddressCount must be a number"))
}

/// Every address of `block` a network interface may be given, as the numbers of its first
/// and last — the test's own arithmetic, so what the service chose is judged against the
/// block rather than against the service's own helpers. "AWS reserves five IP addresses in
/// each subnet CIDR block" (`ec2::state::SUBNET_RESERVED_ADDRESSES`): the first four and the
/// last.
fn assignable_bounds(case: &str, block: &str) -> (u32, u32) {
    let (address, prefix_length) = block
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {block:?} must be a CIDR block"));
    let prefix_length: u32 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {block:?} must carry a prefix length"));
    let base = u32::from(
        address
            .parse::<Ipv4Addr>()
            .unwrap_or_else(|_| panic!("case {case:?}: {address:?} must be an IPv4 address")),
    );
    let size = 1u32 << (32 - prefix_length);
    (
        base + ec2::state::SUBNET_RESERVED_ADDRESSES - 1,
        base + size - 2,
    )
}

/// Every address the assignment answered is one of `block`'s assignable addresses.
fn assert_assignable(case: &str, block: &str, addresses: &[String]) {
    let (lowest, highest) = assignable_bounds(case, block);
    for address in addresses {
        let value = u32::from(
            address
                .parse::<Ipv4Addr>()
                .unwrap_or_else(|_| panic!("case {case:?}: {address:?} must be an IPv4 address")),
        );
        assert!(
            (lowest..=highest).contains(&value),
            "case {case:?}: expected an assignable address of {block}, got {address:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What an assignment records
// ---------------------------------------------------------------------------

/// One row of the assignment table: the case, the addresses the request names, the count it
/// asks for, the addresses expected in the answer — empty when the SERVICE chooses them,
/// which is asserted as a property instead — and how many of them are NEW to the interface.
type AssignmentRow = (
    &'static str,
    Vec<&'static str>,
    i32,
    Vec<&'static str>,
    usize,
);

/// The rule: the addresses a request names, or the number of them it asks for, are taken out
/// of the subnet's IPv4 block and recorded on the interface as SECONDARY addresses — so the
/// call answers exactly what it assigned, the interface keeps the primary address it already
/// had, a later `DescribeNetworkInterfaces` reports the union, and the subnet's
/// `AvailableIpAddressCount` is down by what the call took.
///
/// The rows differ along what the rule generalises over: named addresses against a count,
/// how many of each, where in the block the address sits (the lowest assignable address, the
/// highest one, and addresses in between), and an address the interface ALREADY holds, which
/// is assigned to it already and so is reported without being recorded twice. Each row gets
/// a subnet and an interface of its own, so a row's named addresses are free addresses of
/// its own block rather than ones another row was handed.
#[tokio::test]
async fn an_assignment_takes_private_addresses_out_of_the_subnet_block_and_round_trips() {
    let case = "assignment";
    let (router, _engine) = super::ec2_query::emulator();
    let router = &router;
    let vpc_id = fixture_vpc(case, router).await;
    let ceiling = i32::try_from(ec2::state::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE)
        .expect("the per-interface ceiling fits an i32");

    let cases: Vec<AssignmentRow> = vec![
        (
            "the lowest assignable address of the block",
            vec!["10.80.1.4"],
            0,
            vec!["10.80.1.4"],
            1,
        ),
        (
            "the highest assignable address of the block",
            vec!["10.80.2.254"],
            0,
            vec!["10.80.2.254"],
            1,
        ),
        (
            "one address the request names",
            vec!["10.80.3.5"],
            0,
            vec!["10.80.3.5"],
            1,
        ),
        (
            "three addresses the request names",
            vec!["10.80.4.6", "10.80.4.7", "10.80.4.200"],
            0,
            vec!["10.80.4.6", "10.80.4.7", "10.80.4.200"],
            3,
        ),
        ("one address the service chooses", vec![], 1, vec![], 1),
        ("three addresses the service chooses", vec![], 3, vec![], 3),
        (
            // The per-interface bound this crate publishes
            // (`ec2::state::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE`), reached exactly: the
            // interface already holds its primary address, so one less than the ceiling is
            // what a call may still add.
            "the per-interface maximum, all chosen",
            vec![],
            ceiling - 1,
            vec![],
            usize::try_from(ceiling - 1).expect("a positive count"),
        ),
        (
            // An assignment is idempotent in the address it assigns: the interface's own
            // primary address is already assigned TO IT, so the call reports it and records
            // nothing new.
            "an address the interface already holds",
            vec!["10.80.8.100"],
            0,
            vec!["10.80.8.100"],
            0,
        ),
    ];

    // Every address this suite has handed out, so no row can be given one another row holds.
    let mut handed_out: Vec<String> = Vec::new();

    for (index, (row, named, count, expected, new_addresses)) in cases.into_iter().enumerate() {
        // A block of its own per row, so a named address is a free address of that block.
        let block = format!("10.80.{}.0/24", index + 1);
        let subnet = create_subnet(row, router, &vpc_id, &block).await;
        let primary = format!("10.80.{}.100", index + 1);
        let interface = create_interface(row, router, &subnet, Some(&primary)).await;
        let available_before = available_addresses(router, row, &subnet).await;

        let body = ok(
            row,
            router,
            "AssignPrivateIpAddresses",
            &assign_params(&interface, &named, count, false),
        )
        .await;

        let reported_interface = member(row, &body, "networkInterfaceId");
        assert_eq!(
            reported_interface, interface,
            "case {row:?}: expected the assignment on {interface:?}, got {reported_interface:?}"
        );
        let assigned = answered_addresses(row, &body);
        if expected.is_empty() {
            let wanted = usize::try_from(count).expect("a non-negative count");
            assert_eq!(
                assigned.len(),
                wanted,
                "case {row:?}: expected {wanted} assigned addresses, got {assigned:?}"
            );
        } else {
            assert_eq!(
                assigned, expected,
                "case {row:?}: expected the addresses {expected:?}, got {assigned:?}"
            );
        }
        assert_assignable(row, &block, &assigned);
        for address in &assigned {
            assert!(
                !handed_out.contains(address),
                "case {row:?}: {address:?} was already assigned to another interface, and a \
                 private IPv4 address is held by one interface at a time"
            );
            handed_out.push(address.clone());
        }

        // The round trip: what a later caller sees is the primary address the interface was
        // created with, followed by what this call assigned — and only the primary one is
        // reported as primary.
        let described = described_addresses(router, row, &interface).await;
        let mut expected_set = vec![(primary.clone(), true)];
        for address in &assigned {
            if address != &primary {
                expected_set.push((address.clone(), false));
            }
        }
        assert_eq!(
            described, expected_set,
            "case {row:?}: DescribeNetworkInterfaces reported {described:?}, expected \
             {expected_set:?}"
        );

        // The addresses left the subnet: "the number of unused private IPv4 addresses in the
        // subnet" (`Subnet$AvailableIpAddressCount`) is down by exactly what was recorded.
        let available_after = available_addresses(router, row, &subnet).await;
        let taken = available_before - available_after;
        let expected_taken = i64::try_from(new_addresses).expect("a small count");
        assert_eq!(
            taken, expected_taken,
            "case {row:?}: expected the subnet to lose {expected_taken} addresses, it went \
             from {available_before} to {available_after}"
        );
    }
}

/// What one `AssignPrivateIpAddresses` call asks for: the addresses it names, or how many
/// secondary addresses it wants the service to choose (the two a request may not carry
/// together).
type Ask = (Vec<&'static str>, i32);

/// The rule's additive half: a second assignment adds to what the interface already holds,
/// and answers only what IT assigned.
///
/// The rows differ in how each of the two calls asked: a count then a count, a count then
/// named addresses, named then counted, and named then named.
#[tokio::test]
async fn a_second_assignment_adds_to_what_the_interface_already_holds() {
    let case = "additive";
    let (router, _engine) = super::ec2_query::emulator();
    let router = &router;
    let vpc_id = fixture_vpc(case, router).await;

    // (case, first ask, second ask) — each ask is (named addresses, count)
    let cases: [(&str, Ask, Ask); 4] = [
        ("a count then a count", (vec![], 2), (vec![], 1)),
        (
            "a count then named addresses",
            (vec![], 1),
            (vec!["10.80.12.50", "10.80.12.51"], 0),
        ),
        (
            "named addresses then a count",
            (vec!["10.80.13.60"], 0),
            (vec![], 3),
        ),
        (
            "named addresses then named addresses",
            (vec!["10.80.14.70"], 0),
            (vec!["10.80.14.71"], 0),
        ),
    ];

    for (index, (row, (first_named, first_count), (second_named, second_count))) in
        cases.into_iter().enumerate()
    {
        let block = format!("10.80.{}.0/24", index + 11);
        let subnet = create_subnet(row, router, &vpc_id, &block).await;
        let primary = format!("10.80.{}.100", index + 11);
        let interface = create_interface(row, router, &subnet, Some(&primary)).await;

        let first_body = ok(
            row,
            router,
            "AssignPrivateIpAddresses",
            &assign_params(&interface, &first_named, first_count, false),
        )
        .await;
        let first = answered_addresses(row, &first_body);
        let second_body = ok(
            row,
            router,
            "AssignPrivateIpAddresses",
            &assign_params(&interface, &second_named, second_count, false),
        )
        .await;
        let second = answered_addresses(row, &second_body);

        for address in &second {
            assert!(
                !first.contains(address),
                "case {row:?}: the second call answered {address:?}, which the first call \
                 had already assigned"
            );
        }
        let described = described_address_list(router, row, &interface).await;
        let mut expected = vec![primary.clone()];
        expected.extend(first.iter().cloned());
        expected.extend(second.iter().cloned());
        assert_eq!(
            described, expected,
            "case {row:?}: DescribeNetworkInterfaces reported {described:?}, expected the \
             primary address followed by both calls' assignments {expected:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Moving an address something else holds
// ---------------------------------------------------------------------------

/// "Indicates whether to allow an IP address that is already assigned to another network
/// interface or instance to be reassigned to the specified network interface"
/// (`AllowReassignment`): without it an address another interface holds is in use, with it a
/// SECONDARY address of another interface MOVES — and either way the primary address of
/// another interface stays where it is, because an interface cannot be left without one.
///
/// The rows differ along what the member generalises over: whether reassignment was allowed,
/// and whether the address another interface holds is its primary or a secondary one. Both
/// interfaces are read back after every row, so a move is a move and a refusal is a no-op.
#[tokio::test]
async fn reassignment_moves_a_secondary_address_and_never_a_primary_one() {
    let case = "reassignment";
    let (router, _engine) = super::ec2_query::emulator();
    let router = &router;
    let vpc_id = fixture_vpc(case, router).await;

    // (case, whether the donor holds the address as its primary, whether reassignment is
    //  allowed, whether the move is expected to happen)
    let cases: [(&str, bool, bool, bool); 4] = [
        ("a secondary address, reassignment not allowed", false, false, false),
        ("a secondary address, reassignment allowed", false, true, true),
        ("a primary address, reassignment not allowed", true, false, false),
        ("a primary address, reassignment allowed", true, true, false),
    ];

    for (index, (row, from_primary, allow, moves)) in cases.into_iter().enumerate() {
        let block = format!("10.80.{}.0/24", index + 21);
        let subnet = create_subnet(row, router, &vpc_id, &block).await;
        let donor_primary = format!("10.80.{}.100", index + 21);
        let donor_secondary = format!("10.80.{}.101", index + 21);
        let donor = create_interface(row, router, &subnet, Some(&donor_primary)).await;
        ok(
            row,
            router,
            "AssignPrivateIpAddresses",
            &assign_params(&donor, &[donor_secondary.as_str()], 0, false),
        )
        .await;
        let recipient_primary = format!("10.80.{}.200", index + 21);
        let recipient = create_interface(row, router, &subnet, Some(&recipient_primary)).await;
        let wanted = if from_primary {
            donor_primary.clone()
        } else {
            donor_secondary.clone()
        };

        let (status, body) = call(
            router,
            "AssignPrivateIpAddresses",
            &assign_params(&recipient, &[wanted.as_str()], 0, allow),
        )
        .await;
        if moves {
            assert_eq!(
                status, 200,
                "case {row:?}: expected the address to move, got {status} with body {body}"
            );
            let assigned = answered_addresses(row, &body);
            assert_eq!(
                assigned,
                vec![wanted.clone()],
                "case {row:?}: expected the moved address {wanted:?} in the answer, got \
                 {assigned:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {row:?}: expected the address to stay where it is, got {status} with \
                 body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, "InvalidIPAddress.InUse",
                "case {row:?}: expected error code \"InvalidIPAddress.InUse\", got {code:?} \
                 in {body}"
            );
        }

        // What both interfaces hold afterwards: the address is on exactly one of them.
        let donor_holds = described_address_list(router, row, &donor).await;
        let recipient_holds = described_address_list(router, row, &recipient).await;
        assert_eq!(
            donor_holds.contains(&wanted),
            !moves,
            "case {row:?}: the donor holds {donor_holds:?}, and the address {wanted:?} \
             {} have moved off it",
            if moves { "should" } else { "should not" }
        );
        assert_eq!(
            recipient_holds.contains(&wanted),
            moves,
            "case {row:?}: the recipient holds {recipient_holds:?}, and the address \
             {wanted:?} {} have moved onto it",
            if moves { "should" } else { "should not" }
        );
        assert_eq!(
            donor_holds.first().map(String::as_str),
            Some(donor_primary.as_str()),
            "case {row:?}: the donor must keep its primary address, it holds {donor_holds:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What is refused, and that a refusal records nothing
// ---------------------------------------------------------------------------

/// Every refusal of the rule, in one table: each row is a request that asks for something
/// the rule does not admit, and each is answered with the EC2 code that says why.
///
/// The rows differ along what each refusal generalises over: which member is missing or
/// contradicted, what is wrong with a named address (not an address, not of the block, one
/// of the ones AWS reserves, already held), and which ceiling the ask would pass. After the
/// table, the subject interface and its subnet are read back: a refused assignment records
/// nothing.
#[tokio::test]
async fn a_refused_assignment_says_why_and_records_nothing() {
    let case = "refusals";
    let (router, _engine) = super::ec2_query::emulator();
    let router = &router;
    let vpc_id = fixture_vpc(case, router).await;
    let subnet = create_subnet(case, router, &vpc_id, "10.80.31.0/24").await;
    let subject = create_interface(case, router, &subnet, Some("10.80.31.100")).await;
    // Another interface of the same subnet, holding one address, so "an address something
    // else holds" is a real row.
    let holder = create_interface(case, router, &subnet, Some("10.80.31.110")).await;
    // A subnet with room for ten more addresses after its interface's primary one: a `/28`
    // holds eleven assignable addresses, so a request for eleven more cannot be served.
    let small_subnet = create_subnet(case, router, &vpc_id, "10.80.32.0/28").await;
    let small_interface = create_interface(case, router, &small_subnet, Some("10.80.32.4")).await;
    // An interface type that carries no IP addresses at all: "EFA-only network interfaces do
    // not support IP addresses" (`CreateNetworkInterfaceRequest$InterfaceType`).
    let efa_only = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", &subnet),
            param("InterfaceType", ec2::state::NETWORK_INTERFACE_TYPE_EFA_ONLY),
        ],
    )
    .await;
    let efa_only = member(case, &efa_only, "networkInterfaceId");

    let ceiling = i32::try_from(ec2::state::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE)
        .expect("the per-interface ceiling fits an i32");
    // One more address than an interface may hold, named one by one: the list a caller sends
    // is held to the same ceiling as what the interface would hold.
    let too_many: Vec<String> = (0..=ceiling)
        .map(|offset| format!("10.80.31.{}", 4 + offset))
        .collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();

    // (case, the request's fields, the EC2 error code)
    let cases: Vec<(&str, Vec<Param>, &str)> = vec![
        (
            "a value that is no interface id at all",
            assign_params("not-an-id", &[], 1, false),
            "InvalidNetworkInterfaceId.Malformed",
        ),
        (
            "another resource kind's id",
            assign_params("subnet-0123456789abcdef0", &[], 1, false),
            "InvalidNetworkInterfaceId.Malformed",
        ),
        (
            "a long id the account holds no interface for",
            assign_params("eni-0123456789abcdef0", &[], 1, false),
            "InvalidNetworkInterfaceID.NotFound",
        ),
        (
            // The all-zero suffix is WELL FORMED for an interface id, so it is answered as
            // not found rather than as malformed.
            "the all-zero long id",
            assign_params("eni-00000000000000000", &[], 1, false),
            "InvalidNetworkInterfaceID.NotFound",
        ),
        (
            "both the addresses and a count of them",
            assign_params(&subject, &["10.80.31.20"], 2, false),
            "InvalidParameterCombination",
        ),
        (
            "neither the addresses nor a count",
            assign_params(&subject, &[], 0, false),
            "MissingParameter",
        ),
        (
            "a negative count",
            assign_params(&subject, &[], -1, false),
            "InvalidParameterValue",
        ),
        (
            "an interface type that carries no IP addresses",
            assign_params(&efa_only, &[], 1, false),
            "InvalidParameterCombination",
        ),
        (
            "a value that is no address at all",
            assign_params(&subject, &["not-an-address"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "an IPv6 address",
            assign_params(&subject, &["2001:db8::1"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "an address outside the subnet's block",
            assign_params(&subject, &["10.99.99.99"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "an address of another subnet of the VPC",
            assign_params(&subject, &["10.80.32.5"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "the network address of the block",
            assign_params(&subject, &["10.80.31.0"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "the last address AWS reserves at the bottom of the block",
            assign_params(&subject, &["10.80.31.3"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "the broadcast address of the block",
            assign_params(&subject, &["10.80.31.255"], 0, false),
            "InvalidParameterValue",
        ),
        (
            "an address another interface already holds",
            assign_params(&subject, &["10.80.31.110"], 0, false),
            "InvalidIPAddress.InUse",
        ),
        (
            "the same address twice in one request",
            assign_params(&subject, &["10.80.31.30", "10.80.31.30"], 0, false),
            "InvalidIPAddress.InUse",
        ),
        (
            "one more address than an interface may hold",
            assign_params(&subject, &too_many, 0, false),
            "PrivateIpAddressLimitExceeded",
        ),
        (
            "a count one above the per-interface maximum",
            assign_params(&subject, &[], ceiling + 1, false),
            "PrivateIpAddressLimitExceeded",
        ),
        (
            // The interface already holds its primary address, so the ceiling is reached by
            // one fewer than it: exactly the ceiling takes it one past.
            "a count that takes the interface one past the maximum",
            assign_params(&subject, &[], ceiling, false),
            "PrivateIpAddressLimitExceeded",
        ),
        (
            "more addresses than the subnet has left",
            assign_params(&small_interface, &[], 11, false),
            "InsufficientFreeAddressesInSubnet",
        ),
    ];

    let available_before = available_addresses(router, case, &subnet).await;
    for (row, params, expected_code) in cases {
        let (status, body) = call(router, "AssignPrivateIpAddresses", &params).await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, expected_code,
            "case {row:?}: expected error code {expected_code:?}, got {code:?} in {body}"
        );
    }

    // Nothing above was recorded: each interface still holds exactly the address it was
    // created with, and the subnet's count never moved.
    for (row, interface, expected) in [
        ("the subject", &subject, vec!["10.80.31.100".to_string()]),
        ("the holder", &holder, vec!["10.80.31.110".to_string()]),
        ("the efa-only interface", &efa_only, Vec::new()),
    ] {
        let described = described_address_list(router, row, interface).await;
        assert_eq!(
            described, expected,
            "case {row:?}: a refused assignment records nothing, expected {expected:?}, got \
             {described:?}"
        );
    }
    let available_after = available_addresses(router, case, &subnet).await;
    assert_eq!(
        available_before, available_after,
        "a refused assignment takes no address out of the subnet: the count went from \
         {available_before} to {available_after}"
    );

    // The wording of the refusals a client reads to tell one cause from another: the two id
    // refusals, which separate a typo from a resource the account does not hold, and the
    // out-of-range address, whose sentence is fixed text naming neither the address nor the
    // block.
    for (row, params, expected_message) in [
        (
            "a malformed id",
            assign_params("not-an-id", &[], 1, false),
            "The interface ID 'not-an-id' is malformed",
        ),
        (
            "a well-formed id naming nothing",
            assign_params("eni-0123456789abcdef0", &[], 1, false),
            "The networkInterface ID 'eni-0123456789abcdef0' does not exist",
        ),
        (
            "an address outside the subnet's block",
            assign_params(&subject, &["10.99.99.99"], 0, false),
            "Address does not fall within the subnet's address range",
        ),
        (
            "an address another interface already holds",
            assign_params(&subject, &["10.80.31.110"], 0, false),
            "Address 10.80.31.110 is in use.",
        ),
    ] {
        let (_, body) = call(router, "AssignPrivateIpAddresses", &params).await;
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {row:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }

    // A request that omits the interface entirely never reaches the operation:
    // `NetworkInterfaceId` is required, so the boundary validator refuses it first.
    let (status, body) = call(
        router,
        "AssignPrivateIpAddresses",
        &[param("SecondaryPrivateIpAddressCount", "1")],
    )
    .await;
    assert_eq!(
        status, 400,
        "a request with no NetworkInterfaceId must be refused with HTTP 400, got {status} \
         with body {body}"
    );
}

/// IPv4 Prefix Delegation is NOT written yet: a request that asks for prefixes is answered
/// as unimplemented rather than with an interface that silently lacks them, and records
/// nothing — the same answer `CreateNetworkInterface` gives the same members.
///
/// The rows are the two ways a request asks for prefixes: naming them, and counting them.
#[tokio::test]
async fn a_request_for_ipv4_prefixes_is_not_written_yet() {
    let case = "prefixes";
    let (router, _engine) = super::ec2_query::emulator();
    let router = &router;
    let vpc_id = fixture_vpc(case, router).await;
    let subnet = create_subnet(case, router, &vpc_id, "10.80.41.0/24").await;
    let interface = create_interface(case, router, &subnet, Some("10.80.41.100")).await;

    for (row, extra) in [
        (
            "named prefixes",
            vec![param("Ipv4Prefix.1", "10.80.41.32/28")],
        ),
        ("a count of prefixes", vec![param("Ipv4PrefixCount", "1")]),
    ] {
        let mut params = vec![param("NetworkInterfaceId", &interface)];
        params.extend(extra);
        let (status, body) = call(router, "AssignPrivateIpAddresses", &params).await;
        assert_eq!(
            status, 501,
            "case {row:?}: expected HTTP 501 for a prefix this emulator does not hold, got \
             {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "NotImplemented",
            "case {row:?}: expected error code \"NotImplemented\", got {code:?} in {body}"
        );
    }

    let described = described_address_list(router, case, &interface).await;
    assert_eq!(
        described,
        vec!["10.80.41.100".to_string()],
        "a refused prefix request records nothing, got {described:?}"
    );
}
