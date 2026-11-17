// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ReplaceRoute`, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=ReplaceRoute`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a route is addressed inside its table by the ONE destination it
//! matches on, and a replacement points THAT route at the target the request names** —
//! "replaces an existing route within a route table in a VPC. You must specify either a
//! destination CIDR block or a prefix list ID. You must also specify exactly one of the
//! resources from the parameter list". So the route that was there keeps its destination, its
//! origin and its place in the table, carries the new target and carries no member of the
//! target it had, and the table still holds exactly one route for that destination.
//!
//! Its two halves follow from the same reading. The route's STATE is its target's
//! availability — "the `blackhole` state indicates that the route's target isn't available
//! (for example, the specified gateway isn't attached to the VPC, or the specified NAT instance
//! has been terminated)" (`Route$State`) — so a route pointed at an elastic network interface
//! attached to nothing is reported `blackhole` while one pointed at an attached gateway is
//! `active`. And a request that names no route, or a target this account does not hold, is
//! refused rather than written: the refusal ladder below is the same one the create of a route
//! is held to, with `InvalidRoute.NotFound` for a destination the table holds no route for.
//!
//! The rows differ along what the rule generalises over: which KIND of resource the new target
//! is (an internet gateway, an elastic network interface, a NAT gateway), which member of the
//! stored route carries it, and whether that target's availability makes the route active or a
//! blackhole.
//!
//! The replacement side of the route slice. [`super::vpc_internet_path`] drives the creates,
//! [`super::delete_route`] the deletes, and [`super::create_route_table`] the table they live
//! in.

use ec2::state::{
    DRY_RUN_OPERATION, INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
    INVALID_ROUTE_TABLE_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// Amazon EC2's code for a route the named table does not hold ("InvalidRoute.NotFound — the
/// specified route does not exist", EC2 API "Error codes").
const INVALID_ROUTE_NOT_FOUND: &str = "InvalidRoute.NotFound";

/// The block the VPC every row's table belongs to is created with, the subnet-sized block the
/// interface and the NAT gateway of the estate live in, and the destination every row's route
/// matches on.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDR: &str = "10.90.1.0/24";
const DESTINATION: &str = "0.0.0.0/0";

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

/// The `DescribeRouteTables` body for the one table `route_table_id` names.
async fn describe(case: &str, router: &MultiServiceRouter, route_table_id: &str) -> String {
    ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await
}

/// The estate every row's route points into: the VPC, a subnet of it, the internet gateway
/// attached to it, an elastic network interface in that subnet (attached to nothing, which is
/// what makes a route through it a blackhole) and a private NAT gateway of that subnet.
struct Estate {
    vpc_id: String,
    internet_gateway_id: String,
    network_interface_id: String,
    nat_gateway_id: String,
}

/// Create that estate.
async fn estate(router: &MultiServiceRouter) -> Estate {
    let case = "the estate the routes point into";
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    let gateway = ok(case, router, "CreateInternetGateway", &[]).await;
    let internet_gateway_id = member(case, &gateway, "internetGatewayId");
    ok(
        case,
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let interface = ok(
        case,
        router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnet_id)],
    )
    .await;
    let network_interface_id = member(case, &interface, "networkInterfaceId");
    // A PRIVATE NAT gateway, the one form whose create states no Elastic IP address: "you
    // cannot specify an Elastic IP address with a private NAT gateway"
    // (`CreateNatGatewayRequest$AllocationId`).
    let nat = ok(
        case,
        router,
        "CreateNatGateway",
        &[
            param("SubnetId", &subnet_id),
            param("ConnectivityType", "private"),
        ],
    )
    .await;
    let nat_gateway_id = member(case, &nat, "natGatewayId");
    Estate {
        vpc_id,
        internet_gateway_id,
        network_interface_id,
        nat_gateway_id,
    }
}

/// A route table of `vpc_id`, as its id.
async fn route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    member(case, &body, "routeTableId")
}

/// One row: the member `ReplaceRoute` names the new target under, the element
/// `DescribeRouteTables` reports that target in, and the state the route is then reported in.
struct Row {
    case: &'static str,
    member: &'static str,
    reported: &'static str,
    state: &'static str,
}

/// Source: `ReplaceRoute`'s own target members
/// (`smithy_query(query="operation",operation="ReplaceRoute")`) against the `Route` members
/// that report each kind of target (`Route$GatewayId`, `$NetworkInterfaceId`,
/// `$NatGatewayId`), and `Route$State` for the two values a route is reported in.
const ROWS: &[Row] = &[
    Row {
        case: "an internet gateway attached to the table's own VPC",
        member: "GatewayId",
        reported: "gatewayId",
        state: "active",
    },
    Row {
        case: "an elastic network interface attached to nothing",
        member: "NetworkInterfaceId",
        reported: "networkInterfaceId",
        state: "blackhole",
    },
    Row {
        case: "a NAT gateway of the VPC's own subnet",
        member: "NatGatewayId",
        reported: "natGatewayId",
        state: "active",
    },
];

/// The target of one row, out of the estate.
fn target(row: &Row, estate: &Estate) -> String {
    match row.member {
        "GatewayId" => estate.internet_gateway_id.clone(),
        "NetworkInterfaceId" => estate.network_interface_id.clone(),
        _ => estate.nat_gateway_id.clone(),
    }
}

/// The route of a described table that matches on [`DESTINATION`], as the `<item>` block
/// `DescribeRouteTables` reports it in.
///
/// Read out of the `<routeSet>` the answer nests a table's routes in, so an `<item>` of the
/// table's association or tag sets cannot be mistaken for a route.
fn route_items(described: &str) -> Vec<String> {
    let set = described
        .find("<routeSet>")
        .map_or("", |start| &described[start..]);
    let set = &set[..set.find("</routeSet>").unwrap_or(set.len())];
    set.split("<item>")
        .skip(1)
        .map(|item| {
            item.split("</item>")
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// That one route, or a panic naming the case.
fn stored_route(case: &str, described: &str) -> String {
    route_items(described)
        .into_iter()
        .find(|item| element(item, "destinationCidrBlock").as_deref() == Some(DESTINATION))
        .unwrap_or_else(|| panic!("case {case:?}: no route for {DESTINATION} in {described}"))
}

/// The destinations a described table's routes match on, in the order it reports them: what
/// says a replaced route kept its PLACE in the table.
fn stored_destinations(described: &str) -> Vec<String> {
    route_items(described)
        .iter()
        .filter_map(|item| element(item, "destinationCidrBlock"))
        .collect()
}

/// A replacement points the route its destination names at the new target, and the route keeps
/// everything else that makes it the route it is.
#[tokio::test]
async fn replace_route_points_the_route_at_the_target_it_names() {
    let router = emulator();
    let estate = estate(&router).await;

    for row in ROWS {
        let case = row.case;
        // TWO tables carrying the same route: what the replacement must leave alone is the
        // other table's, since a route is addressed by its table as well as its destination.
        let replaced_in = route_table(case, &router, &estate.vpc_id).await;
        let untouched = route_table(case, &router, &estate.vpc_id).await;
        for table in [&replaced_in, &untouched] {
            ok(
                case,
                &router,
                "CreateRoute",
                &[
                    param("RouteTableId", table),
                    param("DestinationCidrBlock", DESTINATION),
                    param("GatewayId", &estate.internet_gateway_id),
                ],
            )
            .await;
        }

        // A dry run of a replacement that WOULD have succeeded changes nothing.
        let (status, dry) = call(
            &router,
            "ReplaceRoute",
            &[
                param("RouteTableId", &replaced_in),
                param("DestinationCidrBlock", DESTINATION),
                param(row.member, &target(row, &estate)),
                param("DryRun", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 412,
            "case {case:?}: expected a dry run to be refused, got {status} with body {dry}"
        );
        assert_eq!(
            error_code(&dry),
            DRY_RUN_OPERATION,
            "case {case:?}: expected {DRY_RUN_OPERATION}, got {dry}"
        );
        let read = describe(case, &router, &replaced_in).await;
        assert_eq!(
            element(&stored_route(case, &read), "gatewayId").as_deref(),
            Some(estate.internet_gateway_id.as_str()),
            "case {case:?}: expected the dry run to leave the route pointing where it did, got \
             {read}"
        );

        let body = ok(
            case,
            &router,
            "ReplaceRoute",
            &[
                param("RouteTableId", &replaced_in),
                // The destination the create stored the route under, which is what addresses
                // the route this replacement re-points.
                param("DestinationCidrBlock", DESTINATION),
                param(row.member, &target(row, &estate)),
            ],
        )
        .await;
        assert!(
            !body.is_empty(),
            "case {case:?}: expected ReplaceRoute to answer its own envelope, got {body}"
        );

        // The table holds ONE route for that destination, pointing at the new target, with no
        // member of the target it had, in the state that target's availability makes it — and
        // still as a route a caller added, in the place it was in.
        let read = describe(case, &router, &replaced_in).await;
        assert_eq!(
            stored_destinations(&read)
                .iter()
                .filter(|block| *block == DESTINATION)
                .count(),
            1,
            "case {case:?}: expected the table to hold one route for {DESTINATION}, got {read}"
        );
        let route = stored_route(case, &read);
        assert_eq!(
            element(&route, row.reported).as_deref(),
            Some(target(row, &estate).as_str()),
            "case {case:?}: expected the route to point at its new target, got {route}"
        );
        if row.member != "GatewayId" {
            assert_eq!(
                element(&route, "gatewayId"),
                None,
                "case {case:?}: expected the route to carry no member of the target it had, \
                 got {route}"
            );
        }
        assert_eq!(
            element(&route, "state").as_deref(),
            Some(row.state),
            "case {case:?}: expected the route to be reported {state}, got {route}",
            state = row.state
        );
        assert_eq!(
            element(&route, "origin").as_deref(),
            Some("CreateRoute"),
            "case {case:?}: expected the replaced route to still be one a caller added, got \
             {route}"
        );
        // Its place in the table is where it was: the local route the table came with first,
        // then the one the create added.
        assert_eq!(
            stored_destinations(&read),
            vec![VPC_CIDR.to_string(), DESTINATION.to_string()],
            "case {case:?}: expected the replaced route to keep its place in the table, got \
             {read}"
        );
        // ...and the other table's route is exactly where it was: a replacement addresses ONE
        // table.
        let other = describe(case, &router, &untouched).await;
        assert_eq!(
            element(&stored_route(case, &other), "gatewayId").as_deref(),
            Some(estate.internet_gateway_id.as_str()),
            "case {case:?}: expected the other table's route to be left alone, got {other}"
        );
    }
}

/// A destination written with host bits set addresses the route the create stored under the
/// canonical form of that block.
#[tokio::test]
async fn replace_route_canonicalises_the_destination_it_addresses() {
    let case = "a destination written with host bits set";
    let router = emulator();
    let estate = estate(&router).await;
    let table = route_table(case, &router, &estate.vpc_id).await;
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &table),
            param("DestinationCidrBlock", "198.51.100.77/26"),
            param("GatewayId", &estate.internet_gateway_id),
        ],
    )
    .await;
    ok(
        case,
        &router,
        "ReplaceRoute",
        &[
            param("RouteTableId", &table),
            // The same block spelled with other host bits: the canonical form of both is the
            // one the create stored.
            param("DestinationCidrBlock", "198.51.100.99/26"),
            param("NetworkInterfaceId", &estate.network_interface_id),
        ],
    )
    .await;
    let read = describe(case, &router, &table).await;
    assert_eq!(
        stored_destinations(&read),
        vec![VPC_CIDR.to_string(), "198.51.100.64/26".to_string()],
        "case {case:?}: expected the stored destination to be left canonical, got {read}"
    );
    assert_eq!(
        elements(&read, "networkInterfaceId"),
        vec![estate.network_interface_id.clone()],
        "case {case:?}: expected the canonical destination's route to be the one replaced, got \
         {read}"
    );
}

/// What a replacement refuses, and that it wrote nothing on the way.
#[tokio::test]
async fn replace_route_refuses_the_requests_that_name_no_route() {
    let case = "the refusal ladder";
    let router = emulator();
    let estate = estate(&router).await;
    let table = route_table(case, &router, &estate.vpc_id).await;
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &table),
            param("DestinationCidrBlock", DESTINATION),
            param("GatewayId", &estate.internet_gateway_id),
        ],
    )
    .await;

    // (case, the parameters, the HTTP status, the code the refusal carries)
    let cases: Vec<(&str, Vec<Param>, u16, &str)> = vec![
        (
            "a route table the account does not hold",
            vec![
                param("RouteTableId", "rtb-0123456789abcdef0"),
                param("DestinationCidrBlock", DESTINATION),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            INVALID_ROUTE_TABLE_ID_NOT_FOUND,
        ),
        (
            "no destination at all, which names no route",
            vec![
                param("RouteTableId", &table),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            "MissingParameter",
        ),
        (
            "two destinations, which name no one route",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
                param("DestinationIpv6CidrBlock", "::/0"),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            "InvalidParameterCombination",
        ),
        (
            "a destination that is not a CIDR block",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", "not-a-block"),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            "InvalidParameterValue",
        ),
        (
            "no target at all",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
            ],
            400,
            "MissingParameter",
        ),
        (
            "two targets, which name no one target",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
                param("GatewayId", &estate.internet_gateway_id),
                param("NatGatewayId", &estate.nat_gateway_id),
            ],
            400,
            "InvalidParameterCombination",
        ),
        (
            "the local route of the table's own VPC",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", VPC_CIDR),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            "InvalidParameterValue",
        ),
        (
            "a destination the table holds no route for",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", "192.0.2.0/24"),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            400,
            INVALID_ROUTE_NOT_FOUND,
        ),
        (
            "a network interface the account does not hold",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
                param("NetworkInterfaceId", "eni-0123456789abcdef0"),
            ],
            400,
            INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
        ),
        (
            // Work this emulator holds no state for at all, which is a 501 rather than a
            // refusal that blames the caller.
            "a reset of the local route to its default target",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
                param("LocalTarget", "true"),
            ],
            501,
            "NotImplemented",
        ),
        (
            "a Cloud WAN core network this emulator holds none of",
            vec![
                param("RouteTableId", &table),
                param("DestinationCidrBlock", DESTINATION),
                param(
                    "CoreNetworkArn",
                    "arn:aws:networkmanager::000000000000:core-network/core-network-0123",
                ),
            ],
            501,
            "NotImplemented",
        ),
    ];

    for (refused, params, status, expected) in cases {
        let (answered, body) = call(&router, "ReplaceRoute", &params).await;
        assert_eq!(
            answered, status,
            "case {refused:?}: expected HTTP {status}, got {answered} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {refused:?}: expected {expected}, got {body}"
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {refused:?}: expected the refusal to carry a message, got {body}"
        );
    }

    // ...and a DRY RUN of a request that would NOT have succeeded is refused with what the
    // request earned rather than with the dry run's own verdict: `DryRunOperation` says "the
    // request would have succeeded", which a destination the table holds no route for would
    // not have.
    let (status, body) = call(
        &router,
        "ReplaceRoute",
        &[
            param("RouteTableId", &table),
            param("DestinationCidrBlock", "192.0.2.0/24"),
            param("GatewayId", &estate.internet_gateway_id),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the dry run of a request naming no route to be refused as \
         that request, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_ROUTE_NOT_FOUND,
        "case {case:?}: expected {INVALID_ROUTE_NOT_FOUND}, got {body}"
    );

    // Nothing any of the refusals asked for was written: the route still points where the
    // create put it, and the table still holds its own two routes.
    let read = describe(case, &router, &table).await;
    assert_eq!(
        stored_destinations(&read),
        vec![VPC_CIDR.to_string(), DESTINATION.to_string()],
        "case {case:?}: expected the table to keep its two routes, got {read}"
    );
    assert_eq!(
        element(&stored_route(case, &read), "gatewayId").as_deref(),
        Some(estate.internet_gateway_id.as_str()),
        "case {case:?}: expected the route to point where the create put it, got {read}"
    );
}

/// A route CREATED at a target that is not available is stored a blackhole, which is the other
/// side of the state rule the replacements above are held to.
#[tokio::test]
async fn a_route_created_at_an_unattached_interface_is_a_blackhole() {
    let case = "a route created at an interface attached to nothing";
    let router = emulator();
    let estate = estate(&router).await;
    let table = route_table(case, &router, &estate.vpc_id).await;
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &table),
            param("DestinationCidrBlock", DESTINATION),
            param("NetworkInterfaceId", &estate.network_interface_id),
        ],
    )
    .await;
    let read = describe(case, &router, &table).await;
    let route = stored_route(case, &read);
    assert_eq!(
        element(&route, "state").as_deref(),
        Some("blackhole"),
        "case {case:?}: expected a route whose target is not available to be reported a \
         blackhole, got {route}"
    );
    // ...while the local route the table came with, whose target is the VPC itself, is active.
    assert!(
        elements(&read, "state").contains(&"active".to_string()),
        "case {case:?}: expected the table's local route to stay active, got {read}"
    );
}
