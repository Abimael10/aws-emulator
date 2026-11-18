// SPDX-License-Identifier: CC0-1.0

//! `DeleteRoute` addressed by its DESTINATION, in whichever valid spelling a request writes
//! that destination in, driven over the wire the service speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=DeleteRoute`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a route is addressed inside its table by the ONE destination it
//! matches on, compared in the canonical form the create stored it under, so a delete naming
//! that destination in any valid spelling takes exactly that route out of exactly that table
//! and leaves every other route of it where it was** — "deletes the specified route from the
//! specified route table", on a destination whose stored form is canonical ("we modify the
//! specified CIDR block to its canonical form", `CreateRouteRequest$DestinationCidrBlock`).
//! Its refusal half follows from the same reading: a destination the table holds no route for
//! is `InvalidRoute.NotFound`, named by the destination FILTER the route would be selected by
//! and the table it is not in, and a destination that is no CIDR block at all is
//! `InvalidParameterValue`.
//!
//! The rows differ along what the rule generalises over: which member carries the destination
//! (IPv4, IPv6, a prefix list), how the request SPELLS the same block (host bits set or clear,
//! an IPv6 address expanded or compressed, upper or lower case), the prefix length — each
//! published bound and one past it — how many routes the table holds beside the one being
//! taken out, and whether the route's target is still available or has gone to `blackhole`.
//!
//! [`super::delete_route`] covers the one-route tables and the parameter-combination refusals;
//! this suite covers the spellings, the prefix-length bounds and the many-route tables.

use ec2::state::{LOCAL_ROUTE_GATEWAY_ID, ROUTE_STATE_ACTIVE, ROUTE_STATE_BLACKHOLE};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// Amazon EC2's code for a route the named table does not hold ("InvalidRoute.NotFound — the
/// specified route does not exist", EC2 API "Error codes").
const INVALID_ROUTE_NOT_FOUND: &str = "InvalidRoute.NotFound";

/// Amazon EC2's code for a parameter whose value the action cannot read at all (EC2 API
/// "Error codes": `InvalidParameterValue`).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The block the VPC every table here belongs to is created with.
const VPC_CIDR: &str = "10.90.0.0/16";

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

/// One route as `DescribeRouteTables` reports it: the destination it matches on — whichever of
/// the three members carries it — the gateway it points at, and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedRoute {
    destination: String,
    gateway_id: String,
    state: String,
}

/// The routes the one table `route_table_id` names holds, read back through
/// `DescribeRouteTables` so every expectation is about the state the account really holds.
async fn routes_of(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Vec<ReportedRoute> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .flat_map(|table| set_items(table, "routeSet"))
        .map(|route| ReportedRoute {
            destination: child_text(route, "destinationCidrBlock")
                .or_else(|| child_text(route, "destinationIpv6CidrBlock"))
                .or_else(|| child_text(route, "destinationPrefixListId"))
                .unwrap_or_default(),
            gateway_id: child_text(route, "gatewayId").unwrap_or_default(),
            state: child_text(route, "state").unwrap_or_default(),
        })
        .collect()
}

/// The destinations the table reports a route for, in the order it reports them.
async fn destinations_of(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Vec<String> {
    routes_of(case, router, route_table_id)
        .await
        .into_iter()
        .map(|route| route.destination)
        .collect()
}

/// Assert the table still carries the local route its VPC came with, reachable.
fn assert_local_route(case: &str, reported: &[ReportedRoute]) {
    assert!(
        reported.iter().any(|route| route.gateway_id == LOCAL_ROUTE_GATEWAY_ID
            && route.state == ROUTE_STATE_ACTIVE),
        "case {case:?}: expected the table to keep the active local route of its VPC, got \
         {reported:?}"
    );
}

/// The VPC and the internet gateway attached to it: the target every route here points at.
async fn estate(router: &MultiServiceRouter) -> (String, String) {
    let case = "the estate the routes are in";
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
    (vpc_id, internet_gateway_id)
}

/// A route table of `vpc_id`, as its id.
async fn route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    member(case, &body, "routeTableId")
}

/// Put one route into `route_table_id` for the destination `member` carries, pointing at
/// `internet_gateway_id`.
async fn create_route(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    destination_member: &str,
    destination: &str,
    internet_gateway_id: &str,
) {
    ok(
        case,
        router,
        "CreateRoute",
        &[
            param("RouteTableId", route_table_id),
            param(destination_member, destination),
            param("GatewayId", internet_gateway_id),
        ],
    )
    .await;
}

/// One row of the spelling table: the member that carries the destination, the spelling the
/// CREATE wrote, a different valid spelling of the SAME block the delete writes, the canonical
/// form the table stores and reports it under, and the filter a refusal names it by.
struct Spelling {
    case: &'static str,
    member: &'static str,
    created_with: &'static str,
    deleted_with: &'static str,
    stored: &'static str,
    filter: &'static str,
}

/// Source: `DeleteRoute`'s own destination members
/// (`smithy_query(query="operation",operation="DeleteRoute")`: `DestinationCidrBlock`,
/// `DestinationIpv6CidrBlock`, `DestinationPrefixListId`), the canonical form the create
/// documents for a block (`CreateRouteRequest$DestinationCidrBlock`), and the destination
/// filter names a route is selected by (`DescribeRouteTables`: `route.destination-cidr-block`,
/// `route.destination-ipv6-cidr-block`). The prefix lengths span each published bound — an
/// IPv4 prefix is 0..=32 bits and an IPv6 one 0..=128, the widths of the two address families,
/// the members themselves publishing no block size of their own.
const SPELLINGS: &[Spelling] = &[
    Spelling {
        case: "an IPv4 block created with host bits set, deleted by its canonical form",
        member: "DestinationCidrBlock",
        created_with: "198.51.100.77/26",
        deleted_with: "198.51.100.64/26",
        stored: "198.51.100.64/26",
        filter: "destination-cidr-block",
    },
    Spelling {
        case: "an IPv4 block created canonical, deleted with host bits set",
        member: "DestinationCidrBlock",
        created_with: "192.0.2.0/24",
        deleted_with: "192.0.2.123/24",
        stored: "192.0.2.0/24",
        filter: "destination-cidr-block",
    },
    Spelling {
        case: "the shortest IPv4 prefix, deleted by another address of the same block",
        member: "DestinationCidrBlock",
        created_with: "0.0.0.0/0",
        deleted_with: "203.0.113.9/0",
        stored: "0.0.0.0/0",
        filter: "destination-cidr-block",
    },
    Spelling {
        case: "an IPv6 block created expanded and upper case, deleted compressed",
        member: "DestinationIpv6CidrBlock",
        created_with: "2001:0DB8:ABCD:0000:0000:0000:0000:0000/48",
        deleted_with: "2001:db8:abcd::/48",
        stored: "2001:db8:abcd::/48",
        filter: "destination-ipv6-cidr-block",
    },
    Spelling {
        case: "an IPv6 block created canonical, deleted with host bits set",
        member: "DestinationIpv6CidrBlock",
        created_with: "2001:db8:1234::/64",
        deleted_with: "2001:DB8:1234:0:FFFF:FFFF::/64",
        stored: "2001:db8:1234::/64",
        filter: "destination-ipv6-cidr-block",
    },
    Spelling {
        case: "the shortest IPv6 prefix, deleted by another address of the same block",
        member: "DestinationIpv6CidrBlock",
        created_with: "::/0",
        deleted_with: "2001:db8::1/0",
        stored: "::/0",
        filter: "destination-ipv6-cidr-block",
    },
    Spelling {
        case: "the longest IPv6 prefix, a single address written expanded",
        member: "DestinationIpv6CidrBlock",
        created_with: "2001:db8::beef/128",
        deleted_with: "2001:0db8:0000:0000:0000:0000:0000:beef/128",
        stored: "2001:db8::beef/128",
        filter: "destination-ipv6-cidr-block",
    },
];

/// A delete addresses the route whose destination it names, whichever valid spelling of that
/// destination the request writes — and the destination is free to carry a route again
/// afterwards, since what the delete took out is the route and not the destination.
#[tokio::test]
async fn a_delete_addresses_its_destination_in_any_valid_spelling() {
    let router = emulator();
    let (vpc_id, internet_gateway_id) = estate(&router).await;

    for row in SPELLINGS {
        let case = row.case;
        let table = route_table(case, &router, &vpc_id).await;
        create_route(
            case,
            &router,
            &table,
            row.member,
            row.created_with,
            &internet_gateway_id,
        )
        .await;
        let reported = destinations_of(case, &router, &table).await;
        assert!(
            reported.contains(&row.stored.to_string()),
            "case {case:?}: expected the create to store {spelling} as {stored}, got {reported:?}",
            spelling = row.created_with,
            stored = row.stored
        );

        let (status, body) = call(
            &router,
            "DeleteRoute",
            &[
                param("RouteTableId", &table),
                param(row.member, row.deleted_with),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting {spelling}, got {status} with body {body}",
            spelling = row.deleted_with
        );

        let left = routes_of(case, &router, &table).await;
        assert!(
            !left.iter().any(|route| route.destination == row.stored),
            "case {case:?}: expected the route for {stored} to be out of the table, got {left:?}",
            stored = row.stored
        );
        assert_local_route(case, &left);

        // The same request a second time addresses no route of the table, and says so by the
        // destination filter and the table it is not in.
        let (status, again) = call(
            &router,
            "DeleteRoute",
            &[
                param("RouteTableId", &table),
                param(row.member, row.deleted_with),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the second delete to be refused, got {status} with body \
             {again}"
        );
        assert_eq!(
            error_code(&again),
            INVALID_ROUTE_NOT_FOUND,
            "case {case:?}: expected {INVALID_ROUTE_NOT_FOUND}, got {again}"
        );
        let message = error_message(&again);
        assert!(
            message.contains(row.filter) && message.contains(&table),
            "case {case:?}: expected the refusal to name {filter} and {table}, got {message:?}",
            filter = row.filter
        );

        // The destination takes a route again: the delete removed one route, not the right to
        // route that destination.
        create_route(
            case,
            &router,
            &table,
            row.member,
            row.created_with,
            &internet_gateway_id,
        )
        .await;
        let reported = destinations_of(case, &router, &table).await;
        assert!(
            reported.contains(&row.stored.to_string()),
            "case {case:?}: expected the destination {stored} to take a route again, got \
             {reported:?}",
            stored = row.stored
        );
    }
}

/// The refusal for a destination the table holds no route for, worded the way Amazon EC2 words
/// this error class: the destination filter, the destination itself and the table it is not in.
#[tokio::test]
async fn a_delete_of_a_destination_the_table_does_not_hold_names_it_and_the_table() {
    let router = emulator();
    let case = "a table holding only the local route of its VPC";
    let (vpc_id, _internet_gateway_id) = estate(&router).await;
    let table = route_table(case, &router, &vpc_id).await;

    // (case, the member the destination is written in, the destination, the filter it is
    // reported by)
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "an IPv4 destination",
            "DestinationCidrBlock",
            "192.0.2.0/24",
            "destination-cidr-block",
        ),
        (
            "an IPv6 destination",
            "DestinationIpv6CidrBlock",
            "2001:db8::/64",
            "destination-ipv6-cidr-block",
        ),
    ];

    for (case, destination_member, destination, filter) in rows {
        let (status, body) = call(
            &router,
            "DeleteRoute",
            &[
                param("RouteTableId", &table),
                param(destination_member, destination),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the delete to be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_ROUTE_NOT_FOUND,
            "case {case:?}: expected {INVALID_ROUTE_NOT_FOUND}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("no route with {filter} {destination} in route table {table}"),
            "case {case:?}: expected the sentence EC2 refuses a missing route with, got {body}"
        );
    }
}

/// A destination whose prefix length is past the width of its address family, or that is no
/// CIDR block at all, is a parameter the action cannot read; each published prefix length is
/// readable and so answers the missing-route refusal instead. Nothing is taken out either way.
#[tokio::test]
async fn a_delete_reads_every_published_prefix_length_and_refuses_what_is_no_block() {
    let router = emulator();
    let case = "the table the refusals are read against";
    let (vpc_id, internet_gateway_id) = estate(&router).await;
    let table = route_table(case, &router, &vpc_id).await;
    // The one route the table holds beside its local one, at a destination no row below
    // addresses: what the "nothing was taken out" assertion at the end reads.
    create_route(
        case,
        &router,
        &table,
        "DestinationCidrBlock",
        "203.0.113.0/24",
        &internet_gateway_id,
    )
    .await;
    let held = destinations_of(case, &router, &table).await;

    // (case, member, value, the code the refusal carries, what its message must name). An
    // IPv4 prefix length runs 0..=32 and an IPv6 one 0..=128 — the widths of the two address
    // families — so each bound is read, and one past each bound is no block at all. A block
    // the action READ is named in its canonical form, the form a route of it would be stored
    // under; a value it could not read is echoed as the request wrote it.
    let rows: [(&str, &str, &str, &str, &str); 12] = [
        (
            "the shortest IPv4 prefix",
            "DestinationCidrBlock",
            "192.0.2.0/0",
            INVALID_ROUTE_NOT_FOUND,
            "0.0.0.0/0",
        ),
        (
            "the longest IPv4 prefix",
            "DestinationCidrBlock",
            "192.0.2.7/32",
            INVALID_ROUTE_NOT_FOUND,
            "192.0.2.7/32",
        ),
        (
            "one past the longest IPv4 prefix",
            "DestinationCidrBlock",
            "192.0.2.0/33",
            INVALID_PARAMETER_VALUE,
            "192.0.2.0/33",
        ),
        (
            "an IPv4 block with no prefix length at all",
            "DestinationCidrBlock",
            "192.0.2.0",
            INVALID_PARAMETER_VALUE,
            "192.0.2.0",
        ),
        (
            "an IPv4 prefix length that is not a number",
            "DestinationCidrBlock",
            "192.0.2.0/twenty-four",
            INVALID_PARAMETER_VALUE,
            "192.0.2.0/twenty-four",
        ),
        (
            "the shortest IPv6 prefix",
            "DestinationIpv6CidrBlock",
            "2001:db8::/0",
            INVALID_ROUTE_NOT_FOUND,
            "::/0",
        ),
        (
            "the longest IPv6 prefix",
            "DestinationIpv6CidrBlock",
            "2001:db8::7/128",
            INVALID_ROUTE_NOT_FOUND,
            "2001:db8::7/128",
        ),
        (
            "one past the longest IPv6 prefix",
            "DestinationIpv6CidrBlock",
            "2001:db8::/129",
            INVALID_PARAMETER_VALUE,
            "2001:db8::/129",
        ),
        (
            "an IPv6 address that is not one",
            "DestinationIpv6CidrBlock",
            "2001:db8:::/64",
            INVALID_PARAMETER_VALUE,
            "2001:db8:::/64",
        ),
        (
            "an IPv4 block written in the IPv6 member",
            "DestinationIpv6CidrBlock",
            "192.0.2.0/24",
            INVALID_PARAMETER_VALUE,
            "192.0.2.0/24",
        ),
        // A refused value is ECHOED into the XML refusal, so a value carrying the characters
        // that are significant in XML is the row that says whether the envelope survives it:
        // the body still parses and the value comes back whole.
        (
            "an IPv4 destination carrying the characters XML marks up with",
            "DestinationCidrBlock",
            "192.0.2.0/24<&\"]]>",
            INVALID_PARAMETER_VALUE,
            "192.0.2.0/24<&\"]]>",
        ),
        (
            "an IPv6 destination carrying the characters XML marks up with",
            "DestinationIpv6CidrBlock",
            "2001:db8::/64<&\"]]>",
            INVALID_PARAMETER_VALUE,
            "2001:db8::/64<&\"]]>",
        ),
    ];

    for (case, destination_member, destination, expected, named) in rows {
        let (status, body) = call(
            &router,
            "DeleteRoute",
            &[
                param("RouteTableId", &table),
                param(destination_member, destination),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {destination} to be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected} for {destination}, got {body}"
        );
        // The refusal is a well-formed XML envelope whatever the value it echoes: a value the
        // service wrote through unescaped would leave a body no client could parse.
        let parsed = document(case, &body);
        assert_eq!(
            parsed.root_element().tag_name().name(),
            "Response",
            "case {case:?}: expected EC2's error envelope, got {body}"
        );
        assert!(
            error_message(&body).contains(named),
            "case {case:?}: expected the refusal for {destination} to name {named}, got {body}"
        );
    }

    // A refused delete takes nothing out: the table holds exactly what it held before.
    let left = destinations_of(case, &router, &table).await;
    assert_eq!(
        left, held,
        "case {case:?}: expected the refused deletes to leave the table's routes alone"
    );
}

/// A table holding several routes loses exactly the one each delete names, whichever member
/// carries its destination — and a route whose target is no longer available (`blackhole`) is
/// taken out the same way a reachable one is, since what addresses a route is its destination.
#[tokio::test]
async fn a_delete_takes_out_one_route_of_many_whatever_state_its_target_is_in() {
    let router = emulator();
    let case = "a table holding four routes to one gateway";
    let (vpc_id, internet_gateway_id) = estate(&router).await;
    let table = route_table(case, &router, &vpc_id).await;

    // The prefix list the fourth route matches on: a destination list this account holds.
    let list = ok(
        case,
        &router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", "a-prefix-list"),
            param("MaxEntries", "5"),
            param("AddressFamily", "IPv4"),
            param("Entry.1.Cidr", "192.0.2.0/24"),
        ],
    )
    .await;
    let prefix_list_id = member(case, &list, "prefixListId");

    // (the member the destination is written in, the spelling the create writes, the canonical
    // form the table reports). The rows span the three destination members a route may match
    // on and both spellings of a block.
    let seeded: Vec<(&str, String, String)> = vec![
        (
            "DestinationCidrBlock",
            "0.0.0.0/0".to_string(),
            "0.0.0.0/0".to_string(),
        ),
        (
            "DestinationCidrBlock",
            "198.51.100.77/26".to_string(),
            "198.51.100.64/26".to_string(),
        ),
        (
            "DestinationIpv6CidrBlock",
            "::/0".to_string(),
            "::/0".to_string(),
        ),
        (
            "DestinationPrefixListId",
            prefix_list_id.clone(),
            prefix_list_id.clone(),
        ),
    ];
    for (destination_member, requested, _) in &seeded {
        create_route(
            case,
            &router,
            &table,
            destination_member,
            requested,
            &internet_gateway_id,
        )
        .await;
    }

    // The gateway every one of those routes points at goes away as a target: "the `blackhole`
    // state indicates that the route's target isn't available" (`Route$State`), which is the
    // state a delete has to work in just as well as in `active`.
    ok(
        case,
        &router,
        "DetachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let reported = routes_of(case, &router, &table).await;
    for (_, _, stored) in &seeded {
        let route = reported
            .iter()
            .find(|route| &route.destination == stored)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a route for {stored} in {reported:?}")
            });
        assert_eq!(
            route.state, ROUTE_STATE_BLACKHOLE,
            "case {case:?}: expected the route for {stored} to report its target unavailable, \
             got {route:?}"
        );
    }
    assert_local_route(case, &reported);

    // One delete per route: each takes out the destination it names and leaves the ones not
    // yet named, in the state they were in.
    let mut outstanding: Vec<String> = seeded
        .iter()
        .map(|(_, _, stored)| stored.clone())
        .collect();
    for (destination_member, requested, stored) in &seeded {
        ok(
            case,
            &router,
            "DeleteRoute",
            &[
                param("RouteTableId", &table),
                param(destination_member, requested),
            ],
        )
        .await;
        outstanding.retain(|destination| destination != stored);

        let left = routes_of(case, &router, &table).await;
        assert!(
            !left.iter().any(|route| &route.destination == stored),
            "case {case:?}: expected the route for {stored} to be out of the table, got {left:?}"
        );
        for destination in &outstanding {
            let route = left
                .iter()
                .find(|route| &route.destination == destination)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the route for {destination} to survive the \
                         delete of {stored}, got {left:?}"
                    )
                });
            assert_eq!(
                route.state, ROUTE_STATE_BLACKHOLE,
                "case {case:?}: expected the surviving route for {destination} left as it was, \
                 got {route:?}"
            );
        }
        assert_local_route(case, &left);
    }

    // What is left is the local route the table came with, and nothing else: every route a
    // caller added has been taken out one delete at a time.
    let left = routes_of(case, &router, &table).await;
    assert_eq!(
        left.len(),
        1,
        "case {case:?}: expected only the local route to be left, got {left:?}"
    );
    assert_local_route(case, &left);
}
