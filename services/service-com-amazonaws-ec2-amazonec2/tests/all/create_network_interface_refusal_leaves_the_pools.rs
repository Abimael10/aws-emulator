// SPDX-License-Identifier: CC0-1.0

//! What a REFUSED `CreateNetworkInterface` — and a `DryRun` of one that would have succeeded
//! — leaves behind, driven over the wire this service speaks (`ec2Query`: an
//! `application/x-www-form-urlencoded` POST answered with XML).
//!
//! RULE under test: a create that does not store an interface leaves every pool it drew from
//! exactly as it found it. "Checks whether you have the required permissions for the action,
//! without actually making the request" (`CreateNetworkInterfaceRequest$DryRun`) says so for
//! the dry run, and a refusal stores nothing by construction — so after either, the account's
//! IPv4 addresses in use, the subnet's `AvailableIpAddressCount`, the IPv6 addresses and the
//! IPv6 prefixes the account's interfaces hold, and the number of interfaces stored, are all
//! unchanged. REPEATING such a call leaves them unchanged too, however many times it is sent:
//! nothing accumulates per attempt, so a caller cannot exhaust a subnet's IPv4 range or its
//! IPv6 range with calls that create nothing.
//!
//! The two halves of the rule come about differently in the handler, which is why the rows
//! cover both. The interface's private IPv4 addresses are TAKEN from the account's in-use pool
//! as they are chosen, so every refusal hands them back
//! (`crate::state::ComAmazonawsEc2Amazonec2State::release_private_ips`). The IPv6 addresses
//! and prefixes are RESOLVED against the pool the interface records themselves are
//! (`resolve_requested_ipv6` takes `&self` and writes nothing: an assigned IPv6 address lives
//! on `NetworkInterface$Ipv6Addresses`, so the record a successful create stores IS the take)
//! — so nothing is owed back there, and this suite is what pins that: were either half to
//! start leaking, these rows fail.
//!
//! The rows span what the rule generalises over: WHICH stop the call makes (a dry run of a
//! request that would have succeeded, an IPv4 address outside the subnet, an IPv4 address
//! another interface already holds, an IPv6 address outside the subnet's block, an IPv6
//! address another interface already holds, and an IPv6 prefix outside it), whether the
//! request also asks for IPv6 space (a count, named values, or both), and how many times the
//! same call is repeated. One row is a CONTROL that must change the pools: a create that
//! succeeds takes an address and an IPv6 address, which is what makes the other rows' equality
//! an assertion about refusals rather than about a handler that takes nothing at all.

use std::collections::BTreeSet;

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::create_network_interface_ipv6::{call, fixtures, member, ok, Fixtures};
use super::ec2_query::error_code;
use super::wire::{param, Param};

/// An IPv6 address inside the fixture subnet's `/64` and above the addresses AWS reserves in
/// it, so a request naming it is one the subnet could serve.
const SERVABLE_IPV6: &str = "2001:db8:a:1::1:0";

/// A second one, for the row that holds it on another interface first.
const HELD_IPV6: &str = "2001:db8:a:1::1:1";

/// An IPv6 address of ANOTHER block, which the fixture subnet cannot serve.
const FOREIGN_IPV6: &str = "2001:db8:b:1::5";

/// An IPv6 prefix of another block, on the same terms.
const FOREIGN_IPV6_PREFIX: &str = "2001:db8:b:1::/80";

/// A private IPv4 address inside the fixture dual-stack subnet.
const SERVABLE_IPV4: &str = "10.150.1.42";

/// A second one, which nothing in these rows holds, for the row whose request would have
/// succeeded.
const FREE_IPV4: &str = "10.150.1.43";

/// A private IPv4 address of ANOTHER subnet, which this one cannot serve.
const FOREIGN_IPV4: &str = "10.150.9.9";

/// Every pool a create draws from, as a reader can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pools {
    /// How many interfaces the account holds.
    interfaces: usize,
    /// The subnet's own count of assignable IPv4 addresses left.
    available_ipv4: i32,
    /// Every IPv6 address the account's interfaces hold.
    ipv6_addresses: BTreeSet<String>,
    /// Every IPv6 prefix delegated to them.
    ipv6_prefixes: BTreeSet<String>,
    /// Whether the account holds the IPv4 addresses these rows name.
    servable_ipv4_in_use: bool,
    free_ipv4_in_use: bool,
    foreign_ipv4_in_use: bool,
}

/// The pools as they stand, read off the one store the handler writes.
fn pools(fixtures: &Fixtures) -> Pools {
    let store = fixtures.engine.state::<Ec2State>();
    let guard = store.lock();
    Pools {
        interfaces: guard.network_interfaces.len(),
        available_ipv4: guard
            .subnets
            .get(&fixtures.dual_stack_subnet)
            .and_then(|subnet| subnet.available_ip_address_count)
            .unwrap_or_default(),
        ipv6_addresses: guard.assigned_ipv6_addresses().into_iter().collect(),
        ipv6_prefixes: guard.assigned_ipv6_prefixes().into_iter().collect(),
        servable_ipv4_in_use: guard.private_ip_in_use(SERVABLE_IPV4),
        free_ipv4_in_use: guard.private_ip_in_use(FREE_IPV4),
        foreign_ipv4_in_use: guard.private_ip_in_use(FOREIGN_IPV4),
    }
}

/// One call that must not store an interface.
struct Row {
    case: &'static str,
    /// The fields the create carries beyond `SubnetId`.
    extra: fn() -> Vec<Param>,
    /// The HTTP status it must be answered with.
    status: u16,
    /// The EC2 error code it must carry.
    code: &'static str,
    /// How many times the same call is sent, so a per-attempt leak shows up as drift.
    repeats: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // A request that would have SUCCEEDED, stopped by the dry run: "if you have the
            // required permissions, the error response is DryRunOperation" (`DryRun`).
            case: "a dry run of a request that asks for IPv6 addresses",
            extra: || vec![param("DryRun", "true"), param("Ipv6AddressCount", "2")],
            status: 412,
            code: "DryRunOperation",
            repeats: 5,
        },
        Row {
            // The same stop, asking for the other half of the IPv6 space: a delegated prefix.
            // "You may specify only one of ipv6Addresses, ipv6AddressCount, ipv6Prefixes and
            // ipv6PrefixCount", so each row asks for exactly one of them.
            case: "a dry run of a request that asks for an IPv6 prefix",
            extra: || vec![param("DryRun", "true"), param("Ipv6PrefixCount", "1")],
            status: 412,
            code: "DryRunOperation",
            repeats: 5,
        },
        Row {
            case: "a dry run naming an IPv6 address and an IPv4 address",
            extra: || {
                vec![
                    param("DryRun", "true"),
                    param("PrivateIpAddress", FREE_IPV4),
                    param("Ipv6Addresses.1.Ipv6Address", SERVABLE_IPV6),
                ]
            },
            status: 412,
            code: "DryRunOperation",
            repeats: 3,
        },
        Row {
            case: "an IPv4 address the subnet cannot serve, with an IPv6 count",
            extra: || {
                vec![
                    param("PrivateIpAddress", FOREIGN_IPV4),
                    param("Ipv6AddressCount", "1"),
                ]
            },
            status: 400,
            code: "InvalidParameterValue",
            repeats: 3,
        },
        Row {
            case: "an IPv6 address the subnet cannot serve",
            extra: || {
                vec![param("Ipv6Addresses.1.Ipv6Address", FOREIGN_IPV6)]
            },
            status: 400,
            code: "InvalidParameterValue",
            repeats: 3,
        },
        Row {
            case: "an IPv6 prefix the subnet cannot serve",
            // The member carries no `@ec2QueryName`, so ec2Query spells it with its
            // `@xmlName`: `Ipv6Prefix.N.Ipv6Prefix`.
            extra: || vec![param("Ipv6Prefix.1.Ipv6Prefix", FOREIGN_IPV6_PREFIX)],
            status: 400,
            code: "InvalidParameterValue",
            repeats: 3,
        },
        Row {
            case: "an IPv6 address another interface already holds",
            extra: || vec![param("Ipv6Addresses.1.Ipv6Address", HELD_IPV6)],
            status: 400,
            code: "InvalidIPAddress.InUse",
            repeats: 3,
        },
        Row {
            case: "an IPv4 address another interface already holds, with IPv6 space asked for",
            extra: || {
                vec![
                    param("PrivateIpAddress", SERVABLE_IPV4),
                    param("Ipv6AddressCount", "1"),
                ]
            },
            status: 400,
            code: "InvalidIPAddress.InUse",
            repeats: 3,
        },
    ]
}

/// RULE: every row leaves the pools it found, however many times it is sent.
#[tokio::test]
async fn a_create_that_stores_nothing_leaves_every_pool_it_found() {
    let fixtures = fixtures("the fixtures").await;

    // The interface the two "already holds" rows collide with: it holds one of this suite's
    // IPv4 addresses and one of its IPv6 addresses, so those rows are refused on state rather
    // than on their own shape.
    let holder = ok(
        "the holding interface",
        &fixtures.router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", &fixtures.dual_stack_subnet),
            param("PrivateIpAddress", SERVABLE_IPV4),
            param("Ipv6Addresses.1.Ipv6Address", HELD_IPV6),
        ],
    )
    .await;
    let holder_id = member("the holding interface", &holder, "networkInterfaceId");

    for row in rows() {
        let before = pools(&fixtures);
        assert!(
            before.servable_ipv4_in_use,
            "case {:?}: the holding interface must hold {SERVABLE_IPV4}, got {before:?}",
            row.case
        );
        assert!(
            before.ipv6_addresses.contains(HELD_IPV6),
            "case {:?}: the holding interface must hold {HELD_IPV6}, got {before:?}",
            row.case
        );

        for attempt in 1..=row.repeats {
            let mut params = vec![param("SubnetId", &fixtures.dual_stack_subnet)];
            params.extend((row.extra)());
            let (status, body) = call(&fixtures.router, "CreateNetworkInterface", &params).await;
            assert_eq!(
                status, row.status,
                "case {:?} (attempt {attempt}): expected HTTP {}, got {status} with body {body}",
                row.case, row.status
            );
            assert_eq!(
                error_code(&body),
                row.code,
                "case {:?} (attempt {attempt}): expected the {} code, got {body}",
                row.case,
                row.code
            );
            // Nothing was stored, and nothing was taken: the pools read exactly as they did
            // before this attempt, so no attempt can accumulate anything.
            let after = pools(&fixtures);
            assert_eq!(
                after, before,
                "case {:?} (attempt {attempt}): a call that stores no interface must leave \
                 every pool it found, got {after:?} after {before:?}",
                row.case
            );
        }
    }

    // CONTROL: a create that SUCCEEDS does move the pools, so the equalities above are about
    // refusals rather than about a handler that never takes anything.
    let before = pools(&fixtures);
    let created = ok(
        "the control create",
        &fixtures.router,
        "CreateNetworkInterface",
        &[
            param("SubnetId", &fixtures.dual_stack_subnet),
            param("Ipv6AddressCount", "1"),
        ],
    )
    .await;
    let created_id = member("the control create", &created, "networkInterfaceId");
    let after = pools(&fixtures);
    assert_eq!(
        after.interfaces,
        before.interfaces + 1,
        "the control create must store one interface, got {after:?} after {before:?}"
    );
    assert_eq!(
        after.available_ipv4,
        before.available_ipv4 - 1,
        "the control create must take one IPv4 address out of the subnet, got {after:?} after \
         {before:?}"
    );
    assert_eq!(
        after.ipv6_addresses.len(),
        before.ipv6_addresses.len() + 1,
        "the control create must take one IPv6 address, got {after:?} after {before:?}"
    );

    // And the pools go back when the interfaces go: a delete is the exact reverse of the
    // create, so the account ends where the holding interface left it.
    for id in [created_id, holder_id] {
        ok(
            "the cleanup delete",
            &fixtures.router,
            "DeleteNetworkInterface",
            &[param("NetworkInterfaceId", &id)],
        )
        .await;
    }
    let emptied = pools(&fixtures);
    assert_eq!(
        emptied.interfaces, 0,
        "every interface this suite created must be gone, got {emptied:?}"
    );
    assert!(
        emptied.ipv6_addresses.is_empty() && emptied.ipv6_prefixes.is_empty(),
        "an account holding no interface holds no IPv6 address or prefix, got {emptied:?}"
    );
    assert!(
        !emptied.servable_ipv4_in_use && !emptied.free_ipv4_in_use && !emptied.foreign_ipv4_in_use,
        "an account holding no interface holds none of the IPv4 addresses, got {emptied:?}"
    );
}

/// RULE, the other half of the subnet question: a request that asks for IPv6 space in a subnet
/// with NO IPv6 range is refused, and that refusal leaves the pools it found too — the subnet
/// it addressed has nothing to take, and the one it does not address is untouched.
#[tokio::test]
async fn an_ipv6_request_in_a_subnet_with_no_range_takes_nothing_either() {
    let fixtures = fixtures("the fixtures").await;
    let case = "a subnet with no IPv6 range";
    let before = pools(&fixtures);

    for attempt in 1..=3 {
        let (status, body) = call(
            &fixtures.router,
            "CreateNetworkInterface",
            &[
                param("SubnetId", &fixtures.ipv4_only_subnet),
                param("Ipv6AddressCount", "2"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?} (attempt {attempt}): expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?} (attempt {attempt}): expected the InvalidParameterValue code, got \
             {body}"
        );
        let after = pools(&fixtures);
        assert_eq!(
            after, before,
            "case {case:?} (attempt {attempt}): the refusal must leave every pool it found, \
             got {after:?} after {before:?}"
        );
    }
}
