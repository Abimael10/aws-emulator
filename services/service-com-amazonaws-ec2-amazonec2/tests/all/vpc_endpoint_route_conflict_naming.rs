// SPDX-License-Identifier: CC0-1.0

//! How a GATEWAY VPC endpoint create and modify REFUSE a route table that already routes the
//! endpoint service's managed prefix list, driven over `ec2Query` (README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! RULE: the conflict is over the prefix-list ROUTE rather than over the endpoint object, so
//! the refusal is HTTP 400 `RouteAlreadyExists` whose sentence names both sides of it — the
//! route table the request asked to associate and the prefix list that table already has a
//! route for. Which of the tables one request named is the occupied one is the whole
//! actionable content of the message, so it is a different sentence from the one `CreateRoute`
//! answers a duplicate destination with, which has a single table to be about.
//!
//! The rows differ along what the rule generalises over: the endpoint service (and so the
//! prefix list the route is destined for), how many tables the refused request names and
//! WHICH of them is the occupied one, the operation that asks for the association
//! (`CreateVpcEndpoint` and `ModifyVpcEndpoint`), and whether the occupying route was added by
//! an endpoint of the same service or of another one.
//!
//! The prefix list id in every expected sentence is read off `DescribePrefixLists` rather than
//! written out here: it is the Region's own published value for that service name, not a value
//! of one run.
//!
//! Siblings: [`super::vpc_endpoint_prefix_list_route_conflict`] covers the same refusal from
//! the table's side (which service's prefix list makes a table occupied),
//! [`super::create_vpc_endpoint_route_table_associations`] the association set a create
//! serves, and [`super::modify_vpc_endpoint`] the tables added to an endpoint that already
//! exists. What this suite adds is the REPEAT of an identical create, the refusal read over
//! both operations in one estate, and the store being left exactly as it was.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{child_text, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The block of the VPC the endpoints are created in.
const VPC_CIDR: &str = "10.63.0.0/16";

/// The emulator the suite drives.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call, as its HTTP status and XML body.
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

/// One element of a body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The endpoint service name of `service` in [`REGION`].
fn service_name(service: &str) -> String {
    format!("com.amazonaws.{REGION}.{service}")
}

/// The id of the prefix list [`REGION`] publishes under the endpoint service name
/// `service_name`, read off `DescribePrefixLists` — the destination of the route a gateway
/// endpoint adds to each table it is associated with.
async fn published_prefix_list_id(
    case: &str,
    router: &MultiServiceRouter,
    service_name: &str,
) -> String {
    let body = ok(case, router, "DescribePrefixLists", &[]).await;
    let document = super::wire::document(case, &body);
    document
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "prefixListId").is_some())
        .find(|node| child_text(*node, "prefixListName").as_deref() == Some(service_name))
        .and_then(|node| child_text(node, "prefixListId"))
        .unwrap_or_else(|| panic!("case {case:?}: {REGION} publishes no prefix list for {service_name}: {body}"))
}

/// A gateway endpoint of `service` reaching `tables`, as the endpoint id it minted.
async fn create_gateway_endpoint(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    service: &str,
    tables: &[&str],
) -> String {
    let body = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &gateway_request(vpc_id, service, tables),
    )
    .await;
    member(case, &body, "vpcEndpointId")
}

/// The members of a gateway `CreateVpcEndpoint` for `service` naming `tables`.
fn gateway_request(vpc_id: &str, service: &str, tables: &[&str]) -> Vec<Param> {
    let mut params = vec![
        param("VpcId", vpc_id),
        param("ServiceName", &service_name(service)),
        param("VpcEndpointType", "Gateway"),
    ];
    params.extend(indexed("RouteTableId", tables));
    params
}

/// A route table of `vpc_id`, as its id.
async fn create_route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    member(case, &body, "routeTableId")
}

/// Every endpoint id the account holds, as `DescribeVpcEndpoints` reports them.
async fn endpoint_ids(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let body = ok(case, router, "DescribeVpcEndpoints", &[]).await;
    elements(&body, "vpcEndpointId")
}

/// The association set `DescribeVpcEndpoints` reports for the one endpoint `endpoint_id`.
async fn described_tables(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> Vec<String> {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    element(&body, "routeTableIdSet")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default()
}

/// One row: the case, the request that must be refused, the table the sentence has to name and
/// the service whose prefix list it has to name.
struct Row {
    case: &'static str,
    action: &'static str,
    params: Vec<Param>,
    occupied_table: String,
    service: &'static str,
}

/// RULE: an association asked for on a table that already routes the service's prefix list is
/// refused `RouteAlreadyExists`, naming that table and that prefix list — and the store is
/// left exactly as it was.
#[tokio::test]
async fn a_route_table_that_already_routes_the_prefix_list_is_refused_naming_both() {
    let setup = "the estate the refusals are named against";
    let router = emulator();
    let vpc_id = create_vpc(setup, &router, VPC_CIDR).await;
    let first_table = create_route_table(setup, &router, &vpc_id).await;
    let second_table = create_route_table(setup, &router, &vpc_id).await;
    let third_table = create_route_table(setup, &router, &vpc_id).await;
    // A table no endpoint ever reaches, so a row can name a CLEAN table beside the occupied
    // one and see which of the two the sentence is about.
    let clean_table = create_route_table(setup, &router, &vpc_id).await;

    // The occupying endpoints: one for each service, so a row can be refused over the prefix
    // list of either and a table occupied for one service is still free for the other.
    let s3_endpoint = create_gateway_endpoint(
        setup,
        &router,
        &vpc_id,
        "s3",
        &[first_table.as_str(), second_table.as_str()],
    )
    .await;
    let dynamodb_endpoint =
        create_gateway_endpoint(setup, &router, &vpc_id, "dynamodb", &[second_table.as_str()])
            .await;
    // A second endpoint of a service the tables are free for, so the modify rows have an
    // endpoint to add a table to.
    let movable_endpoint =
        create_gateway_endpoint(setup, &router, &vpc_id, "s3", &[third_table.as_str()]).await;

    let modify_request = |endpoint_id: &str, tables: &[&str]| {
        let mut params = vec![param("VpcEndpointId", endpoint_id)];
        params.extend(indexed("AddRouteTableId", tables));
        params
    };

    let rows: Vec<Row> = vec![
        Row {
            case: "the identical create again: the table it already reaches",
            action: "CreateVpcEndpoint",
            params: gateway_request(&vpc_id, "s3", &[first_table.as_str()]),
            occupied_table: first_table.clone(),
            service: "s3",
        },
        Row {
            case: "a create naming a clean table first and the occupied one second",
            action: "CreateVpcEndpoint",
            params: gateway_request(&vpc_id, "s3", &[clean_table.as_str(), second_table.as_str()]),
            occupied_table: second_table.clone(),
            service: "s3",
        },
        Row {
            case: "another service's prefix list on the table that routes it",
            action: "CreateVpcEndpoint",
            params: gateway_request(&vpc_id, "dynamodb", &[second_table.as_str()]),
            occupied_table: second_table.clone(),
            service: "dynamodb",
        },
        Row {
            case: "a modify adding the table the endpoint's own service already reaches",
            action: "ModifyVpcEndpoint",
            params: modify_request(&movable_endpoint, &[first_table.as_str()]),
            occupied_table: first_table.clone(),
            service: "s3",
        },
        Row {
            case: "a modify naming a clean table first and the occupied one second",
            action: "ModifyVpcEndpoint",
            params: modify_request(
                &movable_endpoint,
                &[clean_table.as_str(), second_table.as_str()],
            ),
            occupied_table: second_table.clone(),
            service: "s3",
        },
    ];

    let held_endpoints = endpoint_ids(setup, &router).await;
    for row in rows {
        let case = row.case;
        let prefix_list_id = published_prefix_list_id(case, &router, &service_name(row.service)).await;
        let (status, body) = call(&router, row.action, &row.params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}",
            action = row.action
        );
        assert_eq!(
            error_code(&body),
            ec2::state::ROUTE_ALREADY_EXISTS,
            "case {case:?}: expected {expected}, got {body}",
            expected = ec2::state::ROUTE_ALREADY_EXISTS
        );
        let expected = format!(
            "route table {table} already has a route with destination-prefix-list-id \
             {prefix_list_id}",
            table = row.occupied_table,
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // A refused request stores nothing: no endpoint appears, and every endpoint keeps the
        // tables it already reached.
        assert_eq!(
            endpoint_ids(case, &router).await,
            held_endpoints,
            "case {case:?}: a refused request must create no endpoint"
        );
    }

    // The endpoints the setup created still reach exactly the tables they were created with:
    // the refusals above moved nothing, the partially-named modify rows included.
    for (endpoint_id, expected) in [
        (&s3_endpoint, vec![first_table.clone(), second_table.clone()]),
        (&dynamodb_endpoint, vec![second_table.clone()]),
        (&movable_endpoint, vec![third_table.clone()]),
    ] {
        let described = described_tables(setup, &router, endpoint_id).await;
        assert_eq!(
            described, expected,
            "the refusals must leave {endpoint_id} reaching {expected:?}, got {described:?}"
        );
    }
}

/// A VPC with `cidr`, as the id EC2 minted.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr)]).await;
    member(case, &body, "vpcId")
}
