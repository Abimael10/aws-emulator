// SPDX-License-Identifier: CC0-1.0

//! The STATE an EC2 route-table association is reported in by the calls that move it,
//! driven over the wire the service speaks: `ec2Query`, an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a call that moves a subnet's explicit route-table association
//! answers the state that association moved INTO, as an `associationState` element whose
//! `state` is a member of the model's `RouteTableAssociationStateCode` catalog** — the
//! state an association that holds is reported in where it is put in place
//! (`AssociateRouteTable`, and every later read of the table), and the state of the move a
//! disassociate starts where it is taken away (`DisassociateRouteTable`) — while a call
//! that is REFUSED reports no state at all, having moved nothing.
//!
//! Every row is a distinct input along what that rule generalises over: how many
//! associations the one table carries, which of them the call names, and the order they are
//! taken away in — so no single recorded call is the specification.

use ec2::ops::types::ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The state code an association that HOLDS is reported in — the
/// `RouteTableAssociationStateCode` catalog's `associated` member
/// (`ec2::ops::types::ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES`, generated from the model).
const ASSOCIATED: &str = "associated";

/// The state code the call that TAKES AN ASSOCIATION AWAY reports: the same catalog's
/// `disassociating` member — the move that call starts.
const DISASSOCIATING: &str = "disassociating";

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

/// The `associationState` a response reports, as its `state` code and its `statusMessage`,
/// or `None` when the body carries no such element at all.
fn association_state(case: &str, body: &str) -> Option<(String, Option<String>)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "associationState")
        .map(|node| {
            (
                child_text(node, "state").unwrap_or_default(),
                child_text(node, "statusMessage"),
            )
        })
}

/// Every association a `DescribeRouteTables` body reports, as its id, whether it is the
/// implicit association of a main table, and the `state` code it is reported in.
fn reported_associations(case: &str, body: &str) -> Vec<(String, bool, String)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "associationSet")
        .flat_map(|set| set.children().filter(|item| item.is_element()).collect::<Vec<_>>())
        .map(|item| {
            (
                child_text(item, "routeTableAssociationId").unwrap_or_default(),
                child_text(item, "main").as_deref() == Some("true"),
                child(item, "associationState")
                    .and_then(|state| child_text(state, "state"))
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// A VPC with `cidr_block`, as its id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no <vpcId> in {body}"))
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
    element(&body, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no <subnetId> in {body}"))
}

/// A route table of `vpc_id`, as its id.
async fn create_route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    element(&body, "routeTableId")
        .unwrap_or_else(|| panic!("case {case:?}: no <routeTableId> in {body}"))
}

/// The `DescribeRouteTables` body for the one table `route_table_id` names.
async fn describe_route_table(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> String {
    ok(
        case,
        router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await
}

/// The order a row takes its associations away in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Oldest association first.
    AsAssociated,
    /// Newest association first.
    Reversed,
}

/// Associating a subnet reports the association as one that HOLDS, every later read of the
/// table reports it the same way, and the call that takes it away reports the move it
/// started — leaving the table's other associations exactly as they were.
#[tokio::test]
async fn a_route_table_association_reports_associated_until_the_call_that_takes_it_away() {
    let router = emulator();
    // The state codes this suite asserts are ones the model publishes, not words it made up
    // (`ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES`, generated from the model).
    assert!(
        ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES.contains(&ASSOCIATED)
            && ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES.contains(&DISASSOCIATING),
        "expected {ASSOCIATED:?} and {DISASSOCIATING:?} among the modelled catalog \
         {ROUTE_TABLE_ASSOCIATION_STATE_CODE_VALUES:?}"
    );
    // (case, the VPC's block, the subnet blocks associated with the one table, the order
    // the associations are taken away in)
    let cases: [(&str, &str, &[&str], Order); 4] = [
        (
            "one subnet",
            "10.90.0.0/16",
            &["10.90.1.0/24"],
            Order::AsAssociated,
        ),
        (
            "two subnets, oldest association first",
            "10.91.0.0/16",
            &["10.91.1.0/24", "10.91.2.0/24"],
            Order::AsAssociated,
        ),
        (
            "two subnets, newest association first",
            "10.92.0.0/16",
            &["10.92.1.0/24", "10.92.2.0/24"],
            Order::Reversed,
        ),
        (
            "three subnets, newest association first",
            "10.93.0.0/16",
            &["10.93.1.0/24", "10.93.2.0/24", "10.93.3.0/24"],
            Order::Reversed,
        ),
    ];

    for (case, vpc_cidr, subnet_cidrs, order) in cases {
        let vpc_id = create_vpc(case, &router, vpc_cidr).await;
        let route_table_id = create_route_table(case, &router, &vpc_id).await;

        // Each association is reported as one that holds by the call that puts it in place.
        let mut association_ids = Vec::new();
        for subnet_cidr in subnet_cidrs {
            let subnet_id = create_subnet(case, &router, &vpc_id, subnet_cidr).await;
            let body = ok(
                case,
                &router,
                "AssociateRouteTable",
                &[
                    param("RouteTableId", &route_table_id),
                    param("SubnetId", &subnet_id),
                ],
            )
            .await;
            let reported = association_state(case, &body);
            assert_eq!(
                reported.as_ref().map(|(state, _)| state.as_str()),
                Some(ASSOCIATED),
                "case {case:?}: expected the associate to report {ASSOCIATED:?}, got \
                 {reported:?} in {body}"
            );
            association_ids.push(
                element(&body, "associationId")
                    .unwrap_or_else(|| panic!("case {case:?}: no <associationId> in {body}")),
            );
        }

        // The same state on every read of the table, so the call and the read cannot drift.
        let read = describe_route_table(case, &router, &route_table_id).await;
        for association_id in &association_ids {
            let reported = reported_associations(case, &read)
                .into_iter()
                .find(|(id, _, _)| id == association_id);
            assert_eq!(
                reported.as_ref().map(|(_, _, state)| state.as_str()),
                Some(ASSOCIATED),
                "case {case:?}: expected the read of {association_id} to report \
                 {ASSOCIATED:?}, got {reported:?} in {read}"
            );
        }

        let mut taken_in: Vec<String> = association_ids.clone();
        if order == Order::Reversed {
            taken_in.reverse();
        }
        // The table reports its associations in the order they were made, whichever order
        // they are taken away in.
        let mut still_held: Vec<String> = association_ids.clone();
        for association_id in &taken_in {
            let body = ok(
                case,
                &router,
                "DisassociateRouteTable",
                &[param("AssociationId", association_id)],
            )
            .await;
            // The state the association moved into, and nothing beside it: the move is the
            // whole report, so there is no status message explaining a state it is not in.
            let reported = association_state(case, &body);
            assert_eq!(
                reported,
                Some((DISASSOCIATING.to_string(), None)),
                "case {case:?}: expected the disassociate of {association_id} to report \
                 {DISASSOCIATING:?} and no status message, got {reported:?} in {body}"
            );
            // The envelope's own success flag is still there beside the state.
            assert_eq!(
                element(&body, "return").as_deref(),
                Some("true"),
                "case {case:?}: expected the success flag beside the state, got {body}"
            );

            // Only the named association is gone; every one still held reports the state it
            // was in before this call.
            still_held.retain(|held| held != association_id);
            let read = describe_route_table(case, &router, &route_table_id).await;
            let reported = reported_associations(case, &read);
            let explicit: Vec<&(String, bool, String)> =
                reported.iter().filter(|(_, main, _)| !main).collect();
            let held_now: Vec<&str> = explicit.iter().map(|(id, _, _)| id.as_str()).collect();
            let expected: Vec<&str> = still_held.iter().map(String::as_str).collect();
            assert_eq!(
                held_now, expected,
                "case {case:?}: after taking {association_id} away, expected {expected:?} \
                 still on the table, got {held_now:?} in {read}"
            );
            for (id, _, state) in &explicit {
                assert_eq!(
                    state, ASSOCIATED,
                    "case {case:?}: expected {id} still reported {ASSOCIATED:?}, got \
                     {state:?} in {read}"
                );
            }
        }
    }
}

/// A disassociate that is REFUSED reports no association state: nothing moved, so there is
/// no move to report.
///
/// The rows differ in WHY the id names no explicit association a caller can take away — one
/// that never existed, one already taken away, and the implicit association of a VPC's main
/// table, which is not an association a caller made. The refusal's own wording is each
/// operation's own business and is not asserted here; what is asserted is that a body
/// carrying no 200 carries no state either.
#[tokio::test]
async fn a_refused_disassociate_reports_no_association_state() {
    let router = emulator();
    let vpc_id = create_vpc("setup", &router, "10.94.0.0/16").await;
    let route_table_id = create_route_table("setup", &router, &vpc_id).await;
    let subnet_id = create_subnet("setup", &router, &vpc_id, "10.94.1.0/24").await;
    let associated = ok(
        "setup",
        &router,
        "AssociateRouteTable",
        &[
            param("RouteTableId", &route_table_id),
            param("SubnetId", &subnet_id),
        ],
    )
    .await;
    let association_id = element(&associated, "associationId")
        .unwrap_or_else(|| panic!("setup: no <associationId> in {associated}"));
    ok(
        "setup",
        &router,
        "DisassociateRouteTable",
        &[param("AssociationId", &association_id)],
    )
    .await;

    // The implicit association the VPC's main table is reported with, read back off the
    // describe rather than guessed.
    let all_tables = ok(
        "setup",
        &router,
        "DescribeRouteTables",
        &[param("Filter.1.Name", "vpc-id"), param("Filter.1.Value.1", &vpc_id)],
    )
    .await;
    let main_association_id = reported_associations("setup", &all_tables)
        .into_iter()
        .find(|(_, main, _)| *main)
        .map(|(id, _, _)| id)
        .unwrap_or_else(|| panic!("setup: no main association in {all_tables}"));

    let cases: [(&str, &str); 3] = [
        // A well-formed id in the association id space no association of this account
        // carries.
        ("an id nothing carries", "rtbassoc-0a1b2c3d4e5f6a7b8"),
        ("an id already taken away", &association_id),
        ("the implicit main association", &main_association_id),
    ];

    for (case, candidate) in cases {
        let (status, body) = call(
            &router,
            "DisassociateRouteTable",
            &[param("AssociationId", candidate)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the disassociate of {candidate} refused, got {status} \
             with body {body}"
        );
        assert!(
            association_state(case, &body).is_none(),
            "case {case:?}: expected a refusal to report no association state, got {body}"
        );
        assert!(
            elements(&body, "associationState").is_empty(),
            "case {case:?}: expected no associationState element at all, got {body}"
        );
    }

    // Nothing above moved anything: the table the refusals named still carries no explicit
    // association, and the main table still carries its implicit one.
    let read = describe_route_table("after the refusals", &router, &route_table_id).await;
    let reported = reported_associations("after the refusals", &read);
    assert!(
        reported.iter().all(|(_, main, _)| *main),
        "expected no explicit association left after every refusal, got {reported:?} in {read}"
    );
    let all_tables = ok(
        "after the refusals",
        &router,
        "DescribeRouteTables",
        &[param("Filter.1.Name", "vpc-id"), param("Filter.1.Value.1", &vpc_id)],
    )
    .await;
    let main_now = reported_associations("after the refusals", &all_tables)
        .into_iter()
        .find(|(id, _, _)| *id == main_association_id);
    assert_eq!(
        main_now.as_ref().map(|(_, main, state)| (*main, state.as_str())),
        Some((true, ASSOCIATED)),
        "expected the main table's implicit association untouched, got {main_now:?} in \
         {all_tables}"
    );
}
