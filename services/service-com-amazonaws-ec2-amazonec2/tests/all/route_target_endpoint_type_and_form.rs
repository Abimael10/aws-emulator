// SPDX-License-Identifier: CC0-1.0

//! WHICH VPC endpoint a route may point at, and what a value that is no endpoint id is answered
//! with, driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a route's `VpcEndpointId` resolves ONE endpoint type — "the ID of a
//! VPC endpoint. Supported for Gateway Load Balancer endpoints only"
//! (`smithy_query(query="operation",operation="CreateRoute")`) — and an id of another resource
//! kind is no endpoint id at all**, so:
//!
//! * a value whose id PREFIX is not the VPC endpoint's is refused for its form, before the
//!   account is consulted: `InvalidVpcEndpointId.Malformed`, "The vpcEndpoint ID '<value>' is
//!   malformed";
//! * a value under the endpoint prefix that names no Gateway Load Balancer endpoint of the
//!   account — an endpoint of another type as much as an id nothing minted — is refused
//!   `InvalidVpcEndpointId.NotFound`, "The vpcEndpoint ID '<value>' does not exist";
//! * only a Gateway Load Balancer endpoint of the route table's own VPC is accepted, and the
//!   stored route reports it in `Route$GatewayId`.
//!
//! It is a rule about the TARGET, not about the call, so both operations that point a route at
//! one answer it identically: `CreateRoute`, which stores a route, and `ReplaceRoute`, which
//! re-points one. The rows differ along what the rule generalises over: the endpoint TYPE the
//! value names (the types this service creates, out of the `VpcEndpointType` enum —
//! `smithy_query(query="shape",shape="VpcEndpointType")` lists Gateway, GatewayLoadBalancer,
//! Interface, Resource, ServiceNetwork and Tunnel), which resource kind the id's prefix names,
//! both published id lengths and a suffix of no length EC2 issues, the two sibling families
//! whose own prefixes extend the endpoint's, the case of the prefix, the prefix with nothing
//! behind it, and which of the two operations is driven.
//!
//! The suite's last test is the rule the target members share: EXACTLY ONE of them, which EC2
//! states in one fixed sentence enumerating the members — the same sentence for a request that
//! names none and for one that names several, each keeping its own error code.
//!
//! [`super::route_target_vpc_endpoint`] drives the endpoint target a client holds, and
//! [`super::route_target_member_resolution`] which member resolves which resource kind.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An endpoint service of the PARTNER family (`com.amazonaws.vpce.<region>.<service-id>`), the
/// family a Gateway Load Balancer endpoint of another account's service is created to.
const GATEWAY_LOAD_BALANCER_SERVICE: &str =
    "com.amazonaws.vpce.us-west-2.vpce-svc-0123456789abcdef0";

/// A GATEWAY endpoint service of this Region: the family reached through a route EC2 maintains
/// in the tables the endpoint names, rather than through a route a caller writes.
const GATEWAY_SERVICE: &str = "com.amazonaws.us-west-2.s3";

/// An INTERFACE endpoint service of this Region, reached through endpoint network interfaces.
const INTERFACE_SERVICE: &str = "com.amazonaws.us-west-2.logs";

/// EC2's code for a value it cannot read as an id of the kind the parameter names.
const MALFORMED: &str = "InvalidVpcEndpointId.Malformed";

/// EC2's code for an id of this kind the account has no such resource for.
const NOT_FOUND: &str = ec2::state::INVALID_VPC_ENDPOINT_ID_NOT_FOUND;

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

/// The estate the targets are named to: one VPC with a subnet and a route table, the endpoints
/// of each type this service creates in it, the internet gateway the route under test starts
/// out pointing at, and the network interface the load balancer endpoint holds.
struct Estate {
    vpc_id: String,
    subnet_id: String,
    route_table_id: String,
    internet_gateway_id: String,
    /// The one endpoint a route may point at.
    load_balancer_endpoint_id: String,
    /// A GATEWAY endpoint of the same VPC, which is not a route target.
    gateway_endpoint_id: String,
    /// An INTERFACE endpoint of the same VPC, which is not a route target either.
    interface_endpoint_id: String,
    /// The load balancer endpoint's own endpoint network interface.
    network_interface_id: String,
}

/// Create that estate, with one route of the table pointing at the internet gateway.
async fn estate(router: &MultiServiceRouter) -> Estate {
    let case = "the estate the endpoint targets are named to";
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.70.0.0/16")]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.70.1.0/24"),
            param("AvailabilityZone", &format!("{REGION}a")),
        ],
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
    let load_balancer = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", GATEWAY_LOAD_BALANCER_SERVICE),
            param("VpcEndpointType", "GatewayLoadBalancer"),
            param("SubnetId.1", &subnet_id),
        ],
    )
    .await;
    let load_balancer_endpoint_id = member(case, &load_balancer, "vpcEndpointId");
    let network_interface_id = first_network_interface(case, &load_balancer);
    let gateway_endpoint = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", GATEWAY_SERVICE),
            param("VpcEndpointType", "Gateway"),
        ],
    )
    .await;
    let gateway_endpoint_id = member(case, &gateway_endpoint, "vpcEndpointId");
    let interface_endpoint = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", INTERFACE_SERVICE),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &subnet_id),
            // The VPC's DNS attributes are off by default and private DNS needs both, which is
            // a rule of its own; this endpoint is here only as a TYPE a route may not name.
            param("PrivateDnsEnabled", "false"),
        ],
    )
    .await;
    let interface_endpoint_id = member(case, &interface_endpoint, "vpcEndpointId");
    let table = ok(case, router, "CreateRouteTable", &[param("VpcId", &vpc_id)]).await;
    let route_table_id = member(case, &table, "routeTableId");
    // The route a `ReplaceRoute` row re-points: one the table already holds, pointing at a
    // target of its own VPC, so what a row changes is only the target being resolved.
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
        vpc_id,
        subnet_id,
        route_table_id,
        internet_gateway_id,
        load_balancer_endpoint_id,
        gateway_endpoint_id,
        interface_endpoint_id,
        network_interface_id,
    }
}

/// The first `networkInterfaceIdSet` entry of a created endpoint — the endpoint network
/// interface it holds in its subnet.
fn first_network_interface(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    let endpoint = parsed
        .descendants()
        .find(|node| node.is_element() && child_text(*node, "vpcEndpointId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no endpoint in {body}"));
    set_items(endpoint, "networkInterfaceIdSet")
        .first()
        .and_then(|item| item.text())
        .unwrap_or_else(|| panic!("case {case:?}: the endpoint reported no interface: {body}"))
        .to_string()
}

/// The target `DescribeRouteTables` reports for the route to `destination`, or `None` when the
/// table routes that destination nowhere.
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
        .find(|item| child_text(*item, "destinationCidrBlock").as_deref() == Some(destination))
        .and_then(|item| child_text(item, "gatewayId"))
}

/// Which operation a row drives: the two that resolve a route's target.
#[derive(Clone, Copy)]
enum Through {
    /// `CreateRoute`, which resolves the target of a new route.
    Create,
    /// `ReplaceRoute`, which resolves the target a route already in the table is pointed at.
    Replace,
}

impl Through {
    /// The operation's name on the wire.
    fn action(self) -> &'static str {
        match self {
            Self::Create => "CreateRoute",
            Self::Replace => "ReplaceRoute",
        }
    }

    /// The destination the row addresses: a fresh one for a create, the route the table already
    /// holds for a replacement.
    fn destination(self, index: usize) -> String {
        match self {
            Self::Create => format!("192.0.{index}.0/24"),
            Self::Replace => "0.0.0.0/0".to_string(),
        }
    }
}

/// One refused row: the target it names, which operation names it, and the refusal EC2 answers.
struct Refused<'a> {
    /// The row's name, which every assertion carries.
    row: &'a str,
    /// The value `VpcEndpointId` carries.
    value: &'a str,
    /// Which operation resolves it.
    through: Through,
    /// Its place in the table, which gives a create its own destination.
    index: usize,
    /// The error code the refusal carries.
    code: &'a str,
    /// The sentence it carries.
    message: String,
}

/// Drive one refusal row and assert its code, its sentence, and that it wrote nothing.
async fn refused(router: &MultiServiceRouter, estate: &Estate, refusal: Refused<'_>) {
    let Refused {
        row,
        value,
        through,
        index,
        code: expected_code,
        message: expected_message,
    } = refusal;
    let destination = through.destination(index);
    let case = format!("{row}, through {action}", action = through.action());
    let (status, body) = call(
        router,
        through.action(),
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", &destination),
            param("VpcEndpointId", value),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the target {value} to be refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        expected_code,
        "case {case:?}: expected {expected_code}, got {body}"
    );
    assert_eq!(
        error_message(&body),
        expected_message,
        "case {case:?}: expected the sentence naming the value the request carried, got {body}"
    );
    match through {
        // A refused create stores no route at all.
        Through::Create => assert_eq!(
            route_target(&case, router, &estate.route_table_id, &destination).await,
            None,
            "case {case:?}: a refused {action} must store no route",
            action = through.action()
        ),
        // A refused replacement leaves the route pointing where it was.
        Through::Replace => assert_eq!(
            route_target(&case, router, &estate.route_table_id, &destination).await,
            Some(estate.internet_gateway_id.clone()),
            "case {case:?}: a refused {action} must leave the route pointing where it was",
            action = through.action()
        ),
    }
}

/// RULE, first half: a value under the endpoint prefix that names no Gateway Load Balancer
/// endpoint of the account is refused as an endpoint the account does not have — an endpoint of
/// another TYPE exactly as much as an id nothing minted — while the load balancer endpoint of
/// the table's own VPC is accepted.
#[tokio::test]
async fn only_a_gateway_load_balancer_endpoint_is_a_route_target() {
    let router = emulator();
    let estate = estate(&router).await;

    // (the row, the value `VpcEndpointId` carries)
    let rows: Vec<(String, String)> = vec![
        (
            "a GATEWAY endpoint of the route table's own VPC".to_string(),
            estate.gateway_endpoint_id.clone(),
        ),
        (
            "an INTERFACE endpoint of the route table's own VPC".to_string(),
            estate.interface_endpoint_id.clone(),
        ),
        (
            "a vpce- id nothing minted, in the long form".to_string(),
            "vpce-0a1b2c3d4e5f60718".to_string(),
        ),
        (
            "a vpce- id nothing minted, in the short form".to_string(),
            "vpce-0a1b2c3d".to_string(),
        ),
        (
            // A suffix of a length EC2 does not issue is still under the endpoint's own
            // prefix, so the route operations hand it to the endpoints to answer for: their
            // recorded refusal of such a value is this not-found and not a malformed-id one.
            // The operations whose SUBJECT is the endpoint read the suffix more strictly, on
            // their own recorded refusals; which reading an operation applies is a fact of
            // that operation's validator.
            "a vpce- value whose suffix is no length EC2 issues".to_string(),
            "vpce-0a1b2c3d4e5f6071".to_string(),
        ),
    ];
    for (index, (row, value)) in rows.iter().enumerate() {
        for through in [Through::Create, Through::Replace] {
            refused(
                &router,
                &estate,
                Refused {
                    row,
                    value,
                    through,
                    index,
                    code: NOT_FOUND,
                    message: format!("The vpcEndpoint ID '{value}' does not exist"),
                },
            )
            .await;
        }
    }

    // The other side of the same resolution: the ONE endpoint type a route may point at, in the
    // table's own VPC, is accepted by both operations and reported in the route's `gatewayId`.
    let case = "a replacement at the load balancer endpoint of the table's own VPC";
    ok(
        case,
        &router,
        "ReplaceRoute",
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", "0.0.0.0/0"),
            param("VpcEndpointId", &estate.load_balancer_endpoint_id),
        ],
    )
    .await;
    assert_eq!(
        route_target(case, &router, &estate.route_table_id, "0.0.0.0/0").await,
        Some(estate.load_balancer_endpoint_id.clone()),
        "case {case:?}: expected the route to point at the endpoint"
    );
    let case = "a create at the load balancer endpoint of the table's own VPC";
    let created = ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", "198.51.100.0/24"),
            param("VpcEndpointId", &estate.load_balancer_endpoint_id),
        ],
    )
    .await;
    assert_eq!(
        element(&created, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected return=true for the load balancer endpoint, got {created}"
    );
    assert_eq!(
        route_target(case, &router, &estate.route_table_id, "198.51.100.0/24").await,
        Some(estate.load_balancer_endpoint_id.clone()),
        "case {case:?}: expected the stored route to report the endpoint it points at"
    );
}

/// RULE, second half: a value whose id PREFIX names another resource kind is no endpoint id at
/// all, so it is refused for its FORM — before the account is asked whether it holds such an
/// endpoint — and nothing is written.
#[tokio::test]
async fn a_value_that_is_no_endpoint_id_is_refused_for_its_form() {
    let router = emulator();
    let estate = estate(&router).await;

    // One row per kind of value a caller reaches for that is not an endpoint id: resources of
    // the estate whose ids a client has at hand (the VPC, the subnet, the route table, the
    // internet gateway, the endpoint's own network interface), the same prefix in another case,
    // and a value that is no resource id at all.
    //
    // (the row, the value `VpcEndpointId` carries)
    let rows: Vec<(String, String)> = vec![
        ("the VPC's own id".to_string(), estate.vpc_id.clone()),
        ("the subnet's id".to_string(), estate.subnet_id.clone()),
        (
            "the route table's own id".to_string(),
            estate.route_table_id.clone(),
        ),
        (
            "the internet gateway attached to the VPC".to_string(),
            estate.internet_gateway_id.clone(),
        ),
        (
            "the endpoint's own network interface".to_string(),
            estate.network_interface_id.clone(),
        ),
        (
            "the endpoint prefix in another case".to_string(),
            "VPCE-0a1b2c3d4e5f60718".to_string(),
        ),
        (
            "the endpoint prefix with no suffix behind it".to_string(),
            "vpce-".to_string(),
        ),
        (
            "the endpoint prefix with no separator behind it".to_string(),
            "vpce".to_string(),
        ),
        (
            // The ids of the other resources whose own prefixes EXTEND the endpoint's carry
            // `svc-` or `conn-` where an endpoint id carries nothing but hex digits, so they
            // are no endpoint ids here either — the same separation the operations whose
            // SUBJECT is an endpoint make.
            "an endpoint SERVICE id".to_string(),
            "vpce-svc-0123456789abcdef0".to_string(),
        ),
        (
            "an endpoint CONNECTION id".to_string(),
            "vpce-conn-0a1b2c3d4e5f60718".to_string(),
        ),
        (
            "a vpce- value whose suffix is no hexadecimal".to_string(),
            "vpce-firewallendpoint".to_string(),
        ),
        (
            "a value that is no resource id at all".to_string(),
            "firewall-endpoint".to_string(),
        ),
    ];
    for (index, (row, value)) in rows.iter().enumerate() {
        for through in [Through::Create, Through::Replace] {
            refused(
                &router,
                &estate,
                Refused {
                    row,
                    value,
                    through,
                    index,
                    code: MALFORMED,
                    message: format!("The vpcEndpoint ID '{value}' is malformed"),
                },
            )
            .await;
        }
    }
}

/// The rule the target members share: EXACTLY ONE of them. "You must also specify exactly one of
/// the resources from the parameter list" (`CreateRoute`), so naming none and naming several
/// break one rule, which EC2 states in one fixed sentence enumerating the members and
/// interpolating nothing from the request — the same sentence whichever members the request
/// carried, and whichever of the two operations resolved them. Each refusal keeps its own code:
/// a request that names none is missing a parameter, one that names several carries a
/// combination.
#[tokio::test]
async fn naming_no_target_and_naming_several_answer_one_fixed_sentence() {
    let router = emulator();
    let estate = estate(&router).await;

    // (the row, the target members the request carries beside its destination, the code)
    let rows: Vec<(&str, Vec<Param>, &str)> = vec![
        ("no target at all", Vec::new(), "MissingParameter"),
        (
            "two targets, an endpoint and a gateway",
            vec![
                param("VpcEndpointId", &estate.load_balancer_endpoint_id),
                param("GatewayId", &estate.internet_gateway_id),
            ],
            "InvalidParameterCombination",
        ),
        (
            "two targets neither of which the account holds",
            vec![
                param("VpcEndpointId", "vpce-0a1b2c3d4e5f60718"),
                param("TransitGatewayId", "tgw-0a1b2c3d4e5f60718"),
            ],
            "InvalidParameterCombination",
        ),
        (
            "three targets",
            vec![
                param("VpcEndpointId", &estate.load_balancer_endpoint_id),
                param("GatewayId", &estate.internet_gateway_id),
                param("NetworkInterfaceId", &estate.network_interface_id),
            ],
            "InvalidParameterCombination",
        ),
    ];
    // The sentence EC2 answers, which names the target members and nothing from the request.
    let expected = "The request must contain exactly one of gatewayId, localGatewayId, \
                    carrierGatewayId, natGatewayId, networkInterfaceId, vpcPeeringConnectionId, \
                    egressOnlyInternetGatewayId, transitGatewayId, vpcEndpointId, \
                    coreNetworkArn or instanceId";

    for (index, (row, targets, expected_code)) in rows.iter().enumerate() {
        for through in [Through::Create, Through::Replace] {
            let destination = through.destination(index);
            let case = format!("{row}, through {action}", action = through.action());
            let mut params = vec![
                param("RouteTableId", &estate.route_table_id),
                param("DestinationCidrBlock", &destination),
            ];
            params.extend(targets.iter().cloned());
            let (status, body) = call(&router, through.action(), &params).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, *expected_code),
                "case {case:?}: expected 400 {expected_code}, got {status} with body {body}"
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected the one fixed enumeration of the target members, got {}",
                error_message(&body)
            );
        }
    }

    // And the control: exactly one target, which is what the rule admits.
    let case = "exactly one target";
    let created = ok(
        case,
        &router,
        "CreateRoute",
        &[
            param("RouteTableId", &estate.route_table_id),
            param("DestinationCidrBlock", "203.0.113.0/24"),
            param("VpcEndpointId", &estate.load_balancer_endpoint_id),
        ],
    )
    .await;
    assert_eq!(
        element(&created, "return").as_deref(),
        Some("true"),
        "case {case:?}: expected return=true, got {created}"
    );
}
