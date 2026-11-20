// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DisassociateNatGatewayAddress`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=DisassociateNatGatewayAddress`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! 1. a disassociation DRAINS the secondary Elastic IP addresses it names and then releases
//!    them: each named address stays ON the gateway in the transient `disassociating` status
//!    — "It stays associated and supports the existing connections but does not support any
//!    new connections" — and the CLOCK, not a second call, takes it away at the end of
//!    `MaxDrainDurationSeconds`, at which point the Elastic IP address is free to be
//!    associated again while the allocation itself stays in the account;
//! 2. the wait is the one the REQUEST asked for: a call that names no `MaxDrainDurationSeconds`
//!    drains for the documented default (350 seconds), so its address is still on the gateway
//!    long after a one-second drain has finished;
//! 3. `DryRun` answers the permitted outcome — HTTP 412 `DryRunOperation` — and leaves every
//!    association exactly as it was, on the gateway and on the Elastic IP address record;
//! 4. a request naming a gateway the account does not hold, a gateway whose state or
//!    connectivity does not admit the change, an association the gateway does not hold, the
//!    gateway's PRIMARY address ("You cannot disassociate your primary EIP"), one association
//!    twice, or no association at all is refused with EC2's own error code and changes
//!    NOTHING.
//!
//! Each row is a distinct input along what those rules generalise over — how many
//! associations one call names, whether the drain is named or left to its default, which end
//! of the published `MaxDrainDurationSeconds` range it names, and which way a request fails
//! the contract — so no single recorded call is the specification. Everything the rows need
//! is built through the API: the VPC and subnet through `CreateVpc`/`CreateSubnet`, the
//! Elastic IP addresses through `AllocateAddress`, the gateway through `CreateNatGateway` and
//! its secondary addresses through `AssociateNatGatewayAddress`.

use std::time::{Duration, Instant};

use ec2::state::{
    CONNECTIVITY_TYPE_PRIVATE, MAX_NAT_GATEWAY_DRAIN_SECONDS, MIN_NAT_GATEWAY_DRAIN_SECONDS,
    NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING, NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED,
    NAT_GATEWAY_STATE_AVAILABLE, NAT_GATEWAY_STATE_DELETED,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, indexed};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The Availability Zone the gateways below sit in.
const ZONE: &str = "us-east-1a";

/// EC2's error code for a NAT gateway id the account holds no NAT gateway for.
const NAT_GATEWAY_NOT_FOUND: &str = "NatGatewayNotFound";

/// EC2's error code for a resource whose current state does not admit the request.
const INCORRECT_STATE: &str = "IncorrectState";

/// EC2's error code for a value the service rejects.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// EC2's error code for a parameter the operation needs and the request left out.
const MISSING_PARAMETER: &str = "MissingParameter";

/// The code a dry run of a request that would have succeeded answers, and the HTTP status it
/// is answered with.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;

/// The published bounds of `DisassociateNatGatewayAddressRequest$MaxDrainDurationSeconds`
/// (`smithy_query(query="operation",operation="DisassociateNatGatewayAddress")` reports
/// `range {min: 1, max: 4000}`), named by the constants the crate holds them in rather than
/// by literals so the two cannot drift.
const DRAIN_RANGE: (i64, i64) = (
    MIN_NAT_GATEWAY_DRAIN_SECONDS as i64,
    MAX_NAT_GATEWAY_DRAIN_SECONDS as i64,
);

/// How long a poll is given before it reports that the condition never held: generously past
/// every span this suite waits on (one `TRANSITION_DELAY` for a gateway coming up, and the
/// one-second drain the rows ask for), so a loaded machine fails nothing while a condition
/// that never holds still ends the test instead of hanging.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between reads, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call of `action`, as its HTTP status and XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = ec2(router, action, params).await;
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

/// One `natGatewayAddressSet` item, as the members these rules assert.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAddress {
    allocation_id: String,
    association_id: String,
    private_ip: String,
    status: String,
    is_primary: String,
}

/// Every `natGatewayAddressSet` item of `body`, in document order.
///
/// Read with the XML parser rather than by substring, so a member of any other element of the
/// response can never be read as one of a gateway's addresses.
fn reported_addresses(case: &str, body: &str) -> Vec<ReportedAddress> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "natGatewayAddressSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .map(|item| ReportedAddress {
            allocation_id: child_text(item, "allocationId").unwrap_or_default(),
            association_id: child_text(item, "associationId").unwrap_or_default(),
            private_ip: child_text(item, "privateIp").unwrap_or_default(),
            status: child_text(item, "status").unwrap_or_default(),
            is_primary: child_text(item, "isPrimary").unwrap_or_default(),
        })
        .collect()
}

/// The addresses `DescribeNatGateways` reports for `nat_gateway_id` — what a later caller is
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

/// A VPC of `cidr_block` with one subnet of `subnet_cidr` in it, as that subnet's id.
async fn subnet(
    case: &str,
    router: &MultiServiceRouter,
    cidr_block: &str,
    subnet_cidr: &str,
) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let vpc_id = member(case, &body, "vpcId");
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", subnet_cidr),
            param("AvailabilityZone", ZONE),
        ],
    )
    .await;
    member(case, &body, "subnetId")
}

/// One Elastic IP address out of Amazon's pool, as its allocation id.
async fn allocation(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "AllocateAddress", &[]).await;
    member(case, &body, "allocationId")
}

/// Poll `DescribeNatGateways` until the gateway is in `state`, exactly as a client waiting
/// for its NAT gateway does: nothing but the clock moves it.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
    state: &str,
) {
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
        if reported == state {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the NAT gateway {nat_gateway_id} is still in {reported:?} rather \
             than {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll until every address of the gateway has reached the terminal `succeeded` status.
async fn poll_until_addresses_settled(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
) -> Vec<ReportedAddress> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reported = described_addresses(case, router, nat_gateway_id).await;
        if reported
            .iter()
            .all(|address| address.status == NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED)
        {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the addresses of {nat_gateway_id} never settled, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll until the gateway holds exactly `remaining` addresses — the release the clock owes a
/// drain that has ended, waited for as a CONDITION rather than for a span.
async fn poll_until_addresses_number(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
    remaining: usize,
) -> Vec<ReportedAddress> {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reported = described_addresses(case, router, nat_gateway_id).await;
        if reported.len() == remaining {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {remaining} address(es) to be left on {nat_gateway_id}, \
             got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A serving public NAT gateway in a network of its own, holding its primary Elastic IP
/// address and one secondary — associated by the CREATE itself when `secondary_at_create`,
/// and by `AssociateNatGatewayAddress` afterwards otherwise, which is the same association
/// reached two ways. Answers the gateway id and its addresses once every one of them has
/// settled.
///
/// "By default, you can associate up to 2 Elastic IP addresses per public NAT gateway"
/// (`AssociateNatGatewayAddress`), so a primary and one secondary is the whole of what a
/// single-zone public gateway holds.
async fn serving_gateway(
    case: &str,
    router: &MultiServiceRouter,
    network: (&str, &str),
    secondary_at_create: bool,
) -> (String, Vec<ReportedAddress>) {
    let subnet_id = subnet(case, router, network.0, network.1).await;
    let primary = allocation(case, router).await;
    let secondary = allocation(case, router).await;
    let mut params = vec![param("SubnetId", &subnet_id), param("AllocationId", &primary)];
    if secondary_at_create {
        params.push(param("SecondaryAllocationId.1", &secondary));
    }
    let body = ok(case, router, "CreateNatGateway", &params).await;
    let nat_gateway_id = member(case, &body, "natGatewayId");
    // An association is only accepted while the gateway is serving, so the create's
    // `pending` has to have come due first — the clock does that, not a second call.
    poll_until_state(case, router, &nat_gateway_id, NAT_GATEWAY_STATE_AVAILABLE).await;

    if !secondary_at_create {
        let mut params = vec![param("NatGatewayId", &nat_gateway_id)];
        params.extend(indexed("AllocationId", &[secondary.as_str()]));
        ok(case, router, "AssociateNatGatewayAddress", &params).await;
    }
    let settled = poll_until_addresses_settled(case, router, &nat_gateway_id).await;
    (nat_gateway_id, settled)
}

/// The secondary addresses of a gateway: every one that is not its primary.
fn secondaries(addresses: &[ReportedAddress]) -> Vec<ReportedAddress> {
    addresses
        .iter()
        .filter(|address| address.is_primary != "true")
        .cloned()
        .collect()
}

/// The association ids of `addresses`, as the request names them.
fn association_params(addresses: &[ReportedAddress]) -> Vec<Param> {
    let named: Vec<&str> = addresses
        .iter()
        .map(|address| address.association_id.as_str())
        .collect();
    indexed("AssociationId", &named)
}

/// The `associationId` `DescribeAddresses` reports for one allocation, and whether the
/// allocation is still in the account at all.
async fn described_address(
    case: &str,
    router: &MultiServiceRouter,
    allocation_id: &str,
) -> (bool, Option<String>) {
    let body = ok(
        case,
        router,
        "DescribeAddresses",
        &[param("AllocationId.1", allocation_id)],
    )
    .await;
    let held = body.contains(allocation_id);
    (held, element(&body, "associationId"))
}

// ---------------------------------------------------------------------------
// Rules 1 and 2: the drain, and the release the clock owes it
// ---------------------------------------------------------------------------

/// One row of the drain table.
struct DrainCase {
    case: &'static str,
    /// The VPC's block and the subnet's block the gateway sits in.
    network: (&'static str, &'static str),
    /// Whether the secondary address was associated by the create or by
    /// `AssociateNatGatewayAddress` afterwards.
    secondary_at_create: bool,
}

const DRAIN_CASES: &[DrainCase] = &[
    DrainCase {
        case: "a secondary address the create associated",
        network: ("10.70.0.0/16", "10.70.1.0/24"),
        secondary_at_create: true,
    },
    DrainCase {
        case: "a secondary address associated after the gateway came up",
        network: ("10.71.0.0/16", "10.71.1.0/24"),
        secondary_at_create: false,
    },
];

/// RULE 1: the named addresses go into `disassociating` and STAY on the gateway, and the
/// clock then releases them: the Elastic IP address is free to be associated again and the
/// allocation is still in the account.
#[tokio::test]
async fn a_disassociation_drains_and_then_the_clock_releases_the_address() {
    for row in DRAIN_CASES {
        let case = row.case;
        let router = emulator();
        let (nat_gateway_id, addresses) =
            serving_gateway(case, &router, row.network, row.secondary_at_create).await;
        let secondary = secondaries(&addresses);
        assert_eq!(
            secondary.len(),
            1,
            "case {case:?}: expected one secondary address on the gateway, got {addresses:?}"
        );
        let draining = &secondary[..1];

        // The drain the request asks for is the shortest the model admits, so the release
        // below is waited for as a condition rather than for the documented default.
        let mut params = vec![
            param("NatGatewayId", &nat_gateway_id),
            param("MaxDrainDurationSeconds", &DRAIN_RANGE.0.to_string()),
        ];
        params.extend(association_params(draining));
        let body = ok(case, &router, "DisassociateNatGatewayAddress", &params).await;
        assert_eq!(
            element(&body, "natGatewayId").as_deref(),
            Some(nat_gateway_id.as_str()),
            "case {case:?}: expected the response to name the gateway, got {body}"
        );
        // The response reports the gateway's WHOLE address set, with exactly the named
        // addresses moved into the transient status and every other one left as it was.
        let answered = reported_addresses(case, &body);
        let expected: Vec<(String, String)> = addresses
            .iter()
            .map(|address| {
                let status = if draining
                    .iter()
                    .any(|drained| drained.association_id == address.association_id)
                {
                    NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING
                } else {
                    NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED
                };
                (address.association_id.clone(), status.to_string())
            })
            .collect();
        let reported: Vec<(String, String)> = answered
            .iter()
            .map(|address| (address.association_id.clone(), address.status.clone()))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the named addresses to be {NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING} \
             and the rest {NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED}, got {answered:?}"
        );
        // "It stays associated and supports the existing connections": a draining address is
        // still on the gateway, still carrying its Elastic IP address and its private
        // address, and a later reader is told the same thing.
        let still = described_addresses(case, &router, &nat_gateway_id).await;
        for drained in draining {
            let held = still
                .iter()
                .find(|address| address.association_id == drained.association_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the draining address {drained:?} must still be on the \
                         gateway, got {still:?}"
                    )
                });
            assert_eq!(
                (held.allocation_id.as_str(), held.private_ip.as_str()),
                (drained.allocation_id.as_str(), drained.private_ip.as_str()),
                "case {case:?}: a draining address keeps its addresses, got {held:?}"
            );
        }

        // The clock, not a second call, ends the drain: the address leaves the gateway.
        let left =
            poll_until_addresses_number(case, &router, &nat_gateway_id, addresses.len() - 1).await;
        for drained in draining {
            assert!(
                !left
                    .iter()
                    .any(|address| address.association_id == drained.association_id),
                "case {case:?}: the released {drained:?} must be gone, got {left:?}"
            );
            // "Deleting a public NAT gateway disassociates its Elastic IP address, but does
            // not release the address from your account" — the same reading of a release: the
            // allocation stays, carrying no association.
            let (held, association) =
                described_address(case, &router, &drained.allocation_id).await;
            assert!(
                held,
                "case {case:?}: the allocation {} must still be in the account",
                drained.allocation_id
            );
            assert_eq!(
                association, None,
                "case {case:?}: the released allocation {} must carry no association",
                drained.allocation_id
            );
            // And it is free to be associated again, which is what the release gave back:
            // the Elastic IP address and the private address of the subnet it held.
            let mut params = vec![param("NatGatewayId", &nat_gateway_id)];
            params.extend(indexed("AllocationId", &[drained.allocation_id.as_str()]));
            ok(case, &router, "AssociateNatGatewayAddress", &params).await;
        }
    }
}

/// RULE 2: the wait is the one the request asked for — a call that names no
/// `MaxDrainDurationSeconds` drains for the documented default, so its address is still on
/// the gateway after a one-second drain of another gateway's address has finished.
#[tokio::test]
async fn a_drain_the_request_does_not_bound_keeps_the_address_for_the_default() {
    let case = "a drain left to its default";
    let router = emulator();
    let (bounded_id, bounded) =
        serving_gateway(case, &router, ("10.72.0.0/16", "10.72.1.0/24"), false).await;
    let (default_id, default) =
        serving_gateway(case, &router, ("10.73.0.0/16", "10.73.1.0/24"), false).await;
    let bounded_secondary = secondaries(&bounded);
    let default_secondary = secondaries(&default);

    let mut params = vec![
        param("NatGatewayId", &bounded_id),
        param("MaxDrainDurationSeconds", &DRAIN_RANGE.0.to_string()),
    ];
    params.extend(association_params(&bounded_secondary));
    ok(case, &router, "DisassociateNatGatewayAddress", &params).await;

    let mut params = vec![param("NatGatewayId", &default_id)];
    params.extend(association_params(&default_secondary));
    ok(case, &router, "DisassociateNatGatewayAddress", &params).await;

    // The one-second drain ends; the default one, two orders of magnitude longer, has not.
    poll_until_addresses_number(case, &router, &bounded_id, bounded.len() - 1).await;
    let still = described_addresses(case, &router, &default_id).await;
    assert_eq!(
        still.len(),
        default.len(),
        "case {case:?}: expected the unbounded drain to keep every address, got {still:?}"
    );
    let draining = still
        .iter()
        .find(|address| address.association_id == default_secondary[0].association_id)
        .unwrap_or_else(|| panic!("case {case:?}: the draining address is gone: {still:?}"));
    assert_eq!(
        draining.status, NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING,
        "case {case:?}: expected the address to still be draining, got {draining:?}"
    );
}

/// `MaxDrainDurationSeconds` is held to its modelled range: each published bound is accepted
/// and the value one past each is refused, with nothing drained.
#[tokio::test]
async fn the_drain_duration_is_held_to_its_modelled_range() {
    // (case, the value, whether it is inside the published range)
    let cases: [(&str, i64, bool); 5] = [
        ("one below the minimum", DRAIN_RANGE.0 - 1, false),
        ("the minimum", DRAIN_RANGE.0, true),
        ("inside", (DRAIN_RANGE.0 + DRAIN_RANGE.1) / 2, true),
        ("the maximum", DRAIN_RANGE.1, true),
        ("one above the maximum", DRAIN_RANGE.1 + 1, false),
    ];
    for (index, (case, drain, inside)) in cases.into_iter().enumerate() {
        let router = emulator();
        let network = (
            format!("10.{}.0.0/16", 80 + index),
            format!("10.{}.1.0/24", 80 + index),
        );
        let (nat_gateway_id, addresses) =
            serving_gateway(case, &router, (&network.0, &network.1), false).await;
        let secondary = secondaries(&addresses);
        let mut params = vec![
            param("NatGatewayId", &nat_gateway_id),
            param("MaxDrainDurationSeconds", &drain.to_string()),
        ];
        params.extend(association_params(&secondary));
        let (status, body) = ec2(&router, "DisassociateNatGatewayAddress", &params).await;
        if inside {
            assert_eq!(
                status, 200,
                "case {case:?}: {drain} is inside {DRAIN_RANGE:?}, got {status} with body \
                 {body}"
            );
            let reported = reported_addresses(case, &body);
            assert!(
                reported.iter().any(|address| {
                    address.association_id == secondary[0].association_id
                        && address.status == NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING
                }),
                "case {case:?}: expected the named address to be draining, got {reported:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: {drain} is outside {DRAIN_RANGE:?}, got {status} with body \
                 {body}"
            );
            let untouched = described_addresses(case, &router, &nat_gateway_id).await;
            assert_eq!(
                untouched, addresses,
                "case {case:?}: a refused call must leave every address as it was, got \
                 {untouched:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3: DryRun
// ---------------------------------------------------------------------------

/// RULE 3: a dry run of a disassociation that WOULD have succeeded answers HTTP 412
/// `DryRunOperation` and leaves the association exactly as it was — on the gateway, in
/// `succeeded`, and on the Elastic IP address record.
#[tokio::test]
async fn a_dry_run_reports_the_permitted_outcome_and_changes_nothing() {
    let case = "a dry run of a disassociation that would have succeeded";
    let router = emulator();
    let (nat_gateway_id, addresses) =
        serving_gateway(case, &router, ("10.74.0.0/16", "10.74.1.0/24"), false).await;
    let secondary = secondaries(&addresses);

    let mut params = vec![param("NatGatewayId", &nat_gateway_id), param("DryRun", "true")];
    params.extend(association_params(&secondary));
    let (status, body) = ec2(&router, "DisassociateNatGatewayAddress", &params).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (DRY_RUN_STATUS, DRY_RUN_OPERATION),
        "case {case:?}: expected {DRY_RUN_OPERATION}, got {status} with body {body}"
    );

    let untouched = described_addresses(case, &router, &nat_gateway_id).await;
    assert_eq!(
        untouched, addresses,
        "case {case:?}: a dry run must leave every address as it was, got {untouched:?}"
    );
    let (held, association) =
        described_address(case, &router, &secondary[0].allocation_id).await;
    assert_eq!(
        (held, association.as_deref()),
        (true, Some(secondary[0].association_id.as_str())),
        "case {case:?}: a dry run must leave the Elastic IP address associated"
    );

    // And the same request without the flag really does drain it, so the dry run answered
    // the outcome of a request that would have worked.
    let mut params = vec![
        param("NatGatewayId", &nat_gateway_id),
        param("MaxDrainDurationSeconds", &DRAIN_RANGE.0.to_string()),
    ];
    params.extend(association_params(&secondary));
    let body = ok(case, &router, "DisassociateNatGatewayAddress", &params).await;
    let reported = reported_addresses(case, &body);
    assert!(
        reported.iter().any(|address| {
            address.association_id == secondary[0].association_id
                && address.status == NAT_GATEWAY_ADDRESS_STATUS_DISASSOCIATING
        }),
        "case {case:?}: expected the address to be draining, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: what is refused
// ---------------------------------------------------------------------------

/// RULE 4: a request the contract does not admit is refused with EC2's own code for what was
/// wrong, and changes nothing. The rows differ along which part of the contract fails: the
/// gateway id, the gateway's state, its connectivity type, and the associations named.
#[tokio::test]
async fn a_request_the_contract_does_not_admit_is_refused() {
    let case = "the estate";
    let router = emulator();
    let (serving_id, addresses) =
        serving_gateway(case, &router, ("10.75.0.0/16", "10.75.1.0/24"), false).await;
    let secondary = secondaries(&addresses);
    let primary = addresses
        .iter()
        .find(|address| address.is_primary == "true")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the gateway has no primary: {addresses:?}"));

    // A private NAT gateway, which has no Elastic IP address at all.
    let private_subnet = subnet(case, &router, "10.76.0.0/16", "10.76.1.0/24").await;
    let body = ok(
        case,
        &router,
        "CreateNatGateway",
        &[
            param("SubnetId", &private_subnet),
            param("ConnectivityType", CONNECTIVITY_TYPE_PRIVATE),
        ],
    )
    .await;
    let private_id = member(case, &body, "natGatewayId");
    poll_until_state(case, &router, &private_id, NAT_GATEWAY_STATE_AVAILABLE).await;

    // A gateway that has been deleted: a terminal state, and not one in which its addresses
    // are the caller's to change.
    let (deleted_id, deleted_addresses) =
        serving_gateway(case, &router, ("10.77.0.0/16", "10.77.1.0/24"), true).await;
    let deleted_secondary = secondaries(&deleted_addresses);
    ok(
        case,
        &router,
        "DeleteNatGateway",
        &[param("NatGatewayId", &deleted_id)],
    )
    .await;
    poll_until_state(case, &router, &deleted_id, NAT_GATEWAY_STATE_DELETED).await;

    // (case, the request's fields, the expected code)
    let rows: Vec<(&str, Vec<Param>, &str)> = vec![
        (
            "a NAT gateway id the account holds no gateway for",
            {
                let mut params = vec![param("NatGatewayId", "nat-0123456789abcdef0")];
                params.extend(association_params(&secondary));
                params
            },
            NAT_GATEWAY_NOT_FOUND,
        ),
        (
            "no association at all",
            vec![param("NatGatewayId", &serving_id)],
            MISSING_PARAMETER,
        ),
        (
            "an association the gateway does not hold",
            {
                let mut params = vec![param("NatGatewayId", &serving_id)];
                params.extend(indexed("AssociationId", &["eipassoc-0123456789abcdef0"]));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "the gateway's primary Elastic IP address",
            {
                let mut params = vec![param("NatGatewayId", &serving_id)];
                params.extend(association_params(std::slice::from_ref(&primary)));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "one association named twice",
            {
                let mut params = vec![param("NatGatewayId", &serving_id)];
                params.extend(indexed(
                    "AssociationId",
                    &[
                        secondary[0].association_id.as_str(),
                        secondary[0].association_id.as_str(),
                    ],
                ));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "a private NAT gateway, which holds no Elastic IP address",
            {
                let mut params = vec![param("NatGatewayId", &private_id)];
                params.extend(association_params(&secondary));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "a gateway that has been deleted",
            {
                let mut params = vec![param("NatGatewayId", &deleted_id)];
                params.extend(association_params(&deleted_secondary));
                params
            },
            INCORRECT_STATE,
        ),
    ];

    for (case, params, expected) in rows {
        let (status, body) = ec2(&router, "DisassociateNatGatewayAddress", &params).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, expected),
            "case {case:?}: expected {expected}, got {status} with body {body}"
        );
        // Nothing moved: the serving gateway still holds both of its addresses, each in the
        // terminal status the associations settled in.
        let untouched = described_addresses(case, &router, &serving_id).await;
        assert_eq!(
            untouched, addresses,
            "case {case:?}: a refused call must leave every address as it was, got \
             {untouched:?}"
        );
    }
}
