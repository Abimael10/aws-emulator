// SPDX-License-Identifier: CC0-1.0

//! The route tables a GATEWAY `CreateVpcEndpoint` associates, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: `RouteTableIds` is a member this operation SERVES out of state. A
//! well-formed gateway-endpoint create carrying it is answered HTTP 200 with an endpoint
//! whose association set is the tables the request named — in request order, a table named
//! more than once asked for once — each of those tables then carries the route that reaches
//! the service through the prefix list the Region publishes for it, and
//! `DescribeVpcEndpoints` reports exactly that set; a set the service cannot serve is
//! refused with the EC2 query-protocol code for its cause, and nothing the request named is
//! answered with a not-implemented status.
//!
//! The rows differ along what the rule generalises over: which gateway endpoint service, in
//! which Region, how many of the VPC's tables the create names, whether a table is named
//! more than once, and whether the create spells the endpoint type or leaves it to the
//! documented default. The refusal rows differ along what makes a set unservable — the
//! resource the id names, the network the table routes for, the endpoint type the member is
//! combined with, a destination the table already routes, and how many ids one request may
//! name, at that bound and one past it.
//!
//! Siblings: [`super::create_vpc_endpoint`] covers the create as a whole,
//! [`super::gateway_endpoint_availability`] the state a gateway endpoint is created in, and
//! [`super::modify_vpc_endpoint`] the tables added to and removed from an endpoint that
//! already exists.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{child_text, param, Param};

/// The Region most calls here are signed for.
const REGION: &str = "us-east-1";

/// A second Region, so nothing asserted below is tied to the Region the issue's own repro
/// used: a gateway endpoint is created to the service of whatever Region the call names.
const OTHER_REGION: &str = "eu-west-1";

/// The block the VPC the endpoints are created in is given.
const VPC_CIDR: &str = "10.61.0.0/16";

/// The block of a SECOND VPC, whose route table routes for a network the endpoint cannot
/// carry traffic for.
const OTHER_VPC_CIDR: &str = "10.62.0.0/16";

/// A subnet of the first VPC, for the rows that create an endpoint type that stands up an
/// endpoint network interface.
const SUBNET_CIDR: &str = "10.61.1.0/24";

/// The zone that subnet sits in.
const SUBNET_ZONE: &str = "us-east-1a";

/// A well-formed route table id (the long form, 17 hexadecimal digits) the account holds no
/// table for.
const ABSENT_ROUTE_TABLE: &str = "rtb-0000000000000f00d";

/// The status a scaffold answers for a request shape nobody has implemented — the status
/// every row below must NOT see, since `RouteTableIds` is served.
const NOT_IMPLEMENTED: u16 = 501;

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call signed for `region`, as its HTTP status and XML body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> (u16, String) {
    super::ec2_query::call(router, region, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The association set a VPC endpoint reports: the `<item>`s of its `routeTableIdSet`, and
/// nothing at all for an endpoint associated with no table.
fn reported_tables(body: &str) -> Vec<String> {
    element(body, "routeTableIdSet")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default()
}

/// A VPC with `cidr_block`, as its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, region: &str, cidr: &str) -> String {
    let body = ok(case, router, region, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    member(case, &body, "vpcId")
}

/// A route table of `vpc_id`, as its id.
async fn create_route_table(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    vpc_id: &str,
) -> String {
    let body = ok(
        case,
        router,
        region,
        "CreateRouteTable",
        &[param("VpcId", vpc_id)],
    )
    .await;
    member(case, &body, "routeTableId")
}

/// A subnet of `vpc_id` in [`SUBNET_ZONE`], as its id.
async fn create_subnet(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    vpc_id: &str,
) -> String {
    let body = ok(
        case,
        router,
        region,
        "CreateSubnet",
        &[
            param("VpcId", vpc_id),
            param("CidrBlock", SUBNET_CIDR),
            param("AvailabilityZone", SUBNET_ZONE),
        ],
    )
    .await;
    member(case, &body, "subnetId")
}

/// Every route `DescribeRouteTables` reports for the one table `route_table_id`, as
/// `(destinationPrefixListId, gatewayId)` — the two members a gateway endpoint's own route
/// carries, read through the XML parser so a route is never confused with an association.
async fn routes(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    route_table_id: &str,
) -> Vec<(String, String)> {
    let body = ok(
        case,
        router,
        region,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    let document = super::wire::document(case, &body);
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "routeSet")
        .map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .map(|route| {
                    (
                        child_text(route, "destinationPrefixListId").unwrap_or_default(),
                        child_text(route, "gatewayId").unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The routes of `route_table_id` whose target is `endpoint_id`, as the prefix list each one
/// reaches the service through.
async fn routes_through(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    route_table_id: &str,
    endpoint_id: &str,
) -> Vec<String> {
    routes(case, router, region, route_table_id)
        .await
        .into_iter()
        .filter(|(_, gateway_id)| gateway_id == endpoint_id)
        .map(|(prefix_list_id, _)| prefix_list_id)
        .collect()
}

/// Every VPC endpoint the account holds, as `DescribeVpcEndpoints` reports them — the
/// "nothing was stored" baseline a refusal is held to.
async fn endpoint_ids(case: &str, router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let body = ok(case, router, region, "DescribeVpcEndpoints", &[]).await;
    elements(&body, "vpcEndpointId")
}

/// The association set `DescribeVpcEndpoints` reports for the one endpoint `endpoint_id`.
async fn described_tables(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    endpoint_id: &str,
) -> Vec<String> {
    let body = ok(
        case,
        router,
        region,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    reported_tables(&body)
}

/// The id of the prefix list Region `region` publishes under the endpoint service name
/// `service_name`, read off `DescribePrefixLists` rather than written out here — the
/// destination of the route a gateway endpoint adds to each table it is associated with.
async fn published_prefix_list_id(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    service_name: &str,
) -> String {
    let body = ok(case, router, region, "DescribePrefixLists", &[]).await;
    let document = super::wire::document(case, &body);
    document
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "prefixListId").is_some())
        .find(|node| child_text(*node, "prefixListName").as_deref() == Some(service_name))
        .and_then(|node| child_text(node, "prefixListId"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: {region} publishes no prefix list for {service_name}: {body}")
        })
}

/// `n` distinct well-formed route table ids the account holds no table for.
fn absent_route_tables(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("rtb-{index:017x}")).collect()
}

// ---------------------------------------------------------------------------
// What a create associates
// ---------------------------------------------------------------------------

/// One row: a gateway endpoint created in a VPC that holds `tables` route tables, naming the
/// ones at the positions `named`.
struct AssociationCase {
    case: &'static str,
    /// The Region the call is signed for, and the Region of the service it names.
    region: &'static str,
    /// The endpoint service, as the last label of `com.amazonaws.<region>.<label>` — one of
    /// the two services `DescribeVpcEndpointServices` offers a Gateway endpoint to.
    service_label: &'static str,
    /// Whether the create spells `VpcEndpointType` or leaves it to the documented default
    /// ("Default: Gateway").
    names_type: bool,
    /// How many route tables the row's VPC holds.
    tables: usize,
    /// Which of those tables the create names, by position — repeats included.
    named: &'static [usize],
    /// The positions the endpoint must report, in the order it must report them.
    expected: &'static [usize],
}

/// The rows: both gateway endpoint services, one and several tables, a table named twice on
/// its own and repeated between others, the defaulted spelling of the type, a create naming
/// no table at all, and a Region other than the one the issue's repro used.
fn association_cases() -> Vec<AssociationCase> {
    vec![
        AssociationCase {
            case: "one route table, the shape a client building S3 plumbing sends",
            region: REGION,
            service_label: "s3",
            names_type: true,
            tables: 2,
            named: &[0],
            expected: &[0],
        },
        AssociationCase {
            case: "three route tables of the VPC",
            region: REGION,
            service_label: "dynamodb",
            names_type: true,
            tables: 3,
            named: &[0, 1, 2],
            expected: &[0, 1, 2],
        },
        AssociationCase {
            case: "one route table named twice",
            region: REGION,
            service_label: "s3",
            names_type: true,
            tables: 2,
            named: &[1, 1],
            expected: &[1],
        },
        AssociationCase {
            case: "a route table repeated around another, in request order",
            region: REGION,
            service_label: "dynamodb",
            names_type: true,
            tables: 3,
            named: &[2, 0, 2],
            expected: &[2, 0],
        },
        AssociationCase {
            case: "two route tables with the type left to the documented default",
            region: REGION,
            service_label: "s3",
            names_type: false,
            tables: 2,
            named: &[0, 1],
            expected: &[0, 1],
        },
        AssociationCase {
            case: "no route table at all",
            region: REGION,
            service_label: "s3",
            names_type: true,
            tables: 1,
            named: &[],
            expected: &[],
        },
        AssociationCase {
            case: "one route table, in another Region",
            region: OTHER_REGION,
            service_label: "s3",
            names_type: true,
            tables: 2,
            named: &[0],
            expected: &[0],
        },
    ]
}

/// Every row is served: the create answers 200 with the tables it named, the describe reports
/// the same set, and each named table carries exactly one route to the endpoint, destined for
/// the prefix list the Region publishes for the service.
#[tokio::test]
async fn each_named_route_table_is_associated_and_routed_through_the_endpoint() {
    for row in association_cases() {
        let case = row.case;
        let region = row.region;
        // One emulator per row, so the tables a row names and the routes they carry are its
        // own.
        let router = emulator();
        let service_name = format!("com.amazonaws.{region}.{label}", label = row.service_label);
        let vpc_id = create_vpc(case, &router, region, VPC_CIDR).await;
        let mut tables = Vec::new();
        for _ in 0..row.tables {
            tables.push(create_route_table(case, &router, region, &vpc_id).await);
        }

        let mut params = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", &service_name),
        ];
        if row.names_type {
            params.push(param("VpcEndpointType", "Gateway"));
        }
        let named: Vec<&str> = row
            .named
            .iter()
            .map(|position| tables[*position].as_str())
            .collect();
        params.extend(indexed("RouteTableId", &named));

        let (status, body) = call(&router, region, "CreateVpcEndpoint", &params).await;
        assert_ne!(
            status, NOT_IMPLEMENTED,
            "case {case:?}: a create carrying RouteTableIds is served rather than refused as \
             unimplemented, got {status} with body {body}"
        );
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a well-formed gateway create, got {status} \
             with body {body}"
        );

        let expected: Vec<String> = row
            .expected
            .iter()
            .map(|position| tables[*position].clone())
            .collect();
        assert_eq!(
            reported_tables(&body),
            expected,
            "case {case:?}: expected the endpoint to report {expected:?}, got {body}"
        );
        let endpoint_id = member(case, &body, "vpcEndpointId");
        assert_eq!(
            member(case, &body, "vpcEndpointType"),
            ec2::state::VPC_ENDPOINT_TYPE_GATEWAY,
            "case {case:?}: expected a Gateway endpoint, got {body}"
        );
        assert_eq!(
            member(case, &body, "vpcId"),
            vpc_id,
            "case {case:?}: expected the endpoint in the VPC the request named, got {body}"
        );
        assert_eq!(
            member(case, &body, "serviceName"),
            service_name,
            "case {case:?}: expected the endpoint to the service the request named, got {body}"
        );

        let described = described_tables(case, &router, region, &endpoint_id).await;
        assert_eq!(
            described, expected,
            "case {case:?}: expected the describe to report {expected:?} for {endpoint_id}, \
             got {described:?}"
        );

        let prefix_list_id = published_prefix_list_id(case, &router, region, &service_name).await;
        for (position, route_table_id) in tables.iter().enumerate() {
            let through =
                routes_through(case, &router, region, route_table_id, &endpoint_id).await;
            let expected_routes = if row.expected.contains(&position) {
                vec![prefix_list_id.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(
                through, expected_routes,
                "case {case:?}: expected table {route_table_id} to hold the routes \
                 {expected_routes:?} through {endpoint_id}, got {through:?}"
            );
            // The local route every table comes with is untouched by the association, so the
            // table still routes its own VPC.
            let held = routes(case, &router, region, route_table_id).await;
            assert!(
                held.iter().any(|(_, gateway_id)| gateway_id == "local"),
                "case {case:?}: expected table {route_table_id} to keep its local route, got \
                 {held:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// What a create refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: the request, the status and EC2 code it must be answered
/// with, and the sentence that answer must carry when the sentence is the whole of it.
struct RefusalCase {
    case: &'static str,
    params: Vec<Param>,
    status: u16,
    code: String,
    /// The exact message, where the refusal's wording is a template of the request's own
    /// values; `None` where the row asserts only that the message names the member at fault,
    /// which `names` carries.
    message: Option<String>,
    /// A fragment the message has to name, for a row that does not pin the whole sentence.
    names: &'static str,
}

/// A set of route tables the service cannot serve is refused with the EC2 code for its cause
/// — never with a not-implemented status — and stores nothing: no endpoint appears and the
/// tables keep exactly the routes they had.
#[tokio::test]
async fn a_route_table_set_that_cannot_be_served_is_refused_with_the_code_for_its_cause() {
    let setup = "the estate the refusals are named against";
    let router = emulator();
    let vpc_id = create_vpc(setup, &router, REGION, VPC_CIDR).await;
    let subnet_id = create_subnet(setup, &router, REGION, &vpc_id).await;
    let first_table = create_route_table(setup, &router, REGION, &vpc_id).await;
    let spare_table = create_route_table(setup, &router, REGION, &vpc_id).await;
    let other_vpc_id = create_vpc(setup, &router, REGION, OTHER_VPC_CIDR).await;
    let other_table = create_route_table(setup, &router, REGION, &other_vpc_id).await;

    let gateway_service = format!("com.amazonaws.{REGION}.s3");
    let interface_service = format!("com.amazonaws.{REGION}.logs");
    let prefix_list_id =
        published_prefix_list_id(setup, &router, REGION, &gateway_service).await;

    // One endpoint the account really holds, reached through `first_table`: the row about a
    // destination a table already routes needs a table that really routes one.
    let held = ok(
        setup,
        &router,
        REGION,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", &gateway_service),
            param("VpcEndpointType", "Gateway"),
            param("RouteTableId.1", &first_table),
        ],
    )
    .await;
    let held_endpoint_id = member(setup, &held, "vpcEndpointId");

    let gateway_request = |tables: &[&str]| {
        let mut params = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", &gateway_service),
            param("VpcEndpointType", "Gateway"),
        ];
        params.extend(indexed("RouteTableId", tables));
        params
    };

    let at_the_bound = absent_route_tables(ec2::state::MAX_RESOURCE_IDS);
    let past_the_bound = absent_route_tables(ec2::state::MAX_RESOURCE_IDS + 1);
    let cases = vec![
        RefusalCase {
            case: "a route table id the account holds no table for",
            params: gateway_request(&[ABSENT_ROUTE_TABLE]),
            status: 400,
            code: ec2::state::INVALID_ROUTE_TABLE_ID_NOT_FOUND.to_string(),
            message: Some(format!(
                "The routeTable ID '{ABSENT_ROUTE_TABLE}' does not exist"
            )),
            names: "",
        },
        RefusalCase {
            case: "a route table that routes for another VPC",
            params: gateway_request(&[other_table.as_str()]),
            status: 400,
            code: ec2::state::INVALID_PARAMETER_VALUE.to_string(),
            message: Some(format!(
                "route table {other_table} and VPC {vpc_id} belong to different networks"
            )),
            names: "",
        },
        RefusalCase {
            case: "route tables on an interface endpoint, which has none",
            params: vec![
                param("VpcId", &vpc_id),
                param("ServiceName", &interface_service),
                param("VpcEndpointType", "Interface"),
                param("SubnetId.1", &subnet_id),
                param("RouteTableId.1", &spare_table),
            ],
            status: 400,
            code: ec2::state::INVALID_PARAMETER_COMBINATION.to_string(),
            message: None,
            names: "RouteTableIds",
        },
        RefusalCase {
            case: "route tables on a Gateway Load Balancer endpoint, which has none",
            params: vec![
                param("VpcId", &vpc_id),
                param("ServiceName", &gateway_service),
                param("VpcEndpointType", "GatewayLoadBalancer"),
                param("SubnetId.1", &subnet_id),
                param("RouteTableId.1", &spare_table),
            ],
            status: 400,
            code: ec2::state::INVALID_PARAMETER_COMBINATION.to_string(),
            message: None,
            names: "RouteTableIds",
        },
        RefusalCase {
            case: "a route table that already reaches the service",
            params: gateway_request(&[first_table.as_str()]),
            status: 400,
            code: ec2::state::ROUTE_ALREADY_EXISTS.to_string(),
            message: Some(format!(
                "The route identified by {prefix_list_id} already exists."
            )),
            names: "",
        },
        RefusalCase {
            case: "the most route tables a request may name, none of which exists",
            params: gateway_request(
                &at_the_bound
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<&str>>(),
            ),
            status: 400,
            code: ec2::state::INVALID_ROUTE_TABLE_ID_NOT_FOUND.to_string(),
            message: None,
            names: "does not exist",
        },
        RefusalCase {
            case: "one route table more than a request may name",
            params: gateway_request(
                &past_the_bound
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<&str>>(),
            ),
            status: 400,
            code: ec2::state::INVALID_PARAMETER_VALUE.to_string(),
            message: None,
            names: "RouteTableIds",
        },
    ];

    // Every table a row can name, so "nothing was written" is asked of all of them rather
    // than only of the one the setup associated.
    let every_table = [
        first_table.clone(),
        spare_table.clone(),
        other_table.clone(),
    ];

    for row in cases {
        let case = row.case;
        let before = endpoint_ids(case, &router, REGION).await;
        let mut routes_before = Vec::new();
        for route_table_id in &every_table {
            routes_before.push(routes(case, &router, REGION, route_table_id).await);
        }
        let (status, body) = call(&router, REGION, "CreateVpcEndpoint", &row.params).await;
        assert_ne!(
            status, NOT_IMPLEMENTED,
            "case {case:?}: a set the service cannot serve is refused with the code for its \
             cause rather than as unimplemented, got {status} with body {body}"
        );
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {expected}, got {status} with body {body}",
            expected = row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected the code {expected:?}, got {body}",
            expected = row.code
        );
        let reported = error_message(&body);
        match row.message {
            Some(expected) => assert_eq!(
                reported, expected,
                "case {case:?}: expected the message {expected:?}, got {reported:?}"
            ),
            None => assert!(
                reported.contains(row.names),
                "case {case:?}: expected the message to name {names:?}, got {reported:?}",
                names = row.names
            ),
        }
        assert_eq!(
            endpoint_ids(case, &router, REGION).await,
            before,
            "case {case:?}: a refused create must store no endpoint"
        );
        for (position, route_table_id) in every_table.iter().enumerate() {
            let routes_after = routes(case, &router, REGION, route_table_id).await;
            assert_eq!(
                Some(&routes_after),
                routes_before.get(position),
                "case {case:?}: a refused create must leave the routes of table \
                 {route_table_id} as it found them"
            );
        }
    }

    // The endpoint the setup created is still the one the account holds, still reached through
    // its own table: none of the refusals above touched it.
    assert_eq!(
        endpoint_ids(setup, &router, REGION).await,
        vec![held_endpoint_id.clone()],
        "the refusals must leave the endpoint the setup created in place"
    );
    assert_eq!(
        routes_through(setup, &router, REGION, &first_table, &held_endpoint_id).await,
        vec![prefix_list_id],
        "the endpoint the setup created must still be reached through its own table"
    );
}

// ---------------------------------------------------------------------------
// A repeat of the same request
// ---------------------------------------------------------------------------

/// "Unique, case-sensitive identifier that you provide to ensure the idempotency of the
/// request" (`ClientToken`): the same gateway create twice associates the tables once — the
/// repeat answers the endpoint already created and adds no second route to any table — and
/// the same token naming a different set is refused rather than answered with associations
/// the caller did not ask for.
#[tokio::test]
async fn a_repeated_client_token_associates_each_route_table_once() {
    let case = "a repeated client token";
    let router = emulator();
    let service_name = format!("com.amazonaws.{REGION}.s3");
    let vpc_id = create_vpc(case, &router, REGION, VPC_CIDR).await;
    let first_table = create_route_table(case, &router, REGION, &vpc_id).await;
    let second_table = create_route_table(case, &router, REGION, &vpc_id).await;
    let prefix_list_id = published_prefix_list_id(case, &router, REGION, &service_name).await;

    let request = |tables: &[&str]| {
        let mut params = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", "Gateway"),
            param("ClientToken", "the-one-token"),
        ];
        params.extend(indexed("RouteTableId", tables));
        params
    };
    let both = [first_table.as_str(), second_table.as_str()];

    let first = ok(
        case,
        &router,
        REGION,
        "CreateVpcEndpoint",
        &request(&both),
    )
    .await;
    let endpoint_id = member(case, &first, "vpcEndpointId");
    assert_eq!(
        reported_tables(&first),
        vec![first_table.clone(), second_table.clone()],
        "case {case:?}: expected the create to report both tables, got {first}"
    );

    let again = ok(
        case,
        &router,
        REGION,
        "CreateVpcEndpoint",
        &request(&both),
    )
    .await;
    assert_eq!(
        member(case, &again, "vpcEndpointId"),
        endpoint_id,
        "case {case:?}: expected the repeat to answer the endpoint the first call created, \
         got {again}"
    );
    assert_eq!(
        reported_tables(&again),
        vec![first_table.clone(), second_table.clone()],
        "case {case:?}: expected the repeat to report the same tables, got {again}"
    );
    assert_eq!(
        endpoint_ids(case, &router, REGION).await,
        vec![endpoint_id.clone()],
        "case {case:?}: expected the repeat to create no second endpoint"
    );
    for route_table_id in &both {
        let through = routes_through(case, &router, REGION, route_table_id, &endpoint_id).await;
        assert_eq!(
            through,
            vec![prefix_list_id.clone()],
            "case {case:?}: expected table {route_table_id} to hold one route to \
             {endpoint_id}, got {through:?}"
        );
    }

    let (status, body) = call(
        &router,
        REGION,
        "CreateVpcEndpoint",
        &request(&[first_table.as_str()]),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the token names another request, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::IDEMPOTENT_PARAMETER_MISMATCH,
        "case {case:?}: expected {expected}, got {body}",
        expected = ec2::state::IDEMPOTENT_PARAMETER_MISMATCH
    );
    assert_eq!(
        described_tables(case, &router, REGION, &endpoint_id).await,
        vec![first_table, second_table],
        "case {case:?}: a refused repeat must leave the associations as they were"
    );
}
