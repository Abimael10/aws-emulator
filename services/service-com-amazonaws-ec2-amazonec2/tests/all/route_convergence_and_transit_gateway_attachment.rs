// SPDX-License-Identifier: CC0-1.0

//! What `CreateRoute` does with a route a caller ASSERTS AGAIN, and with a transit gateway
//! that does not reach the route table's VPC — driven over the wire the service actually
//! speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! Two rules are under test:
//!
//! 1. A ROUTE IS IDENTIFIED BY ITS DESTINATION AND ITS TARGET TOGETHER. Re-submitting the
//!    route a table already holds — the same destination toward the SAME target — asks for
//!    nothing that is not already there, so the call is an idempotent no-op success
//!    (`Return=true`, one route in the table); the same destination toward a DIFFERENT target
//!    would re-point it, which is refused `RouteAlreadyExists` naming the destination (that is
//!    `ReplaceRoute`'s job).
//! 2. A ROUTE TARGET HAS TO BE REACHABLE FROM THE ROUTE TABLE'S OWN VPC. A transit gateway
//!    reaches a VPC only through an ATTACHMENT to it, so a route table of a VPC the gateway has
//!    no attachment in resolves no such gateway at all: the create is refused with the member's
//!    own `InvalidTransitGatewayID.NotFound`, exactly as an id the account holds no gateway for
//!    is — and the same route is created once the attachment exists.
//!
//! The rows span what rule 1 generalises over: which destination member carries the match
//! (IPv4 and IPv6), whether the destination needs canonicalising, how wide the block is, and
//! which KIND of target the route points at. Rule 2's rows differ in the destination the route
//! matches on, the gateway being the fixed subject.

use ec2::state::{INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND, ROUTE_ALREADY_EXISTS};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The emulator the tests drive.
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

/// The estate one case routes in: the VPC, a subnet of it, its route table, an internet
/// gateway attached to it and a network interface in the subnet — two targets of different
/// KINDS, both reachable from the table.
struct Estate {
    vpc_id: String,
    subnet_id: String,
    route_table_id: String,
    internet_gateway_id: String,
    network_interface_id: String,
}

/// One estate, built over the wire. `octet` keeps each case's blocks apart.
async fn estate(case: &str, router: &MultiServiceRouter, octet: u8) -> Estate {
    let vpc = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("10.{octet}.0.0/16"))],
    )
    .await;
    let vpc_id = member(case, &vpc, "vpcId");
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", &format!("10.{octet}.1.0/24")),
        ],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    let table = ok(
        case,
        router,
        "CreateRouteTable",
        &[param("VpcId", &vpc_id)],
    )
    .await;
    let route_table_id = member(case, &table, "routeTableId");
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
    Estate {
        vpc_id,
        subnet_id,
        route_table_id,
        internet_gateway_id,
        network_interface_id,
    }
}

/// How many routes the table `route_table_id` holds for `destination`, read back through
/// `DescribeRouteTables`.
async fn routes_for(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    element_name: &str,
    destination: &str,
) -> usize {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    elements(&body, element_name)
        .into_iter()
        .filter(|held| held == destination)
        .count()
}

/// One row of the idempotence table: the destination the route matches on, the member that
/// carries it, the element a describe reports it in, and the canonical form the service stores
/// it under.
struct Row {
    case: &'static str,
    member: &'static str,
    element: &'static str,
    requested: &'static str,
    canonical: &'static str,
}

/// The destinations one route is asserted for: both families, the widest block of each, a
/// narrower one, and a block whose host bits the service canonicalises away ("we modify the
/// specified CIDR block to its canonical form", `CreateRouteRequest$DestinationCidrBlock`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every IPv4 address",
            member: "DestinationCidrBlock",
            element: "destinationCidrBlock",
            requested: "0.0.0.0/0",
            canonical: "0.0.0.0/0",
        },
        Row {
            case: "a narrower IPv4 block",
            member: "DestinationCidrBlock",
            element: "destinationCidrBlock",
            requested: "10.99.0.0/24",
            canonical: "10.99.0.0/24",
        },
        Row {
            case: "an IPv4 block carrying host bits",
            member: "DestinationCidrBlock",
            element: "destinationCidrBlock",
            requested: "198.51.100.37/26",
            canonical: "198.51.100.0/26",
        },
        Row {
            case: "every IPv6 address",
            member: "DestinationIpv6CidrBlock",
            element: "destinationIpv6CidrBlock",
            requested: "::/0",
            canonical: "::/0",
        },
        Row {
            case: "a narrower IPv6 block",
            member: "DestinationIpv6CidrBlock",
            element: "destinationIpv6CidrBlock",
            requested: "2001:db8::/32",
            canonical: "2001:db8::/32",
        },
    ]
}

/// RULE 1: the identical re-submission is an idempotent success that leaves ONE route, and the
/// same destination toward another target is refused `RouteAlreadyExists`.
#[tokio::test]
async fn re_asserting_a_route_converges_and_re_pointing_one_is_refused() {
    let router = emulator();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let estate = estate(case, &router, 20 + u8::try_from(index).unwrap_or(0)).await;
        let create = |target: (&'static str, String), destination: &'static str| {
            vec![
                param("RouteTableId", &estate.route_table_id),
                param(row.member, destination),
                param(target.0, &target.1),
            ]
        };
        let gateway = ("GatewayId", estate.internet_gateway_id.clone());
        let interface = ("NetworkInterfaceId", estate.network_interface_id.clone());

        // The route itself.
        let created = ok(
            case,
            &router,
            "CreateRoute",
            &create(gateway.clone(), row.requested),
        )
        .await;
        assert_eq!(
            element(&created, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected return=true from the first create, got {created}"
        );

        // The SAME destination toward the SAME target, twice over: each answers the success a
        // converging caller re-asserts its routes for, and the table still holds one route.
        for attempt in 1..=2 {
            let again = ok(
                case,
                &router,
                "CreateRoute",
                &create(gateway.clone(), row.requested),
            )
            .await;
            assert_eq!(
                element(&again, "return").as_deref(),
                Some("true"),
                "case {case:?}: expected return=true from re-submission {attempt}, got {again}"
            );
            let held = routes_for(
                case,
                &router,
                &estate.route_table_id,
                row.element,
                row.canonical,
            )
            .await;
            assert_eq!(
                held, 1,
                "case {case:?}: expected exactly one route for {canonical} after re-submission \
                 {attempt}, got {held}",
                canonical = row.canonical
            );
        }

        // The same destination toward a target of ANOTHER KIND would re-point the route, which
        // is refused — and the route the table holds is left alone.
        let (status, refused) = call(
            &router,
            "CreateRoute",
            &create(interface, row.requested),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, ROUTE_ALREADY_EXISTS),
            "case {case:?}: expected 400 {ROUTE_ALREADY_EXISTS} for another target on the same \
             destination, got {status} with body {refused}"
        );
        assert!(
            error_message(&refused).contains(row.canonical),
            "case {case:?}: expected the refusal to name {canonical}, got {message}",
            canonical = row.canonical,
            message = error_message(&refused)
        );
        let body = ok(
            case,
            &router,
            "DescribeRouteTables",
            &[param("RouteTableId.1", &estate.route_table_id)],
        )
        .await;
        assert!(
            elements(&body, "gatewayId").contains(&estate.internet_gateway_id),
            "case {case:?}: the refused create must leave the route pointing where it did, got \
             {body}"
        );
        assert!(
            !elements(&body, "networkInterfaceId").contains(&estate.network_interface_id),
            "case {case:?}: the refused create must store no route, got {body}"
        );
    }
}

/// RULE 2: a route toward a transit gateway with no attachment in the route table's VPC is
/// refused, and the same route is created once the attachment exists — after which the
/// identical re-submission converges like any other route.
#[tokio::test]
async fn a_route_to_a_transit_gateway_needs_an_attachment_in_the_tables_vpc() {
    // Rows: two destinations, so the rule is not about one block.
    for (index, destination) in ["0.0.0.0/0", "172.16.0.0/16"].into_iter().enumerate() {
        let case = destination;
        let router = emulator();
        let estate = estate(case, &router, 40 + u8::try_from(index).unwrap_or(0)).await;
        let gateway = ok(case, &router, "CreateTransitGateway", &[]).await;
        let transit_gateway_id = member(case, &gateway, "transitGatewayId");
        let request = vec![
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", destination),
            param("TransitGatewayId", &transit_gateway_id),
        ];

        // No attachment: the gateway reaches none of this VPC's traffic, so the member
        // resolves no transit gateway of this route table's network at all.
        let (status, refused) = call(&router, "CreateRoute", &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND),
            "case {case:?}: expected 400 {INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND} for an unattached \
             transit gateway, got {status} with body {refused}"
        );
        let message = error_message(&refused);
        assert!(
            message.contains(&transit_gateway_id),
            "case {case:?}: expected the refusal to name the gateway, got {message}"
        );
        assert_eq!(
            routes_for(
                case,
                &router,
                &estate.route_table_id,
                "destinationCidrBlock",
                destination
            )
            .await,
            0,
            "case {case:?}: a refused create must store no route"
        );

        // Attached to THIS VPC, the same request is the route it asks for.
        ok(
            case,
            &router,
            "CreateTransitGatewayVpcAttachment",
            &[
                param("TransitGatewayId", &transit_gateway_id),
                param("VpcId", &estate.vpc_id),
                param("SubnetIds.1", &estate.subnet_id),
            ],
        )
        .await;
        let created = ok(case, &router, "CreateRoute", &request).await;
        assert_eq!(
            element(&created, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected return=true once the gateway is attached, got {created}"
        );
        let again = ok(case, &router, "CreateRoute", &request).await;
        assert_eq!(
            element(&again, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected the identical re-submission to converge, got {again}"
        );
        assert_eq!(
            routes_for(
                case,
                &router,
                &estate.route_table_id,
                "destinationCidrBlock",
                destination
            )
            .await,
            1,
            "case {case:?}: expected exactly one route toward the attached gateway"
        );
    }
}
