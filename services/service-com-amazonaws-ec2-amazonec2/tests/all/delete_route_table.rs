// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteRouteTable`, driven over the wire the service actually speaks:
//! `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=DeleteRouteTable`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a delete takes the ONE route table its `RouteTableId` names, and the
//! routes that live in it, out of the store, and only when nothing uses it any more** —
//! "deletes the specified route table. You must disassociate the route table from any subnets
//! before you can delete it. You can't delete the main route table" (`DeleteRouteTable`). So:
//!
//! 1. A successful delete leaves the named table resolving to nothing — the account's reader
//!    answers that table's own `InvalidRouteTableID.NotFound` for it — takes its routes with
//!    it, so a route filter no longer selects it, and leaves every other table of the VPC,
//!    with its own routes, associations and tags, exactly as the reader reported it before.
//! 2. A delete naming a table the account holds no record of is refused with
//!    `InvalidRouteTableID.NotFound`, which is also what a SECOND delete of a table gets: a
//!    caller that deletes twice can tell the deletion it performed from one it did not.
//! 3. A delete of a table something still uses — the VPC's main table, or a table a subnet is
//!    explicitly associated with — is refused with `DependencyViolation`, naming the table the
//!    request named, and removes nothing.
//! 4. A `DryRun` of a delete that WOULD have succeeded answers `DryRunOperation` and removes
//!    nothing either, since the flag asks whether the action would succeed rather than for it
//!    (`DeleteRouteTableRequest$DryRun`).
//!
//! Every table row is a distinct input along what that rule generalises over — how many routes
//! the deleted table carried, how many other tables the VPC holds, how many subnets still use
//! the table, which form an unresolvable id takes, and what the table carries when a dry run
//! probes it — so no single recorded request is the specification.
//!
//! `super::vpc_internet_path` drives this same delete as the LAST step of building and taking
//! apart a VPC's path to the internet, where it is one call among many; what this suite adds is
//! the refusals' own wording, that the deleted table's ROUTES go with it while another table's
//! identical routes stay, the forms an unresolvable id takes, and the dry-run probe.

use std::collections::BTreeSet;

use super::ec2_query::{elements, error_code, error_message, filters, tag_specification};
use super::wire::{child_text, document, param, set_items, Param};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// EC2's code for a request that named a route table id this account holds no table for
/// (EC2 API "Error codes": `InvalidRouteTableID.NotFound`).
const INVALID_ROUTE_TABLE_ID_NOT_FOUND: &str = "InvalidRouteTableID.NotFound";

/// EC2's code for a delete something still depends on (EC2 API "Error codes":
/// `DependencyViolation`).
const DEPENDENCY_VIOLATION: &str = "DependencyViolation";

/// EC2's code for a request that carried `DryRun` and would have succeeded (EC2 API "Error
/// codes": `DryRunOperation` — "the user has the required permissions, so the request would
/// have succeeded, but the `DryRun` parameter was used").
const DRY_RUN_OPERATION: &str = "DryRunOperation";

/// The HTTP status that refusal carries, spelled out here rather than taken from the service
/// so a wrong status is caught: EC2 answers a dry run `412 Precondition Failed`.
const DRY_RUN_OPERATION_STATUS: u16 = 412;

/// The sentence that refusal carries, as EC2 words it for this error class.
const DRY_RUN_OPERATION_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it. The store is
/// reached only through the wire here, so the engine handle is not needed.
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
    super::ec2_query::element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A VPC with `cidr_block`, as its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    member(case, &body, "vpcId")
}

/// A subnet of `vpc_id` with `cidr_block`, as its id.
async fn create_subnet(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", vpc_id), param("CidrBlock", cidr_block)],
    )
    .await;
    member(case, &body, "subnetId")
}

/// A route table of `vpc_id` carrying `tags`, as its id.
async fn create_route_table(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![param("VpcId", vpc_id)];
    params.extend(tag_specification("route-table", tags));
    let body = ok(case, router, "CreateRouteTable", &params).await;
    member(case, &body, "routeTableId")
}

/// An internet gateway attached to `vpc_id`, as its id — the target the routes these suites
/// create point at, so each route is one a `CreateRoute` really stored.
async fn attached_internet_gateway(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
) -> String {
    let body = ok(case, router, "CreateInternetGateway", &[]).await;
    let internet_gateway_id = member(case, &body, "internetGatewayId");
    ok(
        case,
        router,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &internet_gateway_id),
            param("VpcId", vpc_id),
        ],
    )
    .await;
    internet_gateway_id
}

/// One route of `route_table_id` sending `destination` to `gateway_id`.
async fn create_route(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    destination: &str,
    gateway_id: &str,
) {
    ok(
        case,
        router,
        "CreateRoute",
        &[
            param("RouteTableId", route_table_id),
            param("DestinationCidrBlock", destination),
            param("GatewayId", gateway_id),
        ],
    )
    .await;
}

/// One `DeleteRouteTable` call for `route_table_id`, as its status and body.
async fn delete_route_table(
    router: &MultiServiceRouter,
    route_table_id: &str,
    dry_run: bool,
) -> (u16, String) {
    let mut params = vec![param("RouteTableId", route_table_id)];
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    call(router, "DeleteRouteTable", &params).await
}

/// One route table as the reader reports it, in the members a delete of ANOTHER table has to
/// leave alone: the table's identity, whether it carries the VPC's implicit (main)
/// association, the association ids an `AssociateRouteTable` put on it, the destinations of
/// its routes and its tags.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedTable {
    route_table_id: String,
    main: bool,
    associations: BTreeSet<String>,
    destinations: BTreeSet<String>,
    tags: BTreeSet<(String, String)>,
}

/// Every route table of `vpc_id`, as `DescribeRouteTables` narrowed by the `vpc-id` filter
/// reports them, keyed by id so nothing a suite asserts depends on the report's order.
async fn tables_of_vpc(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
) -> Vec<ReportedTable> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &filters(&[("vpc-id", &[vpc_id])]),
    )
    .await;
    let parsed = document(case, &body);
    let mut reported: Vec<ReportedTable> = set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .map(|item| ReportedTable {
            route_table_id: child_text(item, "routeTableId").unwrap_or_default(),
            main: set_items(item, "associationSet")
                .into_iter()
                .any(|association| {
                    child_text(association, "main").as_deref() == Some("true")
                }),
            associations: set_items(item, "associationSet")
                .into_iter()
                .filter(|association| {
                    child_text(*association, "main").as_deref() != Some("true")
                })
                .filter_map(|association| {
                    child_text(association, "routeTableAssociationId")
                })
                .collect(),
            destinations: set_items(item, "routeSet")
                .into_iter()
                .filter_map(|route| child_text(route, "destinationCidrBlock"))
                .collect(),
            tags: set_items(item, "tagSet")
                .into_iter()
                .filter_map(|tag| {
                    Some((child_text(tag, "key")?, child_text(tag, "value")?))
                })
                .collect(),
        })
        .collect();
    reported.sort_by(|left, right| left.route_table_id.cmp(&right.route_table_id));
    reported
}

/// The one table of `reported` with `route_table_id`, or a panic naming the case.
fn reported_table(case: &str, reported: &[ReportedTable], route_table_id: &str) -> ReportedTable {
    reported
        .iter()
        .find(|table| table.route_table_id == route_table_id)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {route_table_id} reported, got {reported:?}")
        })
}

/// The MAIN route table of `vpc_id` — "your VPC automatically comes with a main route table"
/// (Amazon VPC User Guide, "Route tables") — read off the implicit association the reader
/// reports on it.
async fn main_table_of_vpc(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let reported = tables_of_vpc(case, router, vpc_id).await;
    reported
        .iter()
        .find(|table| table.main)
        .map(|table| table.route_table_id.clone())
        .unwrap_or_else(|| panic!("case {case:?}: expected a main table, got {reported:?}"))
}

/// The sentence EC2 words a not-found route table id with, on its own.
fn not_found_sentence(route_table_id: &str) -> String {
    format!("The routeTable ID '{route_table_id}' does not exist")
}

/// The sentence EC2 words a blocked delete with, on its own.
fn dependency_sentence(route_table_id: &str) -> String {
    format!("The routeTable '{route_table_id}' has dependencies and cannot be deleted.")
}

/// Rule 1: the delete takes the named table and the routes that lived in it, and leaves every
/// other table of the VPC as the reader reported it.
///
/// The rows differ in how many routes the deleted table carried, which is what "the routes
/// that live in it" generalises over; each row keeps a second table of the SAME VPC holding
/// the SAME destinations, so a route that went away went away with its own table only.
#[tokio::test]
async fn a_delete_removes_the_named_table_and_the_routes_that_lived_in_it() {
    let router = emulator();
    // (case, the VPC's block, the destinations both the deleted and the kept table route)
    let cases: [(&str, &str, &[&str]); 3] = [
        ("no route of its own", "10.80.0.0/16", &[]),
        ("one route", "10.81.0.0/16", &["0.0.0.0/0"]),
        (
            "several routes",
            "10.82.0.0/16",
            &["0.0.0.0/0", "192.168.0.0/16", "172.16.0.0/12"],
        ),
    ];

    for (case, vpc_cidr, destinations) in cases {
        let vpc_id = create_vpc(case, &router, vpc_cidr).await;
        let gateway_id = attached_internet_gateway(case, &router, &vpc_id).await;
        let doomed = create_route_table(case, &router, &vpc_id, &[("Name", "doomed")]).await;
        let kept = create_route_table(case, &router, &vpc_id, &[("Name", "kept")]).await;
        for &destination in destinations {
            create_route(case, &router, &doomed, destination, &gateway_id).await;
            create_route(case, &router, &kept, destination, &gateway_id).await;
        }
        let main_id = main_table_of_vpc(case, &router, &vpc_id).await;
        let before = tables_of_vpc(case, &router, &vpc_id).await;
        let kept_before = reported_table(case, &before, &kept);
        let main_before = reported_table(case, &before, &main_id);

        let (status, body) = delete_route_table(&router, &doomed, false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete of an unused table to succeed, got {status} \
             with body {body}"
        );

        // The id resolves to nothing now, and a second delete of it is refused for that same
        // reason rather than answered as another deletion.
        let (status, body) = call(
            &router,
            "DescribeRouteTables",
            &[param("RouteTableId.1", &doomed)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the deleted table reported for nobody, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_ROUTE_TABLE_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_ROUTE_TABLE_ID_NOT_FOUND}, got {body}"
        );
        let (status, body) = delete_route_table(&router, &doomed, false).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a second delete refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_ROUTE_TABLE_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_ROUTE_TABLE_ID_NOT_FOUND} from a second delete, \
             got {body}"
        );

        // The VPC's other tables are reported exactly as before, routes and tags included.
        let after = tables_of_vpc(case, &router, &vpc_id).await;
        assert!(
            !after
                .iter()
                .any(|table| table.route_table_id == doomed),
            "case {case:?}: expected {doomed} gone from the VPC's tables, got {after:?}"
        );
        assert_eq!(
            reported_table(case, &after, &kept),
            kept_before,
            "case {case:?}: expected the kept table unchanged, got {after:?}"
        );
        assert_eq!(
            reported_table(case, &after, &main_id),
            main_before,
            "case {case:?}: expected the main table unchanged, got {after:?}"
        );

        // The routes of the deleted table went with it: the route filter selects the table
        // that still holds the destination and never the one that was deleted.
        for &destination in destinations {
            let body = ok(
                case,
                &router,
                "DescribeRouteTables",
                &filters(&[("route.destination-cidr-block", &[destination])]),
            )
            .await;
            let selected = elements(&body, "routeTableId");
            assert!(
                !selected.contains(&doomed),
                "case {case:?}: expected no table routing {destination} to be {doomed}, got \
                 {selected:?}"
            );
            assert!(
                selected.contains(&kept),
                "case {case:?}: expected {kept} still routing {destination}, got {selected:?}"
            );
        }
    }
}

/// Rule 2: a delete naming a table the account holds no record of is refused with that
/// resource's own not-found code, worded with the id the REQUEST named, and removes nothing.
///
/// The rows differ in which form the unresolvable id takes — a long id of the right prefix
/// naming nothing, the short form, an id of another resource kind, and the id of a table this
/// account really deleted — which is what "a table this account holds no table for"
/// generalises over.
#[tokio::test]
async fn a_delete_naming_no_table_of_the_account_is_refused_and_removes_nothing() {
    let router = emulator();
    let vpc_id = create_vpc("unresolvable ids", &router, "10.83.0.0/16").await;
    let held = create_route_table("unresolvable ids", &router, &vpc_id, &[]).await;
    let deleted = create_route_table("unresolvable ids", &router, &vpc_id, &[]).await;
    let (status, body) = delete_route_table(&router, &deleted, false).await;
    assert_eq!(
        status, 200,
        "the delete that sets the row up must succeed, got {status} with body {body}"
    );
    let before = tables_of_vpc("unresolvable ids", &router, &vpc_id).await;

    // (case, the id the request names)
    let cases: [(&str, &str); 4] = [
        ("a long id naming no table", "rtb-00000000000000000"),
        ("the short form of an id naming no table", "rtb-1a2b3c4d"),
        ("an id of another resource kind", "vpc-00000000000000000"),
        ("the id of a table this account deleted", deleted.as_str()),
    ];

    for (case, route_table_id) in cases {
        let (status, body) = delete_route_table(&router, route_table_id, false).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_ROUTE_TABLE_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_ROUTE_TABLE_ID_NOT_FOUND}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            not_found_sentence(route_table_id),
            "case {case:?}: expected {:?}, got {body}",
            not_found_sentence(route_table_id)
        );
    }

    // Every refusal removed nothing: the table the account does hold is still reported, and
    // the VPC's tables read back as they did before the refused calls.
    let after = tables_of_vpc("unresolvable ids", &router, &vpc_id).await;
    assert_eq!(
        after, before,
        "expected the refused deletes to remove nothing, got {after:?}"
    );
    assert!(
        after.iter().any(|table| table.route_table_id == held),
        "expected {held} still held after the refusals, got {after:?}"
    );
}

/// Rule 3: a delete of a table something still uses is refused with `DependencyViolation`
/// worded for the table the request named, and the table — with its routes and associations —
/// is still there afterwards.
///
/// The rows differ in WHAT still uses the table: the VPC itself, through the implicit
/// association of its main table ("you can't delete the main route table"), and one or more
/// subnets through the associations an `AssociateRouteTable` made ("you must disassociate the
/// route table from any subnets before you can delete it").
#[tokio::test]
async fn a_delete_of_a_table_something_still_uses_is_refused() {
    let router = emulator();
    // (case, the VPC's block, the subnet blocks associated with the table under test — an
    // empty list meaning the row tests the VPC's own main table instead)
    let cases: [(&str, &str, &[&str]); 3] = [
        ("the VPC's main table", "10.84.0.0/16", &[]),
        ("a table one subnet uses", "10.85.0.0/16", &["10.85.1.0/24"]),
        (
            "a table two subnets use",
            "10.86.0.0/16",
            &["10.86.1.0/24", "10.86.2.0/24"],
        ),
    ];

    for (case, vpc_cidr, subnet_cidrs) in cases {
        let vpc_id = create_vpc(case, &router, vpc_cidr).await;
        let gateway_id = attached_internet_gateway(case, &router, &vpc_id).await;
        let route_table_id = if subnet_cidrs.is_empty() {
            main_table_of_vpc(case, &router, &vpc_id).await
        } else {
            let route_table_id = create_route_table(case, &router, &vpc_id, &[]).await;
            for &subnet_cidr in subnet_cidrs {
                let subnet_id = create_subnet(case, &router, &vpc_id, subnet_cidr).await;
                ok(
                    case,
                    &router,
                    "AssociateRouteTable",
                    &[
                        param("RouteTableId", &route_table_id),
                        param("SubnetId", &subnet_id),
                    ],
                )
                .await;
            }
            route_table_id
        };
        create_route(case, &router, &route_table_id, "0.0.0.0/0", &gateway_id).await;
        let before = tables_of_vpc(case, &router, &vpc_id).await;

        let (status, body) = delete_route_table(&router, &route_table_id, false).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a table still in use, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            DEPENDENCY_VIOLATION,
            "case {case:?}: expected {DEPENDENCY_VIOLATION}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            dependency_sentence(&route_table_id),
            "case {case:?}: expected {:?}, got {body}",
            dependency_sentence(&route_table_id)
        );

        // A refused delete removes nothing: the table, its routes and its associations are
        // reported exactly as before.
        let after = tables_of_vpc(case, &router, &vpc_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused delete to remove nothing, got {after:?}"
        );
        let still_there = reported_table(case, &after, &route_table_id);
        assert_eq!(
            still_there.associations.len(),
            subnet_cidrs.len(),
            "case {case:?}: expected {} explicit association(s) left, got {still_there:?}",
            subnet_cidrs.len()
        );
    }
}

/// Rule 4: a `DryRun` of a delete that WOULD have succeeded answers `DryRunOperation` and
/// removes nothing, and the same delete without the flag then succeeds.
///
/// The rows differ in what the table carries when the probe reaches it — nothing, a route, a
/// route and tags — which is what "the request would have succeeded" generalises over.
#[tokio::test]
async fn a_dry_run_delete_records_nothing_and_the_real_delete_then_succeeds() {
    let router = emulator();
    // (case, the VPC's block, the destinations the table routes, the table's tags)
    type Row<'a> = (&'a str, &'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);
    let cases: [Row<'_>; 3] = [
        ("an empty table", "10.87.0.0/16", &[], &[]),
        ("a table with a route", "10.88.0.0/16", &["0.0.0.0/0"], &[]),
        (
            "a tagged table with routes",
            "10.89.0.0/16",
            &["0.0.0.0/0", "192.168.10.0/24"],
            &[("Name", "probe"), ("team", "platform")],
        ),
    ];

    for (case, vpc_cidr, destinations, tags) in cases {
        let vpc_id = create_vpc(case, &router, vpc_cidr).await;
        let gateway_id = attached_internet_gateway(case, &router, &vpc_id).await;
        let route_table_id = create_route_table(case, &router, &vpc_id, tags).await;
        for &destination in destinations {
            create_route(case, &router, &route_table_id, destination, &gateway_id).await;
        }
        let before = tables_of_vpc(case, &router, &vpc_id).await;

        let (status, body) = delete_route_table(&router, &route_table_id, true).await;
        assert_eq!(
            status, DRY_RUN_OPERATION_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS} from the dry run, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            DRY_RUN_OPERATION,
            "case {case:?}: expected {DRY_RUN_OPERATION}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            DRY_RUN_OPERATION_MESSAGE,
            "case {case:?}: expected {DRY_RUN_OPERATION_MESSAGE:?}, got {body}"
        );

        // The probe asked a question: the table and everything in it is still reported.
        let after_probe = tables_of_vpc(case, &router, &vpc_id).await;
        assert_eq!(
            after_probe, before,
            "case {case:?}: expected the dry run to remove nothing, got {after_probe:?}"
        );

        // Without the flag, the same request is carried out.
        let (status, body) = delete_route_table(&router, &route_table_id, false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the real delete to succeed, got {status} with body {body}"
        );
        let after = tables_of_vpc(case, &router, &vpc_id).await;
        assert!(
            !after
                .iter()
                .any(|table| table.route_table_id == route_table_id),
            "case {case:?}: expected {route_table_id} gone after the real delete, got {after:?}"
        );
    }
}
