// SPDX-License-Identifier: CC0-1.0

//! The refusal a GATEWAY VPC-endpoint association gets when a route table it names ALREADY
//! routes the endpoint service's managed prefix list, driven over `ec2Query` (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: associating a gateway VPC endpoint with a route table installs one
//! route per table, destined for the endpoint service's managed prefix list, so a request
//! naming a table that already carries a route for that destination is refused whole — HTTP
//! 400, EC2's duplicate-route code, and a sentence that names the OCCUPIED route table
//! beside the `destination-prefix-list-id` the route is keyed by — and every other table the
//! same request named is left exactly as it was. Both calls that install such an association
//! answer it the same way: the create's `RouteTableIds` and the modify's `AddRouteTableIds`.
//!
//! The rows differ along what the rule generalises over: which endpoint service (so which
//! prefix list is the destination), which Region the call is signed for, how many tables the
//! request names, WHERE in that list the occupied table sits — alone, first, last, between
//! clean ones — and which of the two operations installs the association. The prefix-list id
//! the sentence has to carry is read off `DescribePrefixLists` rather than written out here,
//! so nothing below is tied to one run's generated values.
//!
//! Siblings: [`super::create_vpc_endpoint_route_table_associations`] covers the association
//! set a create serves and the rest of its refusal table,
//! [`super::modify_vpc_endpoint`] the tables added to and removed from an endpoint that
//! already exists.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{child_text, param, Param};

/// The Region most calls here are signed for.
const REGION: &str = "us-east-1";

/// A second Region, so nothing asserted below is tied to one Region's catalogue.
const OTHER_REGION: &str = "eu-west-1";

/// The block the VPC the endpoints are created in is given.
const VPC_CIDR: &str = "10.71.0.0/16";

/// The status a scaffold answers for a request shape nobody has implemented — the status no
/// row below may see, since both associations are served.
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

/// A VPC with [`VPC_CIDR`], as its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, region: &str) -> String {
    let body = ok(
        case,
        router,
        region,
        "CreateVpc",
        &[param("CidrBlock", VPC_CIDR)],
    )
    .await;
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

/// The prefix-list destinations `DescribeRouteTables` reports for the one table
/// `route_table_id` — every route's `destinationPrefixListId`, which is what a conflicting
/// association would have added one more of.
async fn prefix_list_destinations(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    route_table_id: &str,
) -> Vec<String> {
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
                .filter_map(|route| child_text(route, "destinationPrefixListId"))
                .collect()
        })
        .unwrap_or_default()
}

/// Every VPC endpoint the account holds, as `DescribeVpcEndpoints` reports them.
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
    element(&body, "routeTableIdSet")
        .map(|set| elements(&set, "item"))
        .unwrap_or_default()
}

/// Which call installs the association the row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installer {
    /// `CreateVpcEndpoint`'s `RouteTableIds`.
    Create,
    /// `ModifyVpcEndpoint`'s `AddRouteTableIds`, on an endpoint created with no table.
    Modify,
}

/// One row: a VPC holding `tables` route tables, one of which an endpoint to the row's
/// service already reaches, and a second association naming the tables at `named`.
struct ConflictCase {
    case: &'static str,
    /// The Region the call is signed for, and the Region of the service it names.
    region: &'static str,
    /// The endpoint service, as the last label of `com.amazonaws.<region>.<label>` — one of
    /// the two services a Region publishes a gateway prefix list for.
    service_label: &'static str,
    /// How many route tables the row's VPC holds.
    tables: usize,
    /// The position of the table the first endpoint already reaches the service through.
    occupied: usize,
    /// Which tables the second association names, by position and in request order.
    named: &'static [usize],
    /// The call that names them.
    installer: Installer,
}

/// The rows: the occupied table alone, first, last and in the middle of a longer list, both
/// gateway services, a second Region, and both operations that install an association.
fn conflict_cases() -> Vec<ConflictCase> {
    vec![
        ConflictCase {
            case: "the occupied table is the only one the create names",
            region: REGION,
            service_label: "s3",
            tables: 2,
            occupied: 0,
            named: &[0],
            installer: Installer::Create,
        },
        ConflictCase {
            case: "the occupied table first, a clean one after it",
            region: REGION,
            service_label: "s3",
            tables: 2,
            occupied: 0,
            named: &[0, 1],
            installer: Installer::Create,
        },
        ConflictCase {
            case: "a clean table first, the occupied one last",
            region: REGION,
            service_label: "dynamodb",
            tables: 3,
            occupied: 2,
            named: &[1, 2],
            installer: Installer::Create,
        },
        ConflictCase {
            case: "the occupied table between two clean ones",
            region: REGION,
            service_label: "s3",
            tables: 4,
            occupied: 2,
            named: &[0, 2, 3],
            installer: Installer::Create,
        },
        ConflictCase {
            case: "the occupied table in another Region's catalogue",
            region: OTHER_REGION,
            service_label: "s3",
            tables: 2,
            occupied: 1,
            named: &[1, 0],
            installer: Installer::Create,
        },
        ConflictCase {
            case: "the occupied table added to an endpoint that already exists",
            region: REGION,
            service_label: "dynamodb",
            tables: 2,
            occupied: 0,
            named: &[0],
            installer: Installer::Modify,
        },
        ConflictCase {
            case: "a clean table and the occupied one added together",
            region: REGION,
            service_label: "s3",
            tables: 3,
            occupied: 1,
            named: &[2, 1],
            installer: Installer::Modify,
        },
    ]
}

/// Every row is refused the same way: HTTP 400 with EC2's duplicate-route code and the
/// sentence naming the occupied route table and the service's prefix-list id, the clean
/// tables of the same request untouched, no endpoint association written — and the same
/// request naming only the clean tables is then served.
#[tokio::test]
async fn a_route_table_that_already_routes_the_services_prefix_list_is_refused_by_name() {
    for row in conflict_cases() {
        let case = row.case;
        let region = row.region;
        // One emulator per row, so the tables a row names and the routes they carry are its
        // own.
        let router = emulator();
        let service_name = format!("com.amazonaws.{region}.{label}", label = row.service_label);
        let vpc_id = create_vpc(case, &router, region).await;
        let mut tables = Vec::new();
        for _ in 0..row.tables {
            tables.push(create_route_table(case, &router, region, &vpc_id).await);
        }
        let occupied = tables[row.occupied].clone();
        let prefix_list_id = published_prefix_list_id(case, &router, region, &service_name).await;

        // The endpoint that puts the service's prefix-list route into the occupied table.
        ok(
            case,
            &router,
            region,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &vpc_id),
                param("ServiceName", &service_name),
                param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
                param("RouteTableId.1", &occupied),
            ],
        )
        .await;

        // The endpoint the modify rows add tables to: created with no table at all, so the
        // conflict is the one the ADD finds rather than one the create wrote itself.
        let subject = match row.installer {
            Installer::Create => None,
            Installer::Modify => {
                let body = ok(
                    case,
                    &router,
                    region,
                    "CreateVpcEndpoint",
                    &[
                        param("VpcId", &vpc_id),
                        param("ServiceName", &service_name),
                        param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
                    ],
                )
                .await;
                Some(member(case, &body, "vpcEndpointId"))
            }
        };

        let named: Vec<&str> = row
            .named
            .iter()
            .map(|position| tables[*position].as_str())
            .collect();
        let clean: Vec<&str> = named
            .iter()
            .copied()
            .filter(|table| *table != occupied.as_str())
            .collect();

        let held_endpoints = endpoint_ids(case, &router, region).await;
        let (action, params) = match subject.as_deref() {
            None => {
                let mut params = vec![
                    param("VpcId", &vpc_id),
                    param("ServiceName", &service_name),
                    param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
                ];
                params.extend(indexed("RouteTableId", &named));
                ("CreateVpcEndpoint", params)
            }
            Some(endpoint_id) => {
                let mut params = vec![param("VpcEndpointId", endpoint_id)];
                params.extend(indexed("AddRouteTableId", &named));
                ("ModifyVpcEndpoint", params)
            }
        };

        let (status, body) = call(&router, region, action, &params).await;
        assert_ne!(
            status, NOT_IMPLEMENTED,
            "case {case:?}: {action} naming an occupied route table is refused with the code \
             for its cause rather than as unimplemented, got {status} with body {body}"
        );
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
        );
        let answered_code = error_code(&body);
        assert_eq!(
            answered_code,
            ec2::state::ROUTE_ALREADY_EXISTS,
            "case {case:?}: expected the duplicate-route code, got {answered_code:?} in {body}"
        );
        let expected_message = format!(
            "route table {occupied} already has a route with destination-prefix-list-id \
             {prefix_list_id}"
        );
        let answered_message = error_message(&body);
        assert_eq!(
            answered_message, expected_message,
            "case {case:?}: expected the refusal {expected_message:?}, got \
             {answered_message:?}"
        );

        // The WHOLE call is refused: every clean table the same request named still routes
        // nothing through a prefix list, and the endpoint set is the one that was there.
        for clean_table in &clean {
            let destinations =
                prefix_list_destinations(case, &router, region, clean_table).await;
            assert!(
                destinations.is_empty(),
                "case {case:?}: expected the clean table {clean_table} to be left unmodified, \
                 got the prefix-list routes {destinations:?}"
            );
        }
        let after = endpoint_ids(case, &router, region).await;
        assert_eq!(
            after, held_endpoints,
            "case {case:?}: expected the refusal to store no endpoint, held {held_endpoints:?} \
             before and {after:?} after"
        );
        if let Some(endpoint_id) = subject.as_deref() {
            let associated = described_tables(case, &router, region, endpoint_id).await;
            assert!(
                associated.is_empty(),
                "case {case:?}: expected the refused add to leave {endpoint_id} associated with \
                 no table, got {associated:?}"
            );
        }

        // The same request naming only the clean tables is served, which is what makes the
        // refusal above about the occupied table rather than about the request's shape.
        if !clean.is_empty() {
            let (accepted_status, accepted_body) = match subject.as_deref() {
                None => {
                    let mut params = vec![
                        param("VpcId", &vpc_id),
                        param("ServiceName", &service_name),
                        param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
                    ];
                    params.extend(indexed("RouteTableId", &clean));
                    call(&router, region, "CreateVpcEndpoint", &params).await
                }
                Some(endpoint_id) => {
                    let mut params = vec![param("VpcEndpointId", endpoint_id)];
                    params.extend(indexed("AddRouteTableId", &clean));
                    call(&router, region, "ModifyVpcEndpoint", &params).await
                }
            };
            assert_eq!(
                accepted_status, 200,
                "case {case:?}: expected the clean tables {clean:?} to be served, got \
                 {accepted_status} with body {accepted_body}"
            );
            for clean_table in &clean {
                let destinations =
                    prefix_list_destinations(case, &router, region, clean_table).await;
                assert_eq!(
                    destinations,
                    vec![prefix_list_id.clone()],
                    "case {case:?}: expected {clean_table} to route {prefix_list_id} once it \
                     was associated, got {destinations:?}"
                );
            }
        }
    }
}

/// The conflict is about the DESTINATION, not about the table: a table that already routes
/// one gateway service's prefix list takes the other service's route without complaint, and
/// a second endpoint to the SAME service is refused naming that same table.
#[tokio::test]
async fn only_the_services_own_prefix_list_makes_a_table_occupied() {
    let case = "two gateway services through one route table";
    let router = emulator();
    let vpc_id = create_vpc(case, &router, REGION).await;
    let route_table = create_route_table(case, &router, REGION, &vpc_id).await;

    // Both services a Region publishes a gateway prefix list for, each with the id
    // `DescribePrefixLists` reports for it.
    let first_service = format!("com.amazonaws.{REGION}.s3");
    let second_service = format!("com.amazonaws.{REGION}.dynamodb");
    let first_list = published_prefix_list_id(case, &router, REGION, &first_service).await;
    let second_list = published_prefix_list_id(case, &router, REGION, &second_service).await;
    assert_ne!(
        first_list, second_list,
        "case {case:?}: the two gateway services must publish different prefix lists, both \
         reported {first_list}"
    );

    for service_name in [&first_service, &second_service] {
        ok(
            case,
            &router,
            REGION,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &vpc_id),
                param("ServiceName", service_name),
                param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
                param("RouteTableId.1", &route_table),
            ],
        )
        .await;
    }

    let mut destinations = prefix_list_destinations(case, &router, REGION, &route_table).await;
    destinations.sort();
    let mut expected = vec![first_list.clone(), second_list.clone()];
    expected.sort();
    assert_eq!(
        destinations, expected,
        "case {case:?}: expected the table to route both services' prefix lists, got \
         {destinations:?}"
    );

    // A THIRD endpoint, to a service the table already routes, is the conflict — named by
    // table and by that service's own prefix list.
    let (status, body) = call(
        &router,
        REGION,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", &second_service),
            param("VpcEndpointType", ec2::state::VPC_ENDPOINT_TYPE_GATEWAY),
            param("RouteTableId.1", &route_table),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a table that already routes {second_list}, got \
         {status} with body {body}"
    );
    let expected_message = format!(
        "route table {route_table} already has a route with destination-prefix-list-id \
         {second_list}"
    );
    let answered_message = error_message(&body);
    assert_eq!(
        answered_message, expected_message,
        "case {case:?}: expected the refusal {expected_message:?}, got {answered_message:?}"
    );
}
