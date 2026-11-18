// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for HOW a route's TARGET is resolved: which resource kind each target
//! member of `CreateRoute` and `ReplaceRoute` names, and how a target that is not reachable
//! from the route table's VPC is named back to the caller.
//!
//! The rule under test: **each target member resolves against the ONE resource kind it
//! documents, and against no other.** `GatewayId` is "the ID of an internet gateway or virtual
//! private gateway attached to your VPC" and `VpcEndpointId` "the ID of a VPC endpoint.
//! Supported for Gateway Load Balancer endpoints only"
//! (`smithy_query(query="operation",operation="CreateRoute")` on this crate's model), so an id
//! that names a resource of another kind names nothing the member could have meant: it is
//! refused with that member's own not-found rather than resolved through a neighbouring
//! member's store. The SAME VPC endpoint id is therefore a route target under `VpcEndpointId`
//! — HTTP 200, `return` true, and the stored route reports it — and no gateway at all under
//! `GatewayId`.
//!
//! The second half of the rule: a target the route table's VPC cannot reach is refused
//! `InvalidParameterValue` naming the table and the target AS THE SERVICE NAMES IT — a gateway
//! by the gateway noun and its own id, and a VPC endpoint by the endpoint network interface it
//! holds in its subnet, under that interface's id with `interface-` for its prefix.
//!
//! Every table row is a distinct input along what the rule generalises over: which member the
//! request names, which resource KIND the value names (a VPC endpoint of this VPC, one of
//! another VPC, a network interface, a transit gateway, an id nothing minted), the destination
//! family the route matches on, and which of the two operations that resolve a target is
//! driven.

use super::ec2_query::{element, error_code, error_message};
use super::wire::{child, document, param, set_items, Param};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An endpoint service of the PARTNER family (`com.amazonaws.vpce.<region>.<service-id>`), the
/// family a Gateway Load Balancer endpoint of another account's service is created to.
const GATEWAY_LOAD_BALANCER_SERVICE: &str = "com.amazonaws.vpce.us-west-2.vpce-svc-0123456789abcdef0";

/// EC2's answer for a value that names no gateway of the account
/// (`crate::state::INVALID_GATEWAY_ID_NOT_FOUND`), which is what the `GatewayId` member
/// refuses an id of another kind with.
const GATEWAY_NOT_FOUND: &str = "InvalidGatewayID.NotFound";

/// EC2's answer for a parameter whose value the service holds against another of the
/// request's: the code a cross-network target is refused with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The destination of a route that steers every IPv4 address, the commonest one of all.
const EVERY_DESTINATION: &str = "0.0.0.0/0";

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

/// One VPC of the account, with one subnet and one route table, built through EC2's own
/// operations; every id is the one EC2 MINTED.
struct Network {
    vpc_id: String,
    subnet_id: String,
    route_table_id: String,
}

/// Create [`Network`] with the address range `10.<octet>.0.0/16`.
async fn network(case: &str, router: &MultiServiceRouter, octet: u8) -> Network {
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
    let table = ok(
        case,
        router,
        "CreateRouteTable",
        &[param("VpcId", &vpc_id)],
    )
    .await;
    Network {
        subnet_id: member(case, &subnet, "subnetId"),
        route_table_id: member(case, &table, "routeTableId"),
        vpc_id,
    }
}

/// A Gateway Load Balancer VPC endpoint in `network`'s subnet, as `(endpoint id, the id of the
/// endpoint network interface it holds in that subnet)`.
///
/// The interface is the endpoint's own: "an endpoint network interface is created … in an
/// Availability Zone" (`CreateVpcEndpointRequest$SubnetIds`), and it is what the cross-network
/// refusal names.
async fn load_balancer_endpoint(
    case: &str,
    router: &MultiServiceRouter,
    network: &Network,
) -> (String, String) {
    let body = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &network.vpc_id),
            param("ServiceName", GATEWAY_LOAD_BALANCER_SERVICE),
            param("VpcEndpointType", "GatewayLoadBalancer"),
            param("SubnetId.1", &network.subnet_id),
        ],
    )
    .await;
    let endpoint_id = member(case, &body, "vpcEndpointId");
    let parsed = document(case, &body);
    let endpoint = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "vpcEndpointId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no endpoint in {body}"));
    let interface_id = set_items(endpoint, "networkInterfaceIdSet")
        .first()
        .and_then(|item| item.text())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the endpoint reported no network interface: {body}")
        })
        .to_string();
    (endpoint_id, interface_id)
}

/// An internet gateway attached to `vpc_id`, as its id.
async fn attached_internet_gateway(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
) -> String {
    let created = ok(case, router, "CreateInternetGateway", &[]).await;
    let gateway_id = member(case, &created, "internetGatewayId");
    ok(
        case,
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &gateway_id),
            param("VpcId", vpc_id),
        ],
    )
    .await;
    gateway_id
}

/// The target `DescribeRouteTables` reports for the route to `destination`, or `None` when the
/// table routes that destination nowhere.
///
/// An endpoint target is reported in `Route$GatewayId`, the modelled `Route` carrying no member
/// of its own for a VPC endpoint.
async fn route_target(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    destination: &str,
) -> Option<String> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .find(|item| {
            super::wire::child_text(*item, "destinationCidrBlock").as_deref() == Some(destination)
                || super::wire::child_text(*item, "destinationIpv6CidrBlock").as_deref()
                    == Some(destination)
        })
        .and_then(|item| super::wire::child_text(item, "gatewayId"))
}

/// RULE, first half: `GatewayId` names a gateway and nothing else, so an id of any other kind
/// is refused `InvalidGatewayID.NotFound` naming that id — while the same VPC endpoint id under
/// `VpcEndpointId` is accepted and is the target the stored route reports.
#[tokio::test]
async fn gateway_id_resolves_a_gateway_and_no_other_resource_kind() {
    let case = "gateway-id-resolution";
    let router = emulator();
    let here = network(case, &router, 30).await;
    let elsewhere = network(case, &router, 31).await;
    let (endpoint_id, interface_id) = load_balancer_endpoint(case, &router, &here).await;
    let (foreign_endpoint_id, _) = load_balancer_endpoint(case, &router, &elsewhere).await;
    let gateway_id = attached_internet_gateway(case, &router, &here.vpc_id).await;
    let transit_gateway = ok(case, &router, "CreateTransitGateway", &[]).await;
    let transit_gateway_id = member(case, &transit_gateway, "transitGatewayId");

    // One row per kind of id a caller can put in `GatewayId` that is not a gateway: the VPC
    // endpoint of this very VPC (the id a client reads off a firewall's attachment), one of
    // another VPC, the endpoint's own network interface, a transit gateway, and a `vpce-` id
    // nothing minted. Each is refused the same way, the member naming no such gateway.
    //
    // (the row, the value `GatewayId` carries)
    let rows: Vec<(String, String)> = vec![
        (
            "a VPC endpoint of the route table's own VPC".to_string(),
            endpoint_id.clone(),
        ),
        (
            "a VPC endpoint of another VPC".to_string(),
            foreign_endpoint_id,
        ),
        (
            "the endpoint's own network interface".to_string(),
            interface_id,
        ),
        ("a transit gateway".to_string(), transit_gateway_id),
        (
            "a vpce- id nothing minted".to_string(),
            "vpce-00000000000000000".to_string(),
        ),
    ];
    for (index, (row, value)) in rows.iter().enumerate() {
        // A destination of its own per row, so no row can be refused for colliding with
        // another row's route rather than for its target.
        let destination = format!("192.0.{index}.0/24");
        let (status, body) = call(
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &here.route_table_id),
                param("DestinationCidrBlock", &destination),
                param("GatewayId", value),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, GATEWAY_NOT_FOUND),
            "case {row:?}: expected 400 {GATEWAY_NOT_FOUND} for a GatewayId naming {value}, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The gateway ID '{value}' does not exist"),
            "case {row:?}: expected the refusal to name the gateway id the request carried"
        );
        assert_eq!(
            route_target(row, &router, &here.route_table_id, &destination).await,
            None,
            "case {row:?}: a refused CreateRoute must store no route"
        );
    }

    // The positive control for the member: an internet gateway attached to the table's own VPC
    // IS what `GatewayId` names, so that route is created.
    let created = ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &here.route_table_id),
            param("DestinationCidrBlock", "198.51.100.0/24"),
            param("GatewayId", &gateway_id),
        ],
    )
    .await;
    assert_eq!(
        element(&created, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected return=true for a gateway target, got {created}"
    );
    assert_eq!(
        route_target(case, &router, &here.route_table_id, "198.51.100.0/24").await,
        Some(gateway_id),
        "case {case:?}: expected the stored route to report the gateway it points at"
    );

    // And the other half of the rule, on the very id every row above was refused for: under
    // `VpcEndpointId` that endpoint IS a route target, whichever destination family the route
    // matches on, and the stored route reports it.
    for (row, member_name, destination) in [
        (
            "an endpoint target matching every IPv4 address",
            "DestinationCidrBlock",
            EVERY_DESTINATION,
        ),
        (
            "an endpoint target matching a narrower IPv4 block",
            "DestinationCidrBlock",
            "10.30.9.0/24",
        ),
        (
            "an endpoint target matching an IPv6 block",
            "DestinationIpv6CidrBlock",
            "2001:db8::/32",
        ),
    ] {
        let created = ok(
            row,
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &here.route_table_id),
                param(member_name, destination),
                param("VpcEndpointId", &endpoint_id),
            ],
        )
        .await;
        assert_eq!(
            element(&created, "return").as_deref(),
            Some("true"),
            "case {row:?}: expected return=true for the endpoint {endpoint_id}, got {created}"
        );
        assert_eq!(
            route_target(row, &router, &here.route_table_id, destination).await,
            Some(endpoint_id.clone()),
            "case {row:?}: expected the stored route to report the endpoint it points at"
        );
    }
}

/// RULE, second half: a target the route table's VPC cannot reach is refused
/// `InvalidParameterValue` naming the table and the target as the service names it — a VPC
/// endpoint by its endpoint network interface, a gateway by the gateway noun and its id.
///
/// The rows span the target KINDS the refusal words differently, and both operations that
/// resolve a target through the same path.
#[tokio::test]
async fn a_target_in_another_vpc_is_refused_naming_what_the_target_is() {
    /// Which operation the row drives, the two that resolve a route's target.
    enum Through {
        /// `CreateRoute`, which resolves the target of a new route.
        Create,
        /// `ReplaceRoute`, which resolves the target a route already in the table is pointed at.
        Replace,
    }
    let case = "cross-network-target";
    let router = emulator();
    let here = network(case, &router, 40).await;
    let elsewhere = network(case, &router, 41).await;
    let (endpoint_id, interface_id) = load_balancer_endpoint(case, &router, &elsewhere).await;
    let gateway_id = attached_internet_gateway(case, &router, &elsewhere.vpc_id).await;
    let egress_only = ok(
        case,
        &router,
        "CreateEgressOnlyInternetGateway",
        &[param("VpcId", &elsewhere.vpc_id)],
    )
    .await;
    let egress_only_id = member(case, &egress_only, "egressOnlyInternetGatewayId");

    // An EC2 resource id is `<prefix>-<digits>`, and the interface is named by those digits
    // under the interface prefix.
    let interface_digits = interface_id
        .split_once('-')
        .map(|(_, digits)| digits.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: {interface_id} is not an EC2 resource id"));

    // (the row, the member, the value, the destination member, the destination, how the refusal
    //  names the target, which operation resolves it)
    let rows: Vec<(&str, &str, String, &str, &str, String, Through)> = vec![
        (
            "a VPC endpoint of another VPC, through CreateRoute",
            "VpcEndpointId",
            endpoint_id.clone(),
            "DestinationCidrBlock",
            EVERY_DESTINATION,
            format!("interface interface-{interface_digits}"),
            Through::Create,
        ),
        (
            "a VPC endpoint of another VPC, through ReplaceRoute",
            "VpcEndpointId",
            endpoint_id,
            "DestinationCidrBlock",
            "203.0.113.0/24",
            format!("interface interface-{interface_digits}"),
            Through::Replace,
        ),
        (
            "an internet gateway attached to another VPC, through CreateRoute",
            "GatewayId",
            gateway_id.clone(),
            "DestinationCidrBlock",
            "192.0.2.0/24",
            format!("network gateway {gateway_id}"),
            Through::Create,
        ),
        (
            "an internet gateway attached to another VPC, through ReplaceRoute",
            "GatewayId",
            gateway_id.clone(),
            "DestinationCidrBlock",
            "203.0.113.0/24",
            format!("network gateway {gateway_id}"),
            Through::Replace,
        ),
        (
            "an egress-only internet gateway of another VPC, through CreateRoute",
            "EgressOnlyInternetGatewayId",
            egress_only_id.clone(),
            "DestinationIpv6CidrBlock",
            "2001:db8::/32",
            format!("network gateway {egress_only_id}"),
            Through::Create,
        ),
    ];

    // The route a `ReplaceRoute` row re-points: one the table already holds, pointed at a
    // target of its OWN VPC, so what the row changes is only the target being resolved.
    let local_gateway = attached_internet_gateway(case, &router, &here.vpc_id).await;
    ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &here.route_table_id),
            param("DestinationCidrBlock", "203.0.113.0/24"),
            param("GatewayId", &local_gateway),
        ],
    )
    .await;

    for (row, member_name, value, destination_member, destination, named, through) in rows {
        let action = match through {
            Through::Create => "CreateRoute",
            Through::Replace => "ReplaceRoute",
        };
        let (status, body) = call(
            &router,
            action,
            &[
                param("RouteTableId", &here.route_table_id),
                param(destination_member, destination),
                param(member_name, &value),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, INVALID_PARAMETER_VALUE),
            "case {row:?}: expected 400 {INVALID_PARAMETER_VALUE} from {action}, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "route table {table} and {named} belong to different networks",
                table = here.route_table_id,
            ),
            "case {row:?}: expected the refusal to name the table and the target"
        );
    }

    // Nothing the refusals touched moved: the one route the table holds still points where it
    // was created pointing, and the cross-VPC destinations route nowhere at all.
    assert_eq!(
        route_target(case, &router, &here.route_table_id, "203.0.113.0/24").await,
        Some(local_gateway),
        "case {case:?}: a refused ReplaceRoute must leave the route pointing where it was"
    );
    for destination in [EVERY_DESTINATION, "192.0.2.0/24", "2001:db8::/32"] {
        assert_eq!(
            route_target(case, &router, &here.route_table_id, destination).await,
            None,
            "case {case:?}: a refused CreateRoute for {destination} must store no route"
        );
    }
}
