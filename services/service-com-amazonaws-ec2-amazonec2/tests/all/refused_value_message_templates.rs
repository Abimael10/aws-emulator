// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SENTENCES EC2 refuses a value it could not resolve with, driven
//! over the wire the service actually speaks — `ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML (README.md / OPERATIONS.md
//! "Wire protocol"), through the crate's shared [`super::ec2_query`] harness.
//!
//! An error MESSAGE is part of the contract: a client branches on the code and an operator
//! reads the sentence, and both are AWS's rather than this emulator's to word. Each rule below
//! is one sentence TEMPLATE — fixed text plus the values the request or the store supplies —
//! asserted over a table of inputs so that no single value is the specification:
//!
//! 1. AN INSTANCE ID AN OPERATION CANNOT READ names the resource kind: an operation that
//!    resolves the ONE instance it was given answers `InvalidInstanceID.Malformed` with
//!    "The instance ID '<id>' is malformed", while a well-formed id the account holds no
//!    instance for stays the other half of that order,
//!    `InvalidInstanceID.NotFound` / "The instance ID '<id>' does not exist". The describes
//!    that DECODE the obfuscated long id at the request boundary keep that boundary's own
//!    quoted template, which this suite pins as the contrast
//!    ([`super::malformed_id_message_template`] owns that template).
//! 2. AN UNKNOWN NETWORK INSIGHTS PATH is reported by the id ALONE — "<id> does not exist" —
//!    by every operation that SELECTS paths with it, while the one that ACTS on the path it
//!    is given states that the `NetworkInsightsPathId` it was given is not a valid one.
//! 3. A MEMBER THAT HAS TO NAME A REGION is refused with "Region name '<value>' is invalid.",
//!    a sentence about the VALUE rather than about the member it arrived in, so one wording
//!    covers every such member of the service.
//! 4. A SUBNET WITH NO FREE ADDRESS LEFT is `InsufficientFreeAddressesInSubnet` with fixed
//!    text end to end: "The specified subnet does not have enough free addresses to satisfy
//!    the request."
//! 5. A ROUTE TARGET THE ACCOUNT DOES NOT HOLD is reported by the MEMBER that carried it —
//!    "The transitGateway ID '<id>' does not exist." — where the operations that act on the
//!    transit gateway ITSELF keep that resource's own sentence, which this suite pins as the
//!    contrast.
//!
//! Every table row is a distinct input along what its rule generalises over: the operation
//! answering, the way the value fails to resolve, the id FORM (both widths EC2 issues), the
//! size of the subnet and the count asked of it. Nothing is asserted about a value AWS
//! generates per run.

use ec2::state::{
    INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND, INVALID_PARAMETER_VALUE,
    INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for. None of the rules depends on it: each refusal is
/// about a value the request carried.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

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

/// Assert one refusal: the status, the code and the whole sentence.
async fn assert_refusal(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    expected_code: &str,
    expected_message: &str,
) {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} from {action}, got {status} with body \
         {body}"
    );
    let answered_code = error_code(&body);
    assert_eq!(
        answered_code, expected_code,
        "case {case:?}: expected the code {expected_code:?} from {action}, got \
         {answered_code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected the message {expected_message:?} from {action}, got \
         {message:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 1: an instance id an operation cannot read
// ---------------------------------------------------------------------------

/// The code EC2 answers for a value that is no instance id EC2 could have issued.
const INSTANCE_ID_MALFORMED: &str = "InvalidInstanceID.Malformed";

/// The code EC2 answers for an instance id in a published form the account holds no instance
/// for.
const INSTANCE_ID_NOT_FOUND: &str = "InvalidInstanceID.NotFound";

/// The operations that resolve the ONE instance they were given, with the form field each
/// names it under — a stop, a start, a terminate, a reboot and a password read, so the
/// sentence belongs to the id rather than to one handler.
const INSTANCE_RESOLVERS: [(&str, &str); 5] = [
    ("StopInstances", "InstanceId.1"),
    ("StartInstances", "InstanceId.1"),
    ("TerminateInstances", "InstanceId.1"),
    ("RebootInstances", "InstanceId.1"),
    ("GetPasswordData", "InstanceId"),
];

/// `(case, the value)`: every row is outside the form EC2 issues an instance id in, each for a
/// different reason — the length, the leading digit, the alphabet, the prefix, the all-zero
/// placeholder and free text.
fn unreadable_instance_ids() -> [(&'static str, &'static str); 7] {
    [
        ("free text", "not-an-instance"),
        ("another resource kind's prefix", "vol-0a1b2c3d4e5f60718"),
        ("the prefix with no digits at all", "i-"),
        ("one digit short of the short form", "i-0a1b2c3"),
        (
            "a long form whose leading digit is above zero, which EC2 does not issue",
            "i-9a1b2c3d4e5f60718",
        ),
        ("upper-case digits, which no issued id carries", "i-0A1B2C3D"),
        (
            "the all-zero long id, which the mint counting from one never produces",
            "i-00000000000000000",
        ),
    ]
}

/// RULE 1: an operation that resolves the one instance it was given words an id it cannot read
/// with the resource kind, and still tells that refusal apart from an id in a published form
/// that names no instance.
#[tokio::test]
async fn an_unreadable_instance_id_is_refused_with_the_instance_kind_sentence() {
    let router = emulator();

    for (case, instance_id) in unreadable_instance_ids() {
        for (action, field) in INSTANCE_RESOLVERS {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &[param(field, instance_id)],
                INSTANCE_ID_MALFORMED,
                &format!("The instance ID '{instance_id}' is malformed"),
            )
            .await;
        }
    }

    // The other half of the order the same operations keep: an id in EITHER published form
    // that names no instance of the account is the resource that is not there, not a value
    // that could not be read.
    let absent: [(&str, &str); 2] = [
        ("the short published form", "i-0f3c71de"),
        ("the long published form", "i-0a1b2c3d4e5f60718"),
    ];
    for (case, instance_id) in absent {
        for (action, field) in INSTANCE_RESOLVERS {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &[param(field, instance_id)],
                INSTANCE_ID_NOT_FOUND,
                &format!("The instance ID '{instance_id}' does not exist"),
            )
            .await;
        }
    }
}

/// The kind sentence is the wording of the operations that RESOLVE one instance: the describe
/// that decodes the obfuscated long id at the request boundary keeps that boundary's quoted
/// template for the very same values, which is what tells a caller which validator refused it.
#[tokio::test]
async fn the_request_boundary_keeps_its_own_template_for_the_same_values() {
    let router = emulator();

    for (case, instance_id) in unreadable_instance_ids() {
        assert_refusal(
            &format!("{case} (DescribeInstances)"),
            &router,
            "DescribeInstances",
            &[param("InstanceId.1", instance_id)],
            INSTANCE_ID_MALFORMED,
            &format!("Invalid id: \"{instance_id}\""),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// RULE 2: an unknown network insights path
// ---------------------------------------------------------------------------

/// The operations that resolve a `NetworkInsightsPathId` by SELECTING paths with it, with the
/// form field each names it under: the two describes, so the sentence belongs to the resource
/// rather than to one handler. `StartNetworkInsightsAnalysis` ACTS on the one path it is
/// given and answers the other refusal of the pair, which is asserted on its own below.
fn path_resolvers(path_id: &str) -> Vec<(&'static str, Vec<Param>)> {
    vec![
        (
            "DescribeNetworkInsightsPaths",
            vec![param("NetworkInsightsPathId.1", path_id)],
        ),
        (
            "DescribeNetworkInsightsAnalyses",
            vec![param("NetworkInsightsPathId", path_id)],
        ),
    ]
}

/// RULE 2: a path id the account holds no path for is reported by the id ALONE by every
/// operation that SELECTS paths with it, while the start of an analysis — which acts on the
/// one path it is given — states that the `NetworkInsightsPathId` it was given is not valid;
/// and a refused call leaves the account's paths as they were.
#[tokio::test]
async fn an_unknown_network_insights_path_is_reported_by_its_id_alone() {
    let router = emulator();

    // Both widths EC2 issues a resource id in, and a third payload in the long one, so no one
    // id is the specification.
    let unknown: [(&str, &str); 3] = [
        ("the long id form", "nip-0123456789abcdef0"),
        ("the short id form", "nip-0a1b2c3d"),
        ("another long-form payload", "nip-0ffffffffffffffff"),
    ];
    for (case, path_id) in unknown {
        for (action, params) in path_resolvers(path_id) {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &params,
                INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
                &format!("{path_id} does not exist"),
            )
            .await;
        }
        // The start of an analysis acts on the ONE path it is given, so it states that the
        // `NetworkInsightsPathId` it was given is not a valid one — one answer for every id
        // it cannot resolve, whatever its form. Its model declares `ClientToken` `@required`,
        // and a distinct token per row, so no row is answered as a replay of another.
        assert_refusal(
            &format!("{case} (StartNetworkInsightsAnalysis)"),
            &router,
            "StartNetworkInsightsAnalysis",
            &[
                param("NetworkInsightsPathId", path_id),
                param("ClientToken", &format!("one-analysis-per-token-{path_id}")),
            ],
            INVALID_PARAMETER_VALUE,
            &format!("NetworkInsightsPathId ({path_id}) is not valid"),
        )
        .await;
    }

    // The refusals above are about the ids: the account's own report is still served, and
    // reports no path, so nothing a refusal touched was left behind.
    let reported = ok(
        "the account's paths",
        &router,
        "DescribeNetworkInsightsPaths",
        &[],
    )
    .await;
    assert!(
        !reported.contains("<networkInsightsPathId>"),
        "the account holds no path, so the report must name none, got {reported}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: a member that has to name a Region
// ---------------------------------------------------------------------------

/// `(case, the value)`: every row is a value that names no AWS Region, each for a different
/// reason. Source: the Region catalog this service holds such a member to
/// (`ec2::state::is_catalog_region`), which no row below is in.
fn values_that_name_no_region() -> [(&'static str, &'static str); 5] {
    [
        ("a Region code with no number", "us-east"),
        ("words that are not a Region code", "not-a-region"),
        ("an uppercase Region code", "US-EAST-1"),
        ("an Availability Zone rather than a Region", "us-west-2a"),
        ("a Region code of no Region AWS has", "xx-central-1"),
    ]
}

/// RULE 3: every member of this service that has to name a Region refuses a value that names
/// none with `InvalidParameterValue` and the one sentence about that value.
#[tokio::test]
async fn a_member_that_has_to_name_a_region_is_refused_with_the_region_sentence() {
    let router = emulator();

    // The peering connection resolves a VPC of the account beside its peer Region, so the
    // refusals below are about the Region and not about a VPC that is not there.
    let vpc = ok(
        "the peering connection's own vpc",
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.40.0.0/16")],
    )
    .await;
    let vpc_id = member("the peering connection's own vpc", &vpc, "vpcId");

    for (case, value) in values_that_name_no_region() {
        // The members that have to name a Region, each on the operation that carries it: the
        // Region a copy's source is in (three resources), the Region a peer VPC is in, and a
        // Region an IPAM is asked to operate in.
        let rows: Vec<(&str, Vec<Param>)> = vec![
            (
                "CopyFpgaImage",
                vec![
                    param("SourceFpgaImageId", "afi-0123456789abcdef0"),
                    param("SourceRegion", value),
                ],
            ),
            (
                "CopySnapshot",
                vec![
                    param("SourceSnapshotId", "snap-0123456789abcdef0"),
                    param("SourceRegion", value),
                ],
            ),
            (
                "CopyImage",
                vec![
                    param("Name", "a-copy-of-the-image"),
                    param("SourceImageId", "ami-0123456789abcdef0"),
                    param("SourceRegion", value),
                ],
            ),
            (
                "CreateVpcPeeringConnection",
                vec![
                    param("VpcId", &vpc_id),
                    param("PeerVpcId", &vpc_id),
                    param("PeerRegion", value),
                ],
            ),
            (
                "CreateIpam",
                vec![param("OperatingRegion.1.RegionName", value)],
            ),
        ];
        for (action, params) in rows {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &params,
                "InvalidParameterValue",
                &format!("Region name '{value}' is invalid."),
            )
            .await;
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 4: a subnet with no free address left
// ---------------------------------------------------------------------------

/// The code EC2 answers when a subnet has fewer free addresses than the request asks for.
const INSUFFICIENT_FREE_ADDRESSES: &str = "InsufficientFreeAddressesInSubnet";

/// The whole sentence that code carries: fixed text end to end, naming neither the subnet nor
/// the count asked for.
const INSUFFICIENT_FREE_ADDRESSES_MESSAGE: &str =
    "The specified subnet does not have enough free addresses to satisfy the request.";

/// How many addresses AWS reserves in every subnet block ("the first four IP addresses and the
/// last IP address in each subnet CIDR block", Amazon VPC User Guide), so a block of `n`
/// addresses has `n - 5` to assign.
const RESERVED_ADDRESSES_PER_SUBNET: u32 = 5;

/// RULE 4: a request for more addresses than its subnet has left is refused with the fixed
/// sentence — on the create of an interface and on an assignment to one that exists, and in
/// subnets of different sizes.
#[tokio::test]
async fn a_subnet_without_enough_free_addresses_is_refused_with_fixed_text() {
    let router = emulator();

    let vpc = ok(
        "the subnets' vpc",
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.50.0.0/16")],
    )
    .await;
    let vpc_id = member("the subnets' vpc", &vpc, "vpcId");

    // Two subnet sizes, so the rule is about what the block has LEFT rather than about one
    // block: a /28 (16 addresses) and a /27 (32), each asked for one more address than it can
    // assign.
    let sizes: [(&str, &str, u32); 2] = [
        ("a /28 subnet, the smallest EC2 admits", "10.50.1.0/28", 16),
        ("a /27 subnet", "10.50.2.0/27", 32),
    ];
    for (case, cidr_block, addresses) in sizes {
        let assignable = addresses - RESERVED_ADDRESSES_PER_SUBNET;
        let subnet = ok(
            case,
            &router,
            "CreateSubnet",
            &[param("VpcId", &vpc_id), param("CidrBlock", cidr_block)],
        )
        .await;
        let subnet_id = member(case, &subnet, "subnetId");

        // A create that asks for a primary address and `assignable` secondary ones is one
        // address past what the block can assign.
        assert_refusal(
            &format!("{case} (CreateNetworkInterface)"),
            &router,
            "CreateNetworkInterface",
            &[
                param("SubnetId", &subnet_id),
                param(
                    "SecondaryPrivateIpAddressCount",
                    &assignable.to_string(),
                ),
            ],
            INSUFFICIENT_FREE_ADDRESSES,
            INSUFFICIENT_FREE_ADDRESSES_MESSAGE,
        )
        .await;

        // The same cause reached through an assignment to an interface that already holds the
        // block's primary address: `assignable` more takes it one past too.
        let interface = ok(
            case,
            &router,
            "CreateNetworkInterface",
            &[param("SubnetId", &subnet_id)],
        )
        .await;
        let network_interface_id = member(case, &interface, "networkInterfaceId");
        assert_refusal(
            &format!("{case} (AssignPrivateIpAddresses)"),
            &router,
            "AssignPrivateIpAddresses",
            &[
                param("NetworkInterfaceId", &network_interface_id),
                param(
                    "SecondaryPrivateIpAddressCount",
                    &assignable.to_string(),
                ),
            ],
            INSUFFICIENT_FREE_ADDRESSES,
            INSUFFICIENT_FREE_ADDRESSES_MESSAGE,
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// RULE 5: a route target the account does not hold
// ---------------------------------------------------------------------------

/// RULE 5: a route pointed at a transit gateway the account does not hold is refused by the
/// MEMBER that carried the id — on the create of a route and on the replacement of one — while
/// the operations that act on the transit gateway itself keep that resource's own sentence.
#[tokio::test]
async fn a_route_to_an_absent_transit_gateway_names_the_member_it_arrived_in() {
    let router = emulator();
    let case = "the route table the routes live in";

    let vpc = ok(case, &router, "CreateVpc", &[param("CidrBlock", "10.60.0.0/16")]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let table = ok(case, &router, "CreateRouteTable", &[param("VpcId", &vpc_id)]).await;
    let route_table_id = member(case, &table, "routeTableId");

    // A gateway the account DOES hold, ATTACHED to that VPC, so the route replaced below has a
    // target to begin with — a transit gateway carries a VPC's traffic only through an
    // attachment to it — and the refusals are about the id the request named.
    let gateway = ok(case, &router, "CreateTransitGateway", &[]).await;
    let transit_gateway_id = member(case, &gateway, "transitGatewayId");
    let subnet = ok(
        case,
        &router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.60.1.0/24")],
    )
    .await;
    let attachment_subnet_id = member(case, &subnet, "subnetId");
    ok(
        case,
        &router,
        "CreateTransitGatewayVpcAttachment",
        &[
            param("TransitGatewayId", &transit_gateway_id),
            param("VpcId", &vpc_id),
            param("SubnetIds.1", &attachment_subnet_id),
        ],
    )
    .await;
    let destination = "192.168.0.0/16";
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &route_table_id),
            param("DestinationCidrBlock", destination),
            param("TransitGatewayId", &transit_gateway_id),
        ],
    )
    .await;

    // Both widths EC2 issues a transit gateway id in, neither of which names a gateway of the
    // account.
    let absent: [(&str, &str); 2] = [
        ("the long id form", "tgw-0123456789abcdef0"),
        ("the short id form", "tgw-0a1b2c3d"),
    ];
    for (row, absent_gateway) in absent {
        assert_refusal(
            &format!("{row} (CreateRoute)"),
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", "172.16.0.0/16"),
                param("TransitGatewayId", absent_gateway),
            ],
            INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            &format!("The transitGateway ID '{absent_gateway}' does not exist."),
        )
        .await;
        assert_refusal(
            &format!("{row} (ReplaceRoute)"),
            &router,
            "ReplaceRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", destination),
                param("TransitGatewayId", absent_gateway),
            ],
            INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            &format!("The transitGateway ID '{absent_gateway}' does not exist."),
        )
        .await;

        // The same id on the operations that act on the GATEWAY itself: those name the
        // resource rather than the member, which is how a caller tells the two validators
        // apart.
        assert_refusal(
            &format!("{row} (DescribeTransitGateways)"),
            &router,
            "DescribeTransitGateways",
            &[param("TransitGatewayIds.1", absent_gateway)],
            INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            &format!("Transit Gateway {absent_gateway} was deleted or does not exist."),
        )
        .await;
    }

    // Nothing a refusal touched was written: the table still holds exactly the one route the
    // create made, pointed at the gateway the account holds.
    let described = ok(
        case,
        &router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", &route_table_id)],
    )
    .await;
    assert!(
        described.contains(&transit_gateway_id),
        "the route the create made must still point at {transit_gateway_id}, got {described}"
    );
    assert!(
        !described.contains("172.16.0.0/16"),
        "a refused create must have written no route, got {described}"
    );
}
