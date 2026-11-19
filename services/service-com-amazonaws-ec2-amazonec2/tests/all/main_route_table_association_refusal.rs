// SPDX-License-Identifier: CC0-1.0

//! What `DisassociateRouteTable` does with an association id that RESOLVES to the implicit
//! association of a VPC's main route table, driven over the wire the service speaks:
//! `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **an association id the service resolves is never reported as an id
//! nothing carries** — a VPC's main route-table association is a record the describe reports
//! (with `main` set), and it is not one a caller may take away, because every subnet with no
//! association of its own uses it; so the disassociate of one is refused with
//! `InvalidParameterValue` and the sentence that names the association the request asked
//! about, while the SAME call on an explicit association an `AssociateRouteTable` made
//! succeeds, and an id no stored association carries at all stays
//! `InvalidAssociationID.NotFound`.
//!
//! Every row is a distinct input along what that rule generalises over: which main
//! association is named (the one a VPC comes with, the one a `ReplaceRouteTableAssociation`
//! moved to another table, and a second VPC's), how many explicit associations the VPC's
//! tables carry beside it, and the three resolutions an id can have — main, explicit, and
//! nothing.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "eu-west-1";

/// The code EC2 refuses a parameter that cannot take the value the request gave it with, as
/// this crate names it for every such refusal.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The code for an association id no stored association carries, as the crate names it
/// (`ec2::state::INVALID_ASSOCIATION_ID_NOT_FOUND`).
const NOT_FOUND: &str = ec2::state::INVALID_ASSOCIATION_ID_NOT_FOUND;

/// What the refusal of a main association must say, for the association `association_id`.
fn expected_message(association_id: &str) -> String {
    format!("cannot disassociate the main route table association {association_id}")
}

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

/// Every association the tables of `vpc_id` are reported with, as its id and whether it is
/// the implicit association of a main table — read off `DescribeRouteTables`, so what a row
/// names is what the service itself reports.
async fn reported_associations(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
) -> Vec<(String, bool)> {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[
            param("Filter.1.Name", "vpc-id"),
            param("Filter.1.Value.1", vpc_id),
        ],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "associationSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .map(|item| {
            (
                child_text(item, "routeTableAssociationId").unwrap_or_default(),
                child_text(item, "main").as_deref() == Some("true"),
            )
        })
        .collect()
}

/// The ONE main association of `vpc_id`, as the describe reports it.
async fn main_association(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let reported = reported_associations(case, router, vpc_id).await;
    let mains: Vec<String> = reported
        .iter()
        .filter(|(_, main)| *main)
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(
        mains.len(),
        1,
        "case {case:?}: a VPC has exactly one main route-table association, got {reported:?}"
    );
    mains.into_iter().next().unwrap_or_default()
}

/// The main association reported through the filter AWS documents for it
/// (`DescribeRouteTables` `association.main`), so a row names the id that very filter
/// answers with.
async fn main_association_by_filter(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
) -> String {
    let body = ok(
        case,
        router,
        "DescribeRouteTables",
        &[
            param("Filter.1.Name", "vpc-id"),
            param("Filter.1.Value.1", vpc_id),
            param("Filter.2.Name", "association.main"),
            param("Filter.2.Value.1", "true"),
        ],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "associationSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .filter(|item| child_text(*item, "main").as_deref() == Some("true"))
        .find_map(|item| child_text(item, "routeTableAssociationId"))
        .unwrap_or_else(|| panic!("case {case:?}: no main association in {body}"))
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

/// How the main association a row names came to be the main one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainAssociation {
    /// The implicit association of the table the VPC came with.
    AsCreated,
    /// The implicit association a `ReplaceRouteTableAssociation` moved to another table of
    /// the VPC, which is then the main one.
    Moved,
    /// The implicit association of the table the VPC came with, read through the
    /// `association.main` filter rather than off the whole set.
    ByFilter,
}

/// The main association of a VPC cannot be taken away: the disassociate of one is refused
/// with the code and the sentence that refusal carries, and nothing about the VPC's tables
/// moves — while the same call on an explicit association succeeds.
#[tokio::test]
async fn the_main_route_table_association_is_refused_rather_than_reported_unknown() {
    let router = emulator();
    // (case, the VPC's block, the subnet blocks given explicit associations, how the main
    // association a row names came to be the main one)
    let cases: [(&str, &str, &[&str], MainAssociation); 4] = [
        (
            "the table the VPC came with",
            "10.80.0.0/16",
            &[],
            MainAssociation::AsCreated,
        ),
        (
            "the table the VPC came with, beside one explicit association",
            "10.81.0.0/16",
            &["10.81.1.0/24"],
            MainAssociation::AsCreated,
        ),
        (
            "the main association moved to another table",
            "10.82.0.0/16",
            &["10.82.1.0/24", "10.82.2.0/24"],
            MainAssociation::Moved,
        ),
        (
            "the main association read through the association.main filter",
            "10.83.0.0/16",
            &["10.83.1.0/24"],
            MainAssociation::ByFilter,
        ),
    ];

    for (case, vpc_cidr, subnet_cidrs, which) in cases {
        let vpc_id = create_vpc(case, &router, vpc_cidr).await;
        let mut explicit = Vec::new();
        if !subnet_cidrs.is_empty() {
            let route_table_id = create_route_table(case, &router, &vpc_id).await;
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
                explicit.push(
                    element(&body, "associationId")
                        .unwrap_or_else(|| panic!("case {case:?}: no <associationId> in {body}")),
                );
            }
        }

        // "You can also use this operation to change which table is the main route table in
        // the VPC" (`ReplaceRouteTableAssociation`): a row that moves the main association
        // names the id of the association that replaced it, which is main in its turn.
        if which == MainAssociation::Moved {
            let moved_to = create_route_table(case, &router, &vpc_id).await;
            let standing_main = main_association(case, &router, &vpc_id).await;
            ok(
                case,
                &router,
                "ReplaceRouteTableAssociation",
                &[
                    param("AssociationId", &standing_main),
                    param("RouteTableId", &moved_to),
                ],
            )
            .await;
        }

        let main_association_id = match which {
            MainAssociation::ByFilter => main_association_by_filter(case, &router, &vpc_id).await,
            _ => main_association(case, &router, &vpc_id).await,
        };
        let before = reported_associations(case, &router, &vpc_id).await;

        let (status, body) = call(
            &router,
            "DisassociateRouteTable",
            &[param("AssociationId", &main_association_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the disassociate of the main association \
             {main_association_id} refused with HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message(&main_association_id),
            "case {case:?}: expected the sentence naming the association the request asked \
             about, got {body}"
        );

        // The refusal moved nothing: the same associations are reported, the main one among
        // them.
        let after = reported_associations(case, &router, &vpc_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refusal to leave the VPC's associations as they \
             were, got {after:?}"
        );
        assert!(
            after
                .iter()
                .any(|(id, main)| *id == main_association_id && *main),
            "case {case:?}: expected {main_association_id} still reported as the main \
             association, got {after:?}"
        );

        // The identical call on an explicit association an `AssociateRouteTable` made
        // succeeds, which is what makes the refusal above about WHICH association was named.
        for association_id in &explicit {
            let body = ok(
                case,
                &router,
                "DisassociateRouteTable",
                &[param("AssociationId", association_id)],
            )
            .await;
            let parsed = document(case, &body);
            let state = parsed
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "associationState")
                .and_then(|node| child_text(node, "state"));
            assert_eq!(
                state.as_deref(),
                Some("disassociating"),
                "case {case:?}: expected the explicit association {association_id} taken \
                 away, got {body}"
            );
        }

        // And the main association is STILL refused the same way after that, so the refusal
        // does not depend on what else the VPC's tables carry.
        let (status, body) = call(
            &router,
            "DisassociateRouteTable",
            &[param("AssociationId", &main_association_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the main association still refused, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE} after the explicit \
             associations were taken away, got {body}"
        );
    }
}

/// The three resolutions an association id can have are each reported as themselves: the
/// main association is refused for being main, an explicit one is taken away, and an id no
/// stored association carries — one that never existed, and one an earlier call took away —
/// is the not-found.
#[tokio::test]
async fn an_association_id_is_reported_by_what_it_resolves_to() {
    let router = emulator();
    let vpc_id = create_vpc("setup", &router, "10.84.0.0/16").await;
    let route_table_id = create_route_table("setup", &router, &vpc_id).await;
    let subnet_id = create_subnet("setup", &router, &vpc_id, "10.84.1.0/24").await;
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
    let explicit_id = element(&associated, "associationId")
        .unwrap_or_else(|| panic!("setup: no <associationId> in {associated}"));
    let main_association_id = main_association("setup", &router, &vpc_id).await;

    // A second VPC's main association: the refusal is a fact of the record, not of the one
    // VPC the rows above built.
    let other_vpc_id = create_vpc("setup", &router, "10.85.0.0/16").await;
    let other_main_id = main_association("setup", &router, &other_vpc_id).await;

    // An explicit association already taken away, so an id that once resolved and no longer
    // does is a row of its own.
    let second_route_table_id = create_route_table("setup", &router, &vpc_id).await;
    let second_subnet_id = create_subnet("setup", &router, &vpc_id, "10.84.2.0/24").await;
    let gone = ok(
        "setup",
        &router,
        "AssociateRouteTable",
        &[
            param("RouteTableId", &second_route_table_id),
            param("SubnetId", &second_subnet_id),
        ],
    )
    .await;
    let gone_id = element(&gone, "associationId")
        .unwrap_or_else(|| panic!("setup: no <associationId> in {gone}"));
    ok(
        "setup",
        &router,
        "DisassociateRouteTable",
        &[param("AssociationId", &gone_id)],
    )
    .await;

    // (case, the id, the status and code it must be answered with, and the message when the
    // refusal has a sentence of its own)
    let cases: [(&str, &str, u16, &str, Option<String>); 5] = [
        (
            "the main association of the VPC",
            &main_association_id,
            400,
            INVALID_PARAMETER_VALUE,
            Some(expected_message(&main_association_id)),
        ),
        (
            "the main association of another VPC",
            &other_main_id,
            400,
            INVALID_PARAMETER_VALUE,
            Some(expected_message(&other_main_id)),
        ),
        (
            "an id no association of the account carries",
            "rtbassoc-0f1e2d3c4b5a69788",
            400,
            NOT_FOUND,
            None,
        ),
        ("an id already taken away", &gone_id, 400, NOT_FOUND, None),
        (
            // The explicit association goes last: it is the one row that SUCCEEDS, and it
            // leaves nothing for the rows above to depend on.
            "an explicit association a caller made",
            &explicit_id,
            200,
            "",
            None,
        ),
    ];

    for (case, candidate, expected_status, expected_code, expected_message) in cases {
        let (status, body) = call(
            &router,
            "DisassociateRouteTable",
            &[param("AssociationId", candidate)],
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for {candidate}, got {status} \
             with body {body}"
        );
        if expected_status == 200 {
            let parsed = document(case, &body);
            let state = parsed
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "associationState")
                .and_then(|node| child_text(node, "state"));
            assert_eq!(
                state.as_deref(),
                Some("disassociating"),
                "case {case:?}: expected the association taken away, got {body}"
            );
            continue;
        }
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected the code {expected_code}, got {body}"
        );
        if let Some(expected) = expected_message {
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected the sentence {expected:?}, got {body}"
            );
        }
    }

    // Both main associations are still reported, in the state they were in: a refusal is not
    // a half-performed disassociate.
    for (case, vpc, expected_main) in [
        ("the VPC under test", &vpc_id, &main_association_id),
        ("the other VPC", &other_vpc_id, &other_main_id),
    ] {
        let reported = reported_associations(case, &router, vpc).await;
        assert!(
            reported
                .iter()
                .any(|(id, main)| id == expected_main && *main),
            "case {case:?}: expected {expected_main} still reported as the main \
             association, got {reported:?}"
        );
    }

    // The table the explicit association bound reports an `associationState` for nobody now:
    // the one row that succeeded is the one that changed the store.
    let body = ok(
        "after the rows",
        &router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", &route_table_id)],
    )
    .await;
    let parsed = document("after the rows", &body);
    let still_bound = parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "associationSet")
        .flat_map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .collect::<Vec<_>>()
        })
        .filter_map(|item| child(item, "associationState").map(|_| item))
        .count();
    assert_eq!(
        still_bound, 0,
        "expected the table to carry no association after the one that succeeded, got {body}"
    );
}
