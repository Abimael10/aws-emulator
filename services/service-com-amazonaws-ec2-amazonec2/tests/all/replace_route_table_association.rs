// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ReplaceRouteTableAssociation`: which route table the thing an
//! association binds uses, changed and read back through `DescribeRouteTables`. Driven over
//! the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=…`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2` signing
//! name in the request's SigV4 credential scope, so every call here carries one.
//!
//! The rules under test:
//!
//! 1. "Changes the route table associated with a given subnet … in a VPC. After the operation
//!    completes, the subnet or gateway uses the routes in the new route table"
//!    (`ReplaceRouteTableAssociation`): what the association the request names BINDS stays
//!    bound, to the table the request names instead — so that subnet is reported against
//!    exactly one table afterwards, the association the request named is reported by no table
//!    at all, and every other association is left where it was.
//! 2. "You can also use this operation to change which table is the main route table in the
//!    VPC. Specify the main route table's association ID and the route table ID of the new
//!    main route table": the implicit association of a main table is one a caller may name,
//!    and moving it leaves the VPC with exactly one main table — the one the request named,
//!    which the documented `association.main` filter then selects.
//! 3. "The ID of the new association" (`$NewAssociationId`) and "the state of the
//!    association" (`$AssociationState`): the call answers the id of the association it wrote
//!    — an association id of EC2's own form, distinct from the one the request named — and
//!    the state that association is reported in, which the next request can name in turn.
//! 4. An id that resolves to nothing is a refusal, not an association made out of nothing: a
//!    value this service could not have issued as an association id is unreadable
//!    (`InvalidRouteTableAssociationId.Malformed`), an id it could have issued that names no
//!    association is `InvalidAssociationID.NotFound`, and a route table id the account holds
//!    no table for is `InvalidRouteTableID.NotFound`.
//! 5. A route table routes for one VPC (`RouteTable$VpcId`), so what an association binds can
//!    only move onto a table of that same VPC: a request pairing the two across VPCs is
//!    refused and moves nothing.
//! 6. A `DryRun` of a request that would have succeeded answers `DryRunOperation` and moves
//!    nothing.
//!
//! Each table row is a distinct input along what those rules generalise over — which
//! association moves (a subnet's explicit one or the VPC's implicit main one), whether the
//! target is a table the caller created or the table the VPC came with, whether the target
//! already holds another association, whether it is the table the association is already on,
//! and how many hops a chain of replacements makes — so no single recorded request is the
//! specification.

use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_STATUS, INVALID_ASSOCIATION_ID_NOT_FOUND,
    INVALID_PARAMETER_VALUE, INVALID_ROUTE_TABLE_ASSOCIATION_ID_MALFORMED,
    INVALID_ROUTE_TABLE_ID_NOT_FOUND, ROUTE_TABLE_ASSOCIATION_ID_PREFIX,
    ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, filters};
use super::wire::{child, child_text, document, is_long_id, param, set_items, Param};

/// The Region every call here is signed for. A route table carries no Region member, and the
/// default VPC a fresh account already has is seeded for the emulator's default Region.
const REGION: &str = "us-east-1";

/// One association between a route table and what it binds, as `DescribeRouteTables` reports
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAssociation {
    association_id: String,
    route_table_id: String,
    subnet_id: Option<String>,
    main: bool,
    state: Option<String>,
}

/// One reported route table, down to the associations it holds.
#[derive(Debug, Clone)]
struct ReportedTable {
    route_table_id: String,
    vpc_id: String,
    associations: Vec<ReportedAssociation>,
}

/// Every route table the account holds, read off the TREE: an association's members are
/// children of its own `<item>`, so scanning the body for an element name would mix two
/// associations together.
async fn described_tables(router: &MultiServiceRouter, case: &str) -> Vec<ReportedTable> {
    let (status, body) = super::ec2_query::call(router, REGION, "DescribeRouteTables", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 reading the route tables back, got {status} with \
         body {body}"
    );
    let parsed = document(case, &body);
    let tables: Vec<ReportedTable> = set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .map(|table| ReportedTable {
            route_table_id: child_text(table, "routeTableId").unwrap_or_default(),
            vpc_id: child_text(table, "vpcId").unwrap_or_default(),
            associations: set_items(table, "associationSet")
                .into_iter()
                .map(|association| ReportedAssociation {
                    association_id: child_text(association, "routeTableAssociationId")
                        .unwrap_or_default(),
                    route_table_id: child_text(association, "routeTableId").unwrap_or_default(),
                    subnet_id: child_text(association, "subnetId"),
                    main: child_text(association, "main").as_deref() == Some("true"),
                    state: child(association, "associationState")
                        .and_then(|state| child_text(state, "state")),
                })
                .collect(),
        })
        .collect();
    // Every association is reported ON the table it names: an association is a record of the
    // table's own `Associations`, so a reader that found one under another table's id would be
    // reading a second copy of it.
    for table in &tables {
        for association in &table.associations {
            assert_eq!(
                association.route_table_id, table.route_table_id,
                "case {case:?}: expected {association:?} to be reported on the table it names, \
                 got it under {held}",
                held = table.route_table_id
            );
        }
    }
    tables
}

/// Every association the account's route tables report, whichever table holds it.
fn all_associations(tables: &[ReportedTable]) -> Vec<ReportedAssociation> {
    tables
        .iter()
        .flat_map(|table| table.associations.iter().cloned())
        .collect()
}

/// The associations reported for `subnet_id` — a LIST, so a move that left the subnet on two
/// tables at once is caught rather than hidden by a search that stops at the first match.
fn associations_of_subnet(tables: &[ReportedTable], subnet_id: &str) -> Vec<ReportedAssociation> {
    all_associations(tables)
        .into_iter()
        .filter(|association| association.subnet_id.as_deref() == Some(subnet_id))
        .collect()
}

/// The one association reported for `subnet_id`, or a panic naming the case: a subnet uses
/// exactly one route table, so it carries at most one explicit association.
fn association_of_subnet(
    tables: &[ReportedTable],
    case: &str,
    subnet_id: &str,
) -> ReportedAssociation {
    let found = associations_of_subnet(tables, subnet_id);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected {subnet_id} to be associated with exactly one route table, \
         got {found:?}"
    );
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a list of one has a first element: {subnet_id}"))
}

/// The MAIN associations of the VPC `vpc_id` — a LIST, so a move that left the VPC with two
/// main tables (or none) is caught.
fn main_associations_of_vpc(tables: &[ReportedTable], vpc_id: &str) -> Vec<ReportedAssociation> {
    tables
        .iter()
        .filter(|table| table.vpc_id == vpc_id)
        .flat_map(|table| table.associations.iter().cloned())
        .filter(|association| association.main)
        .collect()
}

/// The one main association of `vpc_id`, or a panic naming the case.
fn main_association_of_vpc(
    tables: &[ReportedTable],
    case: &str,
    vpc_id: &str,
) -> ReportedAssociation {
    let found = main_associations_of_vpc(tables, vpc_id);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected {vpc_id} to have exactly one main route table, got {found:?}"
    );
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a list of one has a first element: {vpc_id}"))
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = super::ec2_query::call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A member of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A VPC with `cidr_block`, as its id.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let case = "the VPC";
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    member(case, &body, "vpcId")
}

/// A subnet of `vpc_id` with `cidr_block`, as its id.
async fn create_subnet(router: &MultiServiceRouter, vpc_id: &str, cidr_block: &str) -> String {
    let case = "the subnet";
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", vpc_id), param("CidrBlock", cidr_block)],
    )
    .await;
    member(case, &body, "subnetId")
}

/// A route table of `vpc_id`, as its id.
async fn create_route_table(router: &MultiServiceRouter, vpc_id: &str) -> String {
    let case = "the route table";
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    member(case, &body, "routeTableId")
}

/// The association `AssociateRouteTable` makes between `subnet_id` and `route_table_id`, as
/// its id.
async fn associate(router: &MultiServiceRouter, route_table_id: &str, subnet_id: &str) -> String {
    let case = "the first association";
    let body = ok(
        case,
        router,
        "AssociateRouteTable",
        &[
            param("RouteTableId", route_table_id),
            param("SubnetId", subnet_id),
        ],
    )
    .await;
    member(case, &body, "associationId")
}

/// One `ReplaceRouteTableAssociation` call naming `association_id` and `route_table_id`, with
/// `extra` carrying whatever else the row under test sends.
async fn replace_association(
    router: &MultiServiceRouter,
    association_id: &str,
    route_table_id: &str,
    extra: &[Param],
) -> (u16, String) {
    let mut params = vec![
        param("AssociationId", association_id),
        param("RouteTableId", route_table_id),
    ];
    params.extend_from_slice(extra);
    super::ec2_query::call(router, REGION, "ReplaceRouteTableAssociation", &params).await
}

/// An association id this service COULD have issued that no reported association carries: the
/// held id with its last hexadecimal digit replaced by the first value no association is
/// reported under, so the refusal it draws is the not-found half of the pair rather than the
/// unreadable one.
fn unheld_association_id(tables: &[ReportedTable], held: &str) -> String {
    let in_use: Vec<String> = all_associations(tables)
        .into_iter()
        .map(|association| association.association_id)
        .collect();
    for digit in "0123456789abcdef".chars() {
        let mut candidate = held.to_string();
        candidate.pop();
        candidate.push(digit);
        if !in_use.contains(&candidate) {
            return candidate;
        }
    }
    panic!("every id of the held id's form is in use: {in_use:?}")
}

/// A route table id of EC2's own form that the account holds no table for: the
/// `0123456789abcdef0` shape a hand-written probe reaches for, which no mint produces.
const UNHELD_ROUTE_TABLE_ID: &str = "rtb-0123456789abcdef0";

/// The estate the moves below happen in: one VPC with the table it came with, two tables the
/// caller created in it and three subnets, plus a second VPC with a table of its own that no
/// association of the first VPC may move onto.
struct Fixture {
    vpc_id: String,
    main_route_table_id: String,
    created_route_table_ids: [String; 2],
    subnet_ids: [String; 3],
    other_vpc_route_table_id: String,
}

/// The fixture, built over the wire, with each subnet explicitly associated with the FIRST
/// created table — the association every move below starts from.
async fn fixture(router: &MultiServiceRouter) -> Fixture {
    let case = "the fixture";
    let vpc_id = create_vpc(router, "10.30.0.0/16").await;
    let created_route_table_ids = [
        create_route_table(router, &vpc_id).await,
        create_route_table(router, &vpc_id).await,
    ];
    let subnet_ids = [
        create_subnet(router, &vpc_id, "10.30.1.0/24").await,
        create_subnet(router, &vpc_id, "10.30.2.0/24").await,
        create_subnet(router, &vpc_id, "10.30.3.0/24").await,
    ];
    for subnet_id in &subnet_ids {
        associate(router, &created_route_table_ids[0], subnet_id).await;
    }
    let tables = described_tables(router, case).await;
    let main_route_table_id = main_association_of_vpc(&tables, case, &vpc_id).route_table_id;
    let other_vpc_id = create_vpc(router, "10.31.0.0/16").await;
    let other_vpc_route_table_id = create_route_table(router, &other_vpc_id).await;
    Fixture {
        vpc_id,
        main_route_table_id,
        created_route_table_ids,
        subnet_ids,
        other_vpc_route_table_id,
    }
}

/// Which table a row moves an association onto.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// The table the VPC came with, which is its main route table.
    VpcMain,
    /// One of the two tables the caller created in the VPC.
    Created(usize),
}

impl Target {
    /// The id the target names in `fixture`.
    fn id(self, fixture: &Fixture) -> String {
        match self {
            Target::VpcMain => fixture.main_route_table_id.clone(),
            Target::Created(index) => fixture.created_route_table_ids[index].clone(),
        }
    }
}

/// A move takes what the association binds off the table it was on and puts it on the one the
/// request names: one association for that subnet afterwards, carrying the answered id and the
/// state the answer reported, and every other subnet left where it was.
#[tokio::test]
async fn a_replacement_moves_the_subnet_onto_the_named_route_table() {
    let (router, _engine) = super::ec2_query::emulator();
    let fixture = fixture(&router).await;

    // (case, which subnet moves, the table it moves onto) — the rows span what a move can be:
    // off a created table onto another, onto a table a second subnet already uses, onto the
    // table the VPC came with, onto the table the subnet is ALREADY associated with, and a
    // subnet that has not moved before.
    let cases: [(&str, usize, Target); 6] = [
        ("from one created table to the other", 0, Target::Created(1)),
        (
            "a second subnet onto the created table the first already uses",
            1,
            Target::Created(1),
        ),
        (
            "onto the table the VPC came with",
            0,
            Target::VpcMain,
        ),
        ("back onto a created table", 0, Target::Created(0)),
        (
            "onto the table the subnet is already associated with",
            1,
            Target::Created(1),
        ),
        (
            "a subnet that has not moved before",
            2,
            Target::Created(1),
        ),
    ];

    // Where each subnet starts: on the first created table, which the fixture associated it
    // with.
    let mut expected: Vec<String> = fixture
        .subnet_ids
        .iter()
        .map(|_| fixture.created_route_table_ids[0].clone())
        .collect();

    for (case, subnet_index, target) in cases {
        let subnet_id = &fixture.subnet_ids[subnet_index];
        let target_route_table_id = target.id(&fixture);
        let tables = described_tables(&router, case).await;
        let before = association_of_subnet(&tables, case, subnet_id);

        let (status, body) =
            replace_association(&router, &before.association_id, &target_route_table_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let new_association_id = member(case, &body, "newAssociationId");
        assert!(
            is_long_id(ROUTE_TABLE_ASSOCIATION_ID_PREFIX, &new_association_id),
            "case {case:?}: expected an association id of EC2's own form, got \
             {new_association_id:?}"
        );
        assert_ne!(
            new_association_id, before.association_id,
            "case {case:?}: expected the id of a NEW association, got the one the request \
             named ({new_association_id:?})"
        );
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED),
            "case {case:?}: expected the new association reported \
             {ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED}, got {body}"
        );

        expected[subnet_index] = target_route_table_id.clone();
        let tables = described_tables(&router, case).await;

        // The subnet is associated with the named table, once, under the answered id and in
        // the state the answer reported.
        let after = association_of_subnet(&tables, case, subnet_id);
        assert_eq!(
            after,
            ReportedAssociation {
                association_id: new_association_id.clone(),
                route_table_id: target_route_table_id.clone(),
                subnet_id: Some(subnet_id.clone()),
                main: false,
                state: Some(ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED.to_string()),
            },
            "case {case:?}: expected the subnet on {target_route_table_id} under \
             {new_association_id}, got {after:?}"
        );

        // The association the request named is reported by no table at all.
        let lingering: Vec<ReportedAssociation> = all_associations(&tables)
            .into_iter()
            .filter(|association| association.association_id == before.association_id)
            .collect();
        assert!(
            lingering.is_empty(),
            "case {case:?}: expected the replaced association {replaced} to be gone, got \
             {lingering:?}",
            replaced = before.association_id
        );

        // Every subnet is still on exactly one table — the one it was last moved onto — and
        // the VPC still has exactly one main table.
        for (index, subnet) in fixture.subnet_ids.iter().enumerate() {
            let association = association_of_subnet(&tables, case, subnet);
            assert_eq!(
                association.route_table_id, expected[index],
                "case {case:?}: expected {subnet} on {want}, got {association:?}",
                want = expected[index]
            );
        }
        let main = main_association_of_vpc(&tables, case, &fixture.vpc_id);
        assert_eq!(
            main.route_table_id, fixture.main_route_table_id,
            "case {case:?}: expected the VPC's main table unchanged, got {main:?}"
        );
    }
}

/// The main-route-table half of the rule: the implicit association of a main table is one a
/// caller may name, and moving it makes the named table the VPC's main table — exactly one,
/// which the documented `association.main` filter then selects.
#[tokio::test]
async fn a_replacement_moves_which_table_is_the_main_route_table() {
    let (router, _engine) = super::ec2_query::emulator();
    let fixture = fixture(&router).await;

    // The rows span where the main association can go: onto a table that already holds every
    // subnet's explicit association, onto the other created table, and back onto the table the
    // VPC came with.
    let cases: [(&str, Target); 3] = [
        (
            "onto the created table the subnets use",
            Target::Created(0),
        ),
        ("onto the other created table", Target::Created(1)),
        ("back onto the table the VPC came with", Target::VpcMain),
    ];

    for (case, target) in cases {
        let target_route_table_id = target.id(&fixture);
        let tables = described_tables(&router, case).await;
        let before = main_association_of_vpc(&tables, case, &fixture.vpc_id);

        let (status, body) =
            replace_association(&router, &before.association_id, &target_route_table_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let new_association_id = member(case, &body, "newAssociationId");
        assert_ne!(
            new_association_id, before.association_id,
            "case {case:?}: expected the id of a NEW association, got the one the request \
             named ({new_association_id:?})"
        );

        let tables = described_tables(&router, case).await;
        let main = main_association_of_vpc(&tables, case, &fixture.vpc_id);
        assert_eq!(
            main,
            ReportedAssociation {
                association_id: new_association_id.clone(),
                route_table_id: target_route_table_id.clone(),
                subnet_id: None,
                main: true,
                state: Some(ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED.to_string()),
            },
            "case {case:?}: expected {target_route_table_id} reported as the main table under \
             {new_association_id}, got {main:?}"
        );

        // The documented filter selects the table the main association is now on.
        let filtered = ok(
            case,
            &router,
            "DescribeRouteTables",
            &filters(&[("association.main", &["true"])]),
        )
        .await;
        let selected = super::ec2_query::elements(&filtered, "routeTableId");
        assert!(
            selected.contains(&target_route_table_id),
            "case {case:?}: expected the association.main filter to select \
             {target_route_table_id}, got {filtered}"
        );

        // The subnets' own explicit associations are untouched by a main-table move: each is
        // still on the table the fixture associated it with.
        for subnet_id in &fixture.subnet_ids {
            let association = association_of_subnet(&tables, case, subnet_id);
            assert_eq!(
                association.route_table_id, fixture.created_route_table_ids[0],
                "case {case:?}: expected {subnet_id} left on {table}, got {association:?}",
                table = fixture.created_route_table_ids[0]
            );
        }
    }
}

/// The answered id is the one the NEXT request names: a chain of moves driven entirely off
/// what each call answered leaves the subnet on the last table of the chain, and only there.
#[tokio::test]
async fn the_answered_id_names_the_association_the_next_replacement_replaces() {
    let (router, _engine) = super::ec2_query::emulator();
    let fixture = fixture(&router).await;
    let subnet_id = &fixture.subnet_ids[0];

    let case = "the start of the chain";
    let tables = described_tables(&router, case).await;
    let mut association_id = association_of_subnet(&tables, case, subnet_id).association_id;

    // Each hop names the id the hop before it answered, so nothing here is read back out of
    // the report between moves.
    let chain = [
        ("hop onto the second created table", Target::Created(1)),
        ("hop onto the table the VPC came with", Target::VpcMain),
        ("hop back onto the first created table", Target::Created(0)),
        ("hop onto the second created table again", Target::Created(1)),
    ];
    for (case, target) in chain {
        let target_route_table_id = target.id(&fixture);
        let (status, body) =
            replace_association(&router, &association_id, &target_route_table_id, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        association_id = member(case, &body, "newAssociationId");
    }

    let case = "the end of the chain";
    let tables = described_tables(&router, case).await;
    let after = association_of_subnet(&tables, case, subnet_id);
    assert_eq!(
        after,
        ReportedAssociation {
            association_id: association_id.clone(),
            route_table_id: fixture.created_route_table_ids[1].clone(),
            subnet_id: Some(subnet_id.clone()),
            main: false,
            state: Some(ROUTE_TABLE_ASSOCIATION_STATE_ASSOCIATED.to_string()),
        },
        "case {case:?}: expected the chain to end on {table} under {association_id}, got \
         {after:?}",
        table = fixture.created_route_table_ids[1]
    );
}

/// What a replacement refuses, and the fact that a refused request moves nothing.
#[tokio::test]
async fn a_request_that_cannot_be_served_moves_nothing() {
    let (router, _engine) = super::ec2_query::emulator();
    let fixture = fixture(&router).await;
    let subnet_id = &fixture.subnet_ids[0];
    let case = "before the refusals";
    let tables = described_tables(&router, case).await;
    let held = association_of_subnet(&tables, case, subnet_id);

    // (case, the association id the row sends, the route table id it sends, the code EC2 words
    // that class with) — the rows span what a request can get wrong: a value that is no
    // association id this service could have issued, whatever its printed form; an id it could
    // have issued that names no association; a table id the account holds no table for; and a
    // table of another VPC.
    let rows: Vec<(&str, String, String, &str)> = vec![
        (
            "a value that is not an association id at all",
            "not-an-association".to_string(),
            fixture.created_route_table_ids[1].clone(),
            INVALID_ROUTE_TABLE_ASSOCIATION_ID_MALFORMED,
        ),
        (
            "a documentation-shaped id this service never issued",
            format!("{ROUTE_TABLE_ASSOCIATION_ID_PREFIX}-0123456789abcdef0"),
            fixture.created_route_table_ids[1].clone(),
            INVALID_ROUTE_TABLE_ASSOCIATION_ID_MALFORMED,
        ),
        (
            "an association id no route table holds",
            unheld_association_id(&tables, &held.association_id),
            fixture.created_route_table_ids[1].clone(),
            INVALID_ASSOCIATION_ID_NOT_FOUND,
        ),
        (
            "a route table id the account holds no table for",
            held.association_id.clone(),
            UNHELD_ROUTE_TABLE_ID.to_string(),
            INVALID_ROUTE_TABLE_ID_NOT_FOUND,
        ),
        (
            "a route table of another VPC",
            held.association_id.clone(),
            fixture.other_vpc_route_table_id.clone(),
            INVALID_PARAMETER_VALUE,
        ),
        (
            // Two faults: the association is resolved before the table, so a request that
            // gets both wrong is refused for the association.
            "an unknown association id beside an unknown route table id",
            unheld_association_id(&tables, &held.association_id),
            UNHELD_ROUTE_TABLE_ID.to_string(),
            INVALID_ASSOCIATION_ID_NOT_FOUND,
        ),
    ];

    for (case, association_id, route_table_id, want_code) in rows {
        let (status, body) =
            replace_association(&router, &association_id, &route_table_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, want_code,
            "case {case:?}: expected {want_code}, got {code} with body {body}"
        );

        let tables = described_tables(&router, case).await;
        let after = association_of_subnet(&tables, case, subnet_id);
        assert_eq!(
            after, held,
            "case {case:?}: expected a refused request to move nothing, got {after:?}"
        );
    }

    // The cross-VPC refusal names the pair it collided on, so a caller can act on it.
    let (_, body) = replace_association(
        &router,
        &held.association_id,
        &fixture.other_vpc_route_table_id,
        &[],
    )
    .await;
    let message = error_message(&body);
    assert!(
        message.contains(&fixture.other_vpc_route_table_id) && message.contains(subnet_id),
        "case {case:?}: expected the cross-VPC refusal to name the table and the subnet, got \
         {message:?}",
        case = "the cross-VPC refusal's message"
    );
}

/// A dry run of a request that would have succeeded answers `DryRunOperation` and leaves every
/// association exactly as it was — for a subnet's explicit association and for the VPC's
/// implicit main one alike.
#[tokio::test]
async fn a_dry_run_moves_nothing() {
    let (router, _engine) = super::ec2_query::emulator();
    let fixture = fixture(&router).await;
    let subnet_id = &fixture.subnet_ids[0];

    // (case, whether the row names the main association, the table it would move onto)
    let cases: [(&str, bool, Target); 4] = [
        ("a subnet's association onto a created table", false, Target::Created(1)),
        (
            "a subnet's association onto the table it already uses",
            false,
            Target::Created(0),
        ),
        ("the main association onto a created table", true, Target::Created(1)),
        (
            "the main association onto the table it is already on",
            true,
            Target::VpcMain,
        ),
    ];

    for (case, names_main, target) in cases {
        let tables = described_tables(&router, case).await;
        let subnet_association = association_of_subnet(&tables, case, subnet_id);
        let main_association = main_association_of_vpc(&tables, case, &fixture.vpc_id);
        let named = if names_main {
            main_association.association_id.clone()
        } else {
            subnet_association.association_id.clone()
        };

        let (status, body) = replace_association(
            &router,
            &named,
            &target.id(&fixture),
            &[param("DryRun", "true")],
        )
        .await;
        assert_eq!(
            status, DRY_RUN_OPERATION_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS}, got {status} with body \
             {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, DRY_RUN_OPERATION,
            "case {case:?}: expected {DRY_RUN_OPERATION}, got {code} with body {body}"
        );

        let tables = described_tables(&router, case).await;
        assert_eq!(
            association_of_subnet(&tables, case, subnet_id),
            subnet_association,
            "case {case:?}: expected the dry run to leave the subnet's association as it was"
        );
        assert_eq!(
            main_association_of_vpc(&tables, case, &fixture.vpc_id),
            main_association,
            "case {case:?}: expected the dry run to leave the main association as it was"
        );
    }
}
