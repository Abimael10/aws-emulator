// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `AssignPrivateNatGatewayAddress` and
//! `UnassignPrivateNatGatewayAddress`, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! 1. an assignment ADDS SECONDARY PRIVATE IPv4 ADDRESSES OUT OF THE GATEWAY'S OWN SUBNET
//!    to a serving private NAT gateway — the ones the request names, or the number of them
//!    it asks the service to choose — and persists each on the gateway, on the gateway's own
//!    network interface, as a secondary address in the transient `assigning` status; the
//!    very next `DescribeNatGateways` reports the same records;
//! 2. the CLOCK finishes the assignment: an address answered `assigning` reaches
//!    `succeeded` without anybody calling again;
//! 3. an unassignment leaves each named address on the gateway in the transient
//!    `unassigning` status while its connections drain, and the address is RELEASED at the
//!    end of the drain the request asked for — "A private IP address will only be released
//!    at the end of MaxDrainDurationSeconds" (`UnassignPrivateNatGatewayAddress`) — which
//!    takes it off the gateway and frees it for something else to be given;
//! 4. a request naming a gateway the account does not hold, a gateway whose state or
//!    connectivity does not admit the call, both of the two members that cannot be given
//!    together or neither of them, an address the subnet cannot give, an address the gateway
//!    does not hold, the primary address, or one address past the per-gateway ceiling is
//!    refused with EC2's own error code and changes NOTHING; a request made while an
//!    unassignment drains is refused the same way, and a `DryRun` of either operation
//!    answers `DryRunOperation`.
//!
//! Each table row is a distinct input along what those rules generalise over — how many
//! addresses in one call, whether they are named or counted, which member of the modelled
//! `NatGatewayState` catalog the gateway is in, which connectivity type it has, and whether
//! the drain is the one the request names or the documented default — so no single row is
//! the specification. Everything each row needs is real: the VPC, the subnet and the NAT
//! gateway are all created over the wire.

use std::collections::BTreeMap;
use std::time::Duration;

use ec2::state::{
    CONNECTIVITY_TYPE_PRIVATE, CONNECTIVITY_TYPE_PUBLIC, MAX_SECONDARY_PRIVATE_IPS_PER_NAT_GATEWAY,
    NAT_GATEWAY_ADDRESS_STATUS_ASSIGNING, NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED,
    NAT_GATEWAY_ADDRESS_STATUS_UNASSIGNING, NAT_GATEWAY_STATE_AVAILABLE, TRANSITION_DELAY,
    is_assignable_subnet_address,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The Availability Zone the gateways of this suite sit in.
const ZONE: &str = "us-east-1a";

/// The VPC every row's network is cut out of, and the subnet the gateway sits in: a block of
/// its own, so a private address this suite assigns can only have come out of it.
const VPC_CIDR: &str = "10.42.0.0/16";
const SUBNET_CIDR: &str = "10.42.1.0/24";

/// EC2's error code for a NAT gateway id the account holds no NAT gateway for.
const NAT_GATEWAY_NOT_FOUND: &str = "NatGatewayNotFound";

/// EC2's error code for something that is not a NAT gateway id at all.
const NAT_GATEWAY_MALFORMED: &str = "NatGatewayMalformed";

/// EC2's error code for a resource whose current state does not admit the request.
const INCORRECT_STATE: &str = "IncorrectState";

/// EC2's error code for a member's value that cannot be honoured given the state of the
/// resource it names — as opposed to [`INVALID_PARAMETER_VALUE`], which is about the value
/// itself.
const INVALID_PARAMETER: &str = "InvalidParameter";

/// EC2's error code for a value the service rejects.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// EC2's error code for members that cannot be given together.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// EC2's error code for a parameter the operation needs and the request left out — the code
/// the handler answers for a list that is present and empty, and the one the request walk's
/// own validation answers when a required member is absent altogether.
const MISSING_PARAMETER: &str = "MissingParameter";

/// EC2's error code for a private address something already holds.
const INVALID_IP_ADDRESS_IN_USE: &str = "InvalidIPAddress.InUse";

/// EC2's generic resource-count quota code.
const RESOURCE_COUNT_EXCEEDED: &str = "ResourceCountExceeded";

/// The code a dry run of a request that would have succeeded answers, and the status EC2
/// answers it with.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;

/// A well-formed NAT gateway id this account holds no gateway for.
const ABSENT_NAT_GATEWAY_ID: &str = "nat-0123456789abcdef0";

/// Something that is not a NAT gateway id at all.
const MALFORMED_NAT_GATEWAY_ID: &str = "nat-not-an-id";

/// Three addresses of [`SUBNET_CIDR`] well above the ones the service hands out from the
/// bottom of the block, so a row that names them names addresses nothing holds.
const NAMED_ADDRESSES: [&str; 3] = ["10.42.1.200", "10.42.1.201", "10.42.1.202"];

/// An address of another block altogether: not one the gateway's subnet can give.
const OUTSIDE_SUBNET: &str = "10.99.99.99";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// The members of one reported element, by tag name.
type Reported = BTreeMap<String, String>;

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call of `action` carrying `params`.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// How long a poll for a clock-owed move is given before the test gives up: a generous
/// multiple of the service's own span, so a loaded machine polls more often rather than
/// failing. All it bounds is a hang.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 40
}

/// How long a poll waits before looking again.
fn poll_interval() -> Duration {
    Duration::from_millis(50)
}

/// A VPC and one subnet in it, both created over the wire, as `(vpc id, subnet id)`.
async fn network(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, body) = ec2(router, "CreateVpc", &[p("CidrBlock", VPC_CIDR)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVpc({VPC_CIDR}) must answer 200, got {status} with body {body}"
    );
    let vpc_id = element(&body, "vpcId").unwrap_or_default();
    let (status, body) = ec2(
        router,
        "CreateSubnet",
        &[
            p("VpcId", vpc_id.clone()),
            p("CidrBlock", SUBNET_CIDR),
            p("AvailabilityZone", ZONE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSubnet({SUBNET_CIDR}) must answer 200, \
         got {status} with body {body}"
    );
    let subnet_id = element(&body, "subnetId").unwrap_or_default();
    (vpc_id, subnet_id)
}

/// One NAT gateway of `connectivity` in `subnet_id`, created over the wire: a public one is
/// created with the Elastic IP address it reaches the internet through.
async fn create_gateway(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    connectivity: &str,
) -> String {
    let mut params = vec![
        p("SubnetId", subnet_id.to_string()),
        p("ConnectivityType", connectivity.to_string()),
    ];
    if connectivity == CONNECTIVITY_TYPE_PUBLIC {
        let (status, body) = ec2(router, "AllocateAddress", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: AllocateAddress must answer 200, got {status} with body {body}"
        );
        let allocation_id = element(&body, "allocationId").unwrap_or_default();
        params.push(p("AllocationId", allocation_id));
    }
    let (status, body) = ec2(router, "CreateNatGateway", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateNatGateway({connectivity}) must answer 200, \
         got {status} with body {body}"
    );
    element(&body, "natGatewayId").unwrap_or_default()
}

/// The state `DescribeNatGateways` reports for `nat_gateway_id`.
async fn described_state(case: &str, router: &MultiServiceRouter, nat_gateway_id: &str) -> String {
    let body = describe_ok(case, router, nat_gateway_id).await;
    let parsed = document(case, &body);
    let gateway = reported_gateway(case, &parsed, &body, nat_gateway_id);
    child_text(gateway, "state").unwrap_or_default()
}

/// Look at the gateway AGAIN until a describe reports it serving, or a deadline far past the
/// service's own transition span passes: a wait on the CONDITION, so a slower machine only
/// polls more often.
async fn poll_until_available(case: &str, router: &MultiServiceRouter, nat_gateway_id: &str) {
    let deadline = tokio::time::Instant::now() + poll_deadline();
    loop {
        let state = described_state(case, router, nat_gateway_id).await;
        if state == NAT_GATEWAY_STATE_AVAILABLE {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {nat_gateway_id} must reach \
             {NAT_GATEWAY_STATE_AVAILABLE:?}, last saw {state:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// A private NAT gateway of this suite's own network, created over the wire and polled until
/// it serves: `(subnet id, nat gateway id)`.
async fn serving_private_gateway(
    router: &MultiServiceRouter,
    case: &str,
) -> (String, String) {
    let (_, subnet_id) = network(router, case).await;
    let nat_gateway_id = create_gateway(router, case, &subnet_id, CONNECTIVITY_TYPE_PRIVATE).await;
    poll_until_available(case, router, &nat_gateway_id).await;
    (subnet_id, nat_gateway_id)
}

/// The `DescribeNatGateways` body for exactly `nat_gateway_id`, as that call's own 200.
async fn describe_ok(case: &str, router: &MultiServiceRouter, nat_gateway_id: &str) -> String {
    let (status, body) = ec2(
        router,
        "DescribeNatGateways",
        &indexed("NatGatewayId", &[nat_gateway_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeNatGateways must answer 200, got {status} with body {body}"
    );
    body
}

/// The reported element of `nat_gateway_id` in a parsed describe body.
fn reported_gateway<'a, 'i>(
    case: &str,
    parsed: &'a roxmltree::Document<'i>,
    body: &str,
    nat_gateway_id: &str,
) -> roxmltree::Node<'a, 'i> {
    set_items(parsed.root_element(), "natGatewaySet")
        .into_iter()
        .find(|item| child_text(*item, "natGatewayId").as_deref() == Some(nat_gateway_id))
        .unwrap_or_else(|| panic!("case {case:?}: {nat_gateway_id} is not in {body}"))
}

/// The members of every element of the `name` set inside `node`, by tag name.
fn reported_items(node: roxmltree::Node<'_, '_>, name: &str) -> Vec<Reported> {
    set_items(node, name)
        .into_iter()
        .map(|item| {
            item.children()
                .filter(|member| member.is_element())
                .map(|member| {
                    (
                        member.tag_name().name().to_string(),
                        member.text().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .collect()
}

/// The addresses `DescribeNatGateways` reports for `nat_gateway_id` — the round trip through
/// an implemented reader, so what is asserted is what a later caller is told.
async fn described_addresses(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
) -> Vec<Reported> {
    let body = describe_ok(case, router, nat_gateway_id).await;
    let parsed = document(case, &body);
    let gateway = reported_gateway(case, &parsed, &body, nat_gateway_id);
    reported_items(gateway, "natGatewayAddressSet")
}

/// The addresses a response body reports, in the order it reported them.
fn answered_addresses(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    reported_items(parsed.root_element(), "natGatewayAddressSet")
}

/// The private address of one reported address.
fn private_ip(address: &Reported) -> String {
    address.get("privateIp").cloned().unwrap_or_default()
}

/// The private addresses of `nat_gateway_id` a describe reports, as a set.
async fn described_private_ips(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
) -> std::collections::BTreeSet<String> {
    described_addresses(case, router, nat_gateway_id)
        .await
        .iter()
        .map(private_ip)
        .collect()
}

/// The PRIMARY private address of `nat_gateway_id`: the one its create gave it.
async fn primary_private_ip(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
) -> String {
    described_addresses(case, router, nat_gateway_id)
        .await
        .into_iter()
        .find(|address| address.get("isPrimary").map(String::as_str) == Some("true"))
        .map(|address| private_ip(&address))
        .unwrap_or_else(|| panic!("case {case:?}: {nat_gateway_id} must hold a primary address"))
}

/// One `AssignPrivateNatGatewayAddress` call carrying `params`.
async fn assign(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    ec2(router, "AssignPrivateNatGatewayAddress", params).await
}

/// One `UnassignPrivateNatGatewayAddress` call carrying `params`.
async fn unassign(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    ec2(router, "UnassignPrivateNatGatewayAddress", params).await
}

/// The form parameters of an assignment naming `addresses` on `nat_gateway_id`.
fn named_params(nat_gateway_id: &str, addresses: &[&str]) -> Params {
    let mut params = vec![p("NatGatewayId", nat_gateway_id.to_string())];
    params.extend(indexed("PrivateIpAddress", addresses));
    params
}

/// The form parameters of an assignment asking the service to choose `count` addresses.
fn counted_params(nat_gateway_id: &str, count: usize) -> Params {
    vec![
        p("NatGatewayId", nat_gateway_id.to_string()),
        p("PrivateIpAddressCount", count.to_string()),
    ]
}

// ---------------------------------------------------------------------------
// What an assignment stores
// ---------------------------------------------------------------------------

/// Rule 1: every address an assignment adds is stored on the gateway — on the gateway's own
/// network interface, as a secondary address out of the subnet's block, in the transient
/// `assigning` status and carrying none of the members only a public gateway's address has —
/// and the very next describe reports the same records beside the primary one.
#[tokio::test]
async fn an_assignment_stores_each_address_on_the_gateway() {
    /// One row: `(case, the addresses the request names, the count it asks for instead)`.
    type Row = (&'static str, &'static [&'static str], usize);

    let rows: &[Row] = &[
        ("one address named", &["10.42.1.200"], 0),
        ("three addresses named at once", &NAMED_ADDRESSES, 0),
        ("one address the service chooses", &[], 1),
        ("four addresses the service chooses", &[], 4),
    ];

    for (case, named, count) in rows {
        let router = emulator();
        let (_, nat_gateway_id) = serving_private_gateway(&router, case).await;
        let primary = primary_private_ip(case, &router, &nat_gateway_id).await;
        let interface = described_addresses(case, &router, &nat_gateway_id)
            .await
            .first()
            .and_then(|address| address.get("networkInterfaceId").cloned())
            .unwrap_or_else(|| panic!("case {case:?}: the gateway must hold an interface"));

        let params = if *count == 0 {
            named_params(&nat_gateway_id, named)
        } else {
            counted_params(&nat_gateway_id, *count)
        };
        let (status, body) = assign(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the assignment must answer 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "natGatewayId").as_deref(),
            Some(nat_gateway_id.as_str()),
            "case {case:?}: expected the gateway's own id, got body {body}"
        );

        let answered = answered_addresses(case, &body);
        let expected_count = if *count == 0 { named.len() } else { *count };
        assert_eq!(
            answered.len(),
            expected_count,
            "case {case:?}: expected {expected_count} assigned address(es), got {answered:?}"
        );
        for address in &answered {
            let assigned = private_ip(address);
            assert!(
                is_assignable_subnet_address(SUBNET_CIDR, &assigned),
                "case {case:?}: {assigned:?} must be an assignable address of \
                 {SUBNET_CIDR}, got {address:?}"
            );
            assert_ne!(
                assigned, primary,
                "case {case:?}: an assignment must not hand out the primary address again, \
                 got {address:?}"
            );
            assert_eq!(
                address.get("status").map(String::as_str),
                Some(NAT_GATEWAY_ADDRESS_STATUS_ASSIGNING),
                "case {case:?}: expected the transient status of an assignment, \
                 got {address:?}"
            );
            assert_eq!(
                address.get("isPrimary").map(String::as_str),
                Some("false"),
                "case {case:?}: an assigned address is a secondary one, got {address:?}"
            );
            assert_eq!(
                address.get("networkInterfaceId").map(String::as_str),
                Some(interface.as_str()),
                "case {case:?}: expected the gateway's own interface {interface:?}, \
                 got {address:?}"
            );
            assert_eq!(
                address.get("allocationId"),
                None,
                "case {case:?}: a private address associates no Elastic IP address, \
                 got {address:?}"
            );
            assert_eq!(
                address.get("associationId"),
                None,
                "case {case:?}: a private address carries no association id, got {address:?}"
            );
            assert_eq!(
                address.get("publicIp"),
                None,
                "case {case:?}: a private address carries no public address, got {address:?}"
            );
        }
        if *count == 0 {
            let assigned: Vec<String> = answered.iter().map(private_ip).collect();
            assert_eq!(
                assigned,
                named.iter().map(|value| (*value).to_string()).collect::<Vec<_>>(),
                "case {case:?}: expected the addresses the request named, got {answered:?}"
            );
        }

        // The round trip: the gateway now holds its primary address and every assigned one,
        // and nothing else.
        let described = described_addresses(case, &router, &nat_gateway_id).await;
        assert_eq!(
            described.len(),
            expected_count + 1,
            "case {case:?}: expected the primary address and {expected_count} assigned \
             one(s), got {described:?}"
        );
        for address in &answered {
            let assigned = private_ip(address);
            let stored = described
                .iter()
                .find(|held| private_ip(held) == assigned)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {assigned:?} must be reported, got {described:?}")
                });
            assert_eq!(
                stored.get("networkInterfaceId"),
                address.get("networkInterfaceId"),
                "case {case:?}: the stored address must carry the interface the call \
                 answered, got {stored:?}"
            );
            assert_eq!(
                stored.get("isPrimary"),
                address.get("isPrimary"),
                "case {case:?}: the stored address must be secondary, got {stored:?}"
            );
        }
    }
}

/// Rule 2: the clock finishes the assignment — an address answered `assigning` reaches
/// `succeeded` without anybody assigning it again, and polling it is what materialises that.
#[tokio::test]
async fn an_assigned_address_reaches_succeeded_on_its_own() {
    let case = "an assignment left to the clock";
    let router = emulator();
    let (_, nat_gateway_id) = serving_private_gateway(&router, case).await;
    let (status, body) = assign(&router, &counted_params(&nat_gateway_id, 2)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the assignment must answer 200, got {status} with body {body}"
    );
    let assigned: Vec<String> = answered_addresses(case, &body)
        .iter()
        .map(private_ip)
        .collect();
    assert_eq!(
        assigned.len(),
        2,
        "case {case:?}: expected two assigned addresses, got {assigned:?}"
    );

    // A wait on the CONDITION rather than on a span: the move is the clock's, so a slower
    // machine only polls more often.
    let deadline = tokio::time::Instant::now() + poll_deadline();
    loop {
        let described = described_addresses(case, &router, &nat_gateway_id).await;
        let settled = assigned.iter().all(|address| {
            described
                .iter()
                .any(|held| &private_ip(held) == address
                    && held.get("status").map(String::as_str)
                        == Some(NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED))
        });
        if settled {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: every assigned address must reach \
             {NAT_GATEWAY_ADDRESS_STATUS_SUCCEEDED:?}, last saw {described:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Rule 4: the per-gateway ceiling is asked of what the gateway would HOLD — the published
/// maximum of the count member — so the address past it is refused and the gateway keeps
/// exactly what it had.
#[tokio::test]
async fn the_per_gateway_ceiling_bounds_what_a_gateway_holds() {
    let case = "a gateway filled to the ceiling";
    let router = emulator();
    let (_, nat_gateway_id) = serving_private_gateway(&router, case).await;
    let (status, body) = assign(
        &router,
        &counted_params(&nat_gateway_id, MAX_SECONDARY_PRIVATE_IPS_PER_NAT_GATEWAY),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an assignment up to the ceiling must answer 200, \
         got {status} with body {body}"
    );
    let held = described_addresses(case, &router, &nat_gateway_id).await;
    assert_eq!(
        held.len(),
        MAX_SECONDARY_PRIVATE_IPS_PER_NAT_GATEWAY + 1,
        "case {case:?}: expected the primary address and \
         {MAX_SECONDARY_PRIVATE_IPS_PER_NAT_GATEWAY} assigned ones, got {} addresses",
        held.len()
    );

    for (case, params) in [
        (
            "one more address, counted",
            counted_params(&nat_gateway_id, 1),
        ),
        (
            "one more address, named",
            named_params(&nat_gateway_id, &NAMED_ADDRESSES[..1]),
        ),
    ] {
        let (status, body) = assign(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the address past the ceiling must be refused, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            RESOURCE_COUNT_EXCEEDED,
            "case {case:?}: expected {RESOURCE_COUNT_EXCEEDED}, got body {body}"
        );
        let after = described_addresses(case, &router, &nat_gateway_id).await;
        assert_eq!(
            after.len(),
            held.len(),
            "case {case:?}: a refused assignment must store nothing, got {} addresses",
            after.len()
        );
    }
}

// ---------------------------------------------------------------------------
// What an assignment refuses
// ---------------------------------------------------------------------------

/// Rule 4: every refusal carries EC2's own code for its cause and leaves the gateway exactly
/// as it was.
#[tokio::test]
async fn an_assignment_is_refused_and_stores_nothing() {
    let case = "the refusal table";
    let router = emulator();
    let (_, subnet_id) = network(&router, case).await;
    // Three gateways, so a row can name one whose connectivity or whose state does not admit
    // the call: they come up together, which is one span for all of them rather than one each.
    let private_id = create_gateway(&router, case, &subnet_id, CONNECTIVITY_TYPE_PRIVATE).await;
    let public_id = create_gateway(&router, case, &subnet_id, CONNECTIVITY_TYPE_PUBLIC).await;
    let going_id = create_gateway(&router, case, &subnet_id, CONNECTIVITY_TYPE_PRIVATE).await;
    poll_until_available(case, &router, &private_id).await;
    poll_until_available(case, &router, &public_id).await;
    poll_until_available(case, &router, &going_id).await;
    let (status, body) = ec2(
        &router,
        "DeleteNatGateway",
        &[p("NatGatewayId", going_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteNatGateway must answer 200, got {status} with body {body}"
    );
    let primary = primary_private_ip(case, &router, &private_id).await;

    /// One row: `(case, the request's parameters, the expected error code)`.
    type Row = (&'static str, Params, &'static str);

    let rows: Vec<Row> = vec![
        (
            "a gateway the account does not hold",
            named_params(ABSENT_NAT_GATEWAY_ID, &NAMED_ADDRESSES[..1]),
            NAT_GATEWAY_NOT_FOUND,
        ),
        (
            "something that is not a gateway id",
            named_params(MALFORMED_NAT_GATEWAY_ID, &NAMED_ADDRESSES[..1]),
            NAT_GATEWAY_MALFORMED,
        ),
        (
            "a public gateway",
            named_params(&public_id, &NAMED_ADDRESSES[..1]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "a gateway on its way out",
            named_params(&going_id, &NAMED_ADDRESSES[..1]),
            INCORRECT_STATE,
        ),
        (
            "both the named addresses and a count",
            {
                let mut params = named_params(&private_id, &NAMED_ADDRESSES[..1]);
                params.push(p("PrivateIpAddressCount", "1"));
                params
            },
            INVALID_PARAMETER_COMBINATION,
        ),
        (
            "neither the named addresses nor a count",
            vec![p("NatGatewayId", private_id.clone())],
            MISSING_PARAMETER,
        ),
        (
            "an empty address list",
            named_params(&private_id, &[]),
            MISSING_PARAMETER,
        ),
        (
            "a value that is not an IPv4 address",
            named_params(&private_id, &["not-an-address"]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an address of another block",
            named_params(&private_id, &[OUTSIDE_SUBNET]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "the address the gateway already holds",
            named_params(&private_id, &[primary.as_str()]),
            INVALID_IP_ADDRESS_IN_USE,
        ),
        (
            "one address named twice",
            named_params(&private_id, &[NAMED_ADDRESSES[0], NAMED_ADDRESSES[0]]),
            INVALID_IP_ADDRESS_IN_USE,
        ),
    ];

    let before = described_private_ips(case, &router, &private_id).await;
    for (row, params, expected_code) in rows {
        let (status, body) = assign(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {row:?}: the request must be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {row:?}: expected {expected_code}, got body {body}"
        );
        let after = described_private_ips(case, &router, &private_id).await;
        assert_eq!(
            after, before,
            "case {row:?}: a refused assignment must store nothing, got {after:?}"
        );
    }

    // A dry run of a request that WOULD have succeeded: the code the flag documents, and
    // nothing assigned.
    let row = "a dry run that would have succeeded";
    let mut params = named_params(&private_id, &NAMED_ADDRESSES[..1]);
    params.push(p("DryRun", "true"));
    let (status, body) = assign(&router, &params).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {row:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "case {row:?}: expected {DRY_RUN_OPERATION}, got body {body}"
    );
    let after = described_private_ips(case, &router, &private_id).await;
    assert_eq!(
        after, before,
        "case {row:?}: a dry run must store nothing, got {after:?}"
    );
}

// ---------------------------------------------------------------------------
// What an unassignment does
// ---------------------------------------------------------------------------

/// Rule 3: an unassignment leaves the address on the gateway while its connections drain,
/// and the clock RELEASES it at the end of the drain the request asked for — off the gateway
/// and free for the subnet to give again.
#[tokio::test]
async fn an_unassignment_drains_then_releases_the_address() {
    let case = "an address unassigned with the shortest drain the model admits";
    let router = emulator();
    let (_, nat_gateway_id) = serving_private_gateway(&router, case).await;
    let (status, body) = assign(
        &router,
        &named_params(&nat_gateway_id, &NAMED_ADDRESSES[..2]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the assignment must answer 200, got {status} with body {body}"
    );

    let going = NAMED_ADDRESSES[0];
    let kept = NAMED_ADDRESSES[1];
    let mut params = named_params(&nat_gateway_id, &[going]);
    // The shortest drain `MaxDrainDurationSeconds` admits (`@range(min: 1, max: 4000)`), so
    // the release this row waits for is the one the REQUEST asked for.
    params.push(p("MaxDrainDurationSeconds", "1"));
    let (status, body) = unassign(&router, &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the unassignment must answer 200, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "natGatewayId").as_deref(),
        Some(nat_gateway_id.as_str()),
        "case {case:?}: expected the gateway's own id, got body {body}"
    );
    let answered = answered_addresses(case, &body);
    assert_eq!(
        answered.len(),
        1,
        "case {case:?}: expected the one address being unassigned, got {answered:?}"
    );
    assert_eq!(
        answered.first().map(private_ip).as_deref(),
        Some(going),
        "case {case:?}: expected {going}, got {answered:?}"
    );
    assert_eq!(
        answered
            .first()
            .and_then(|address| address.get("status"))
            .map(String::as_str),
        Some(NAT_GATEWAY_ADDRESS_STATUS_UNASSIGNING),
        "case {case:?}: expected the transient status of an unassignment, got {answered:?}"
    );

    // The clock releases it: looked at AGAIN until the gateway no longer holds it, so what is
    // waited for is the CONDITION and not a span.
    let deadline = tokio::time::Instant::now() + poll_deadline();
    loop {
        let held = described_private_ips(case, &router, &nat_gateway_id).await;
        if !held.contains(going) {
            assert!(
                held.contains(kept),
                "case {case:?}: the address nobody unassigned must stay, got {held:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {going} must be released at the end of its drain, \
             last saw {held:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }

    // Released means FREE: the subnet can give the same address to the gateway again.
    let case = "the released address assigned again";
    let (status, body) = assign(&router, &named_params(&nat_gateway_id, &[going])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the released address must be assignable again, \
         got {status} with body {body}"
    );
    let held = described_private_ips(case, &router, &nat_gateway_id).await;
    assert!(
        held.contains(going),
        "case {case:?}: expected {going} back on the gateway, got {held:?}"
    );
}

/// Rule 4: an unassignment refuses the primary address, an address the gateway does not
/// hold, a malformed or absent gateway and a request made while another unassignment drains
/// — and changes nothing. The drain of that row is the DOCUMENTED default, which no row
/// waits for.
#[tokio::test]
async fn an_unassignment_is_refused_and_changes_nothing() {
    let case = "the unassignment refusal table";
    let router = emulator();
    let (_, subnet_id) = network(&router, case).await;
    let private_id = create_gateway(&router, case, &subnet_id, CONNECTIVITY_TYPE_PRIVATE).await;
    let public_id = create_gateway(&router, case, &subnet_id, CONNECTIVITY_TYPE_PUBLIC).await;
    poll_until_available(case, &router, &private_id).await;
    poll_until_available(case, &router, &public_id).await;
    let (status, body) = assign(&router, &named_params(&private_id, &NAMED_ADDRESSES)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the assignment must answer 200, got {status} with body {body}"
    );
    let primary = primary_private_ip(case, &router, &private_id).await;

    /// One row: `(case, the request's parameters, the expected error code)`.
    type Row = (&'static str, Params, &'static str);

    let rows: Vec<Row> = vec![
        (
            "a gateway the account does not hold",
            named_params(ABSENT_NAT_GATEWAY_ID, &NAMED_ADDRESSES[..1]),
            NAT_GATEWAY_NOT_FOUND,
        ),
        (
            "something that is not a gateway id",
            named_params(MALFORMED_NAT_GATEWAY_ID, &NAMED_ADDRESSES[..1]),
            NAT_GATEWAY_MALFORMED,
        ),
        (
            "a public gateway",
            named_params(&public_id, &NAMED_ADDRESSES[..1]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "the primary address",
            named_params(&private_id, &[primary.as_str()]),
            INVALID_PARAMETER,
        ),
        (
            "an address the gateway does not hold",
            named_params(&private_id, &[OUTSIDE_SUBNET]),
            INVALID_PARAMETER,
        ),
        (
            "a value that is not an IPv4 address",
            named_params(&private_id, &["not-an-address"]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "one address named twice",
            named_params(&private_id, &[NAMED_ADDRESSES[0], NAMED_ADDRESSES[0]]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "an empty address list",
            named_params(&private_id, &[]),
            MISSING_PARAMETER,
        ),
        (
            "an address it holds beside one it does not",
            named_params(&private_id, &[NAMED_ADDRESSES[0], OUTSIDE_SUBNET]),
            INVALID_PARAMETER,
        ),
    ];

    let before = described_addresses(case, &router, &private_id).await;
    for (row, params, expected_code) in rows {
        let (status, body) = unassign(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {row:?}: the request must be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {row:?}: expected {expected_code}, got body {body}"
        );
        let after = described_addresses(case, &router, &private_id).await;
        assert_eq!(
            after, before,
            "case {row:?}: a refused unassignment must change nothing, got {after:?}"
        );
    }

    // The WORDING of the primary-address refusal is part of the contract: the sentence
    // states that the address is the NAT's primary private address and that unassigning the
    // primary address is not supported, and its only variable part is the address the
    // request named.
    let row = "the wording of the primary-address refusal";
    let (status, body) = unassign(&router, &named_params(&private_id, &[primary.as_str()])).await;
    assert_eq!(
        status, 400,
        "case {row:?}: the request must be refused, got {status} with body {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Private IP address {primary} is primary private IP of NAT, \
             unassigning primary IP is not supported."
        ),
        "case {row:?}: expected the sentence AWS words this refusal with, got body {body}"
    );

    // A dry run of an unassignment that WOULD have succeeded changes nothing either.
    let row = "a dry run that would have succeeded";
    let mut params = named_params(&private_id, &NAMED_ADDRESSES[..1]);
    params.push(p("DryRun", "true"));
    let (status, body) = unassign(&router, &params).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {row:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "case {row:?}: expected {DRY_RUN_OPERATION}, got body {body}"
    );
    let after = described_addresses(case, &router, &private_id).await;
    assert_eq!(
        after, before,
        "case {row:?}: a dry run must change nothing, got {after:?}"
    );

    // "While unassigning is in progress, you cannot assign/unassign additional IP addresses
    // while the connections are being drained": the drain here is the documented default, so
    // nothing below waits for it to run out.
    let row = "an unassignment with the default drain";
    let (status, body) = unassign(&router, &named_params(&private_id, &[NAMED_ADDRESSES[0]])).await;
    assert_eq!(
        status, 200,
        "case {row:?}: the unassignment must answer 200, got {status} with body {body}"
    );
    for (row, params, is_unassign) in [
        (
            "another unassignment while one drains",
            named_params(&private_id, &[NAMED_ADDRESSES[1]]),
            true,
        ),
        (
            "an assignment while an unassignment drains",
            counted_params(&private_id, 1),
            false,
        ),
    ] {
        let (status, body) = if is_unassign {
            unassign(&router, &params).await
        } else {
            assign(&router, &params).await
        };
        assert_eq!(
            status, 400,
            "case {row:?}: the request must be refused while the connections drain, \
             got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INCORRECT_STATE,
            "case {row:?}: expected {INCORRECT_STATE}, got body {body}"
        );
    }
    // The address being drained is still ON the gateway, in the transient status: its release
    // is owed at the end of the default drain, which is far longer than this test runs.
    let held = described_addresses(case, &router, &private_id).await;
    let draining = held
        .iter()
        .find(|address| private_ip(address) == NAMED_ADDRESSES[0])
        .unwrap_or_else(|| panic!("case {case:?}: the draining address must stay, got {held:?}"));
    assert_eq!(
        draining.get("status").map(String::as_str),
        Some(NAT_GATEWAY_ADDRESS_STATUS_UNASSIGNING),
        "case {case:?}: expected the transient status of an unassignment, got {draining:?}"
    );
    assert_eq!(
        held.len(),
        before.len(),
        "case {case:?}: a drain takes no address off the gateway, got {held:?}"
    );
}
