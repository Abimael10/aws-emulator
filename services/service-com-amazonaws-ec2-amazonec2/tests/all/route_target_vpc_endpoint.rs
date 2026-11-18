// SPDX-License-Identifier: CC0-1.0

//! A VPC ENDPOINT named as a route's target, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a route's target is a RESOURCE, so an id the account holds no
//! resource for is refused with THAT resource's own `Invalid<Kind>.NotFound` code and
//! sentence** — for `VpcEndpointId` ("the ID of a VPC endpoint. Supported for Gateway Load
//! Balancer endpoints only"), `InvalidVpcEndpointId.NotFound` and "The vpcEndpoint ID '<id>'
//! does not exist", the way the route operations name an unknown target (beside the gateway's
//! "The gateway ID '<id>' does not exist" and the table's "The routeTable ID '<id>' does not
//! exist"). It is a rule about the RESOURCE, not about the call, so every operation that points
//! a route at one answers it identically: `CreateRoute`, which stores a route, and
//! `ReplaceRoute`, which re-points one.
//!
//! The rows differ along what the rule generalises over: which operation named the endpoint,
//! both published id lengths, and the well-formed ids inside the long form that resolve to no
//! endpoint. The endpoint the account DOES hold is a row too — the same resolution that refuses
//! the others accepts it, reported in the route's `GatewayId`, the member the modelled `Route`
//! carries a VPC endpoint under.
//!
//! The endpoint-target slice of the route operations. [`super::replace_route`] drives the
//! replacement itself, [`super::vpc_internet_path`] the create, and
//! [`super::per_resource_refusal_wording`] the operations that resolve a `VpcEndpointId` as the
//! SUBJECT of the call, which name the resource in AWS's other wording for it.

use ec2::state::INVALID_VPC_ENDPOINT_ID_NOT_FOUND;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The block the VPC the routes belong to is created with.
const VPC_CIDR: &str = "10.94.0.0/16";

/// The gateway endpoint service the held endpoint is created for — one the Region publishes.
const ENDPOINT_SERVICE: &str = "com.amazonaws.us-east-1.s3";

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

/// The estate the targets are named to: the VPC, the internet gateway attached to it (the
/// target the route under test starts out pointing at), a route table of that VPC and a GATEWAY
/// endpoint of it — the one endpoint the account really holds.
struct Estate {
    internet_gateway_id: String,
    vpc_endpoint_id: String,
    route_table_id: String,
}

/// Create that estate, with one route of the table pointing at the internet gateway.
async fn estate(router: &MultiServiceRouter) -> Estate {
    let case = "the estate the endpoint targets are named to";
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
    let vpc_id = member(case, &vpc, "vpcId");
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
    // A gateway endpoint of this VPC named by no route table of its own, so it is a target
    // these rows point a route at rather than one that wrote routes itself.
    let endpoint = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", ENDPOINT_SERVICE),
            param("VpcEndpointType", "Gateway"),
        ],
    )
    .await;
    let vpc_endpoint_id = member(case, &endpoint, "vpcEndpointId");
    let table = ok(case, router, "CreateRouteTable", &[param("VpcId", &vpc_id)]).await;
    let route_table_id = member(case, &table, "routeTableId");
    ok(
        case,
        router,
        "CreateRoute",
        &[
            param("RouteTableId", &route_table_id),
            param("DestinationCidrBlock", "0.0.0.0/0"),
            param("GatewayId", &internet_gateway_id),
        ],
    )
    .await;
    Estate {
        internet_gateway_id,
        vpc_endpoint_id,
        route_table_id,
    }
}

/// Both published `VpcEndpointId` lengths — 17 and 8 lowercase hexadecimal digits — and, within
/// the long form, the all-zero id a client reaches for as a placeholder, a mixed-digit one and
/// an all-`f` one: WELL FORMED ids, so each is this not-found refusal rather than a
/// malformed-id one.
const UNHELD_ENDPOINTS: &[&str] = &[
    "vpce-00000000000000000",
    "vpce-0a1b2c3d4e5f60718",
    "vpce-fffffffffffffffff",
    "vpce-0a1b2c3d",
];

/// An endpoint id the account holds no endpoint for is refused as that ENDPOINT, whichever
/// operation pointed a route at it, and writes nothing.
#[tokio::test]
async fn an_unheld_vpc_endpoint_target_is_refused_as_that_endpoint() {
    let router = emulator();
    let estate = estate(&router).await;

    // Both operations that point a route at a target, each with the destination it addresses:
    // the replacement re-points the route the table holds, the create would add one it does
    // not.
    let callers = [
        ("ReplaceRoute", "0.0.0.0/0"),
        ("CreateRoute", "198.51.100.0/24"),
    ];
    for (action, destination) in callers {
        for endpoint_id in UNHELD_ENDPOINTS {
            let case = format!("{action} naming the unheld endpoint {endpoint_id}");
            let (status, body) = call(
                &router,
                action,
                &[
                    param("RouteTableId", &estate.route_table_id),
                    param("DestinationCidrBlock", destination),
                    param("VpcEndpointId", endpoint_id),
                ],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the target to be refused, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_VPC_ENDPOINT_ID_NOT_FOUND,
                "case {case:?}: expected {INVALID_VPC_ENDPOINT_ID_NOT_FOUND}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                format!("The vpcEndpoint ID '{endpoint_id}' does not exist"),
                "case {case:?}: expected the endpoint's own sentence, got {body}"
            );
        }
    }

    // None of them wrote anything: the table holds the local route it came with and the one
    // route the create put there, still pointing at the internet gateway.
    let case = "the table the refusals were named to";
    let read = describe(case, &router, &estate.route_table_id).await;
    assert_eq!(
        elements(&read, "destinationCidrBlock"),
        vec![VPC_CIDR.to_string(), "0.0.0.0/0".to_string()],
        "case {case:?}: expected the refusals to add and rewrite nothing, got {read}"
    );
    assert!(
        elements(&read, "gatewayId").contains(&estate.internet_gateway_id),
        "case {case:?}: expected the route to keep the target the create gave it, got {read}"
    );
}

/// The other side of the same resolution: the endpoint the account DOES hold is accepted by both
/// operations and reported in the route's `GatewayId`, the member the modelled `Route` carries a
/// VPC endpoint under.
#[tokio::test]
async fn a_held_vpc_endpoint_target_is_reported_as_the_routes_gateway() {
    let router = emulator();
    let estate = estate(&router).await;

    // The route the table holds, re-pointed at the endpoint.
    let case = "a replacement at the endpoint the account holds";
    ok(
        case,
        &router,
        "ReplaceRoute",
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", "0.0.0.0/0"),
            param("VpcEndpointId", &estate.vpc_endpoint_id),
        ],
    )
    .await;
    let read = describe(case, &router, &estate.route_table_id).await;
    assert!(
        elements(&read, "gatewayId").contains(&estate.vpc_endpoint_id),
        "case {case:?}: expected the route to point at the endpoint, got {read}"
    );
    assert!(
        !elements(&read, "gatewayId").contains(&estate.internet_gateway_id),
        "case {case:?}: expected the gateway it pointed at to be gone, got {read}"
    );

    // And a route CREATED at the same endpoint, on another destination, is stored the same way.
    let case = "a create at the endpoint the account holds";
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", "198.51.100.0/24"),
            param("VpcEndpointId", &estate.vpc_endpoint_id),
        ],
    )
    .await;
    let read = describe(case, &router, &estate.route_table_id).await;
    assert_eq!(
        elements(&read, "gatewayId")
            .iter()
            .filter(|reported| **reported == estate.vpc_endpoint_id)
            .count(),
        2,
        "case {case:?}: expected both routes to report the endpoint as their gateway, got {read}"
    );
}
