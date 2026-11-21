// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FAN-OUT of AppStream's application/fleet associations over
//! the wire.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * the relation an `AssociateApplicationFleet` records is many-to-many: one call records
//!   exactly the `(FleetName, ApplicationArn)` edge it names and no other, so an Elastic
//!   fleet streams every application associated to it and an application streams on every
//!   fleet it is associated to;
//! * a read of one endpoint reports exactly that endpoint's edges, in the order the store
//!   holds them — by application ARN for a fleet's read, by fleet name for an
//!   application's;
//! * the read pages those edges: following `NextToken` reports every one of them exactly
//!   once, a page carries at most the `MaxResults` asked for and never more than the
//!   modelled list admits, and a `MaxResults` below the published floor is refused;
//! * `DisassociateApplicationFleet` removes exactly the one edge it names, leaving every
//!   other edge of both of its endpoints reported.

use serde_json::{json, Value};

use crate::application_fleet_associations::{
    create_app_block, create_application, create_fleet, ELASTIC,
};
use crate::refusal_messages::refusal;
use crate::stack_fleet_lifecycle::Emulator;

/// The Region every case below signs for.
const REGION: &str = "us-east-1";

/// The one platform every fleet runs and every application supports here, so the only
/// thing deciding an association is which pair the request names. A `PlatformType` the
/// model admits (`smithy_query(query="shape", shape="PlatformType")`).
const PLATFORM: &str = "AMAZON_LINUX2";

/// How many associations one page of `DescribeApplicationFleetAssociations` can carry at
/// most: the model caps `ApplicationFleetAssociationList` at 25 members
/// (`smithy_query(query="shape", shape="ApplicationFleetAssociationList")`), and that list
/// is what a page is rendered into.
const PAGE_CEILING: usize = 25;

/// One association, as both the request and the read render it.
fn edge(fleet_name: &str, application_arn: &str) -> Value {
    json!({ "FleetName": fleet_name, "ApplicationArn": application_arn })
}

/// Walk every association of one endpoint, a page at a time: the associations in the order
/// they were reported, and the size of each page it took.
///
/// `max_results` is the `MaxResults` each call asks for; `None` leaves the member off, so
/// the read sizes the page itself.
async fn walk(
    emulator: &Emulator,
    case: &str,
    selector: &Value,
    max_results: Option<i64>,
) -> (Vec<Value>, Vec<usize>) {
    let mut reported = Vec::new();
    let mut page_sizes = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = selector.clone();
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        if let Some(token) = &token {
            request["NextToken"] = json!(token);
        }
        let payload = emulator
            .ok(case, "DescribeApplicationFleetAssociations", request)
            .await;
        let page: Vec<Value> = payload
            .get("ApplicationFleetAssociations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        page_sizes.push(page.len());
        reported.extend(page);
        token = payload
            .get("NextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if token.is_none() {
            return (reported, page_sizes);
        }
        assert!(
            page_sizes.len() <= 2 * PAGE_CEILING * PAGE_CEILING,
            "case {case:?}: following the token must reach a last page, got page sizes \
             {page_sizes:?}"
        );
    }
}

/// Every association of one endpoint, read with whatever page size the read chooses.
async fn associated(emulator: &Emulator, case: &str, selector: &Value) -> Vec<Value> {
    walk(emulator, case, selector, None).await.0
}

/// One row of the fan-out table: how many Elastic fleets and how many applications the
/// case associates with each other.
struct Grid {
    case: &'static str,
    fleets: usize,
    applications: usize,
}

/// The fan-out table. The rows differ along what the rule generalises over: how many
/// fleets and how many applications are on the two sides of the relation — one each, one
/// application on several fleets, several applications on one fleet, and several of both,
/// which is the case neither endpoint's read may report the other's edges in.
fn grids() -> Vec<Grid> {
    vec![
        Grid {
            case: "one fleet streaming one application",
            fleets: 1,
            applications: 1,
        },
        Grid {
            case: "one application streaming on three fleets",
            fleets: 3,
            applications: 1,
        },
        Grid {
            case: "three applications streaming on one fleet",
            fleets: 1,
            applications: 3,
        },
        Grid {
            case: "two fleets streaming four applications each",
            fleets: 2,
            applications: 4,
        },
    ]
}

#[tokio::test]
async fn an_association_is_one_edge_of_a_many_to_many_relation() {
    for grid in grids() {
        let case = grid.case;
        let emulator = Emulator::in_region(REGION);
        let app_block_arn = create_app_block(&emulator, case).await;

        // Zero-padded names, so the ARN order a read reports in is the order these were
        // created in and the expectations below can be written down.
        let mut arns = Vec::new();
        for index in 0..grid.applications {
            arns.push(
                create_application(
                    &emulator,
                    case,
                    &format!("fan-app-{index:02}"),
                    &[PLATFORM],
                    &app_block_arn,
                )
                .await,
            );
        }
        let fleets: Vec<String> = (0..grid.fleets)
            .map(|index| format!("fan-fleet-{index:02}"))
            .collect();
        for fleet in &fleets {
            create_fleet(&emulator, case, fleet, ELASTIC, PLATFORM).await;
        }

        // Every pair associates, and each call records exactly the edge it names: after
        // the call the fleet reports the applications associated to it so far and nothing
        // else.
        for fleet in &fleets {
            for (associated_so_far, arn) in arns.iter().enumerate() {
                let answer = emulator
                    .ok(case, "AssociateApplicationFleet", edge(fleet, arn))
                    .await;
                assert_eq!(
                    answer,
                    json!({ "ApplicationFleetAssociation": edge(fleet, arn) }),
                    "case {case:?}: the result must carry the edge that was recorded, got \
                     {answer}"
                );
                let reported = associated(&emulator, case, &json!({ "FleetName": fleet })).await;
                let expected: Vec<Value> = arns[..=associated_so_far]
                    .iter()
                    .map(|arn| edge(fleet, arn))
                    .collect();
                assert_eq!(
                    reported, expected,
                    "case {case:?}: {fleet} must report only the applications associated \
                     to it, got {reported:?}"
                );
            }
        }

        // Each endpoint's read reports that endpoint's edges, in the order the store holds
        // them: by application ARN for a fleet, by fleet name for an application.
        for fleet in &fleets {
            let reported = associated(&emulator, case, &json!({ "FleetName": fleet })).await;
            let expected: Vec<Value> = arns.iter().map(|arn| edge(fleet, arn)).collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: {fleet} must report every application it streams in ARN \
                 order, got {reported:?}"
            );
        }
        for arn in &arns {
            let reported = associated(&emulator, case, &json!({ "ApplicationArn": arn })).await;
            let expected: Vec<Value> = fleets.iter().map(|fleet| edge(fleet, arn)).collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: {arn} must report every fleet it streams on in name order, \
                 got {reported:?}"
            );
        }

        // Whatever page size a caller asks for, following the token reports each of an
        // endpoint's edges exactly once and no page carries more than was asked for.
        for fleet in &fleets {
            let selector = json!({ "FleetName": fleet });
            let expected: Vec<Value> = arns.iter().map(|arn| edge(fleet, arn)).collect();
            for max_results in 1..=(arns.len() as i64 + 1) {
                let (reported, page_sizes) =
                    walk(&emulator, case, &selector, Some(max_results)).await;
                assert_eq!(
                    reported, expected,
                    "case {case:?}: paging {fleet} at MaxResults={max_results} must report \
                     every edge exactly once, got {reported:?}"
                );
                let asked = usize::try_from(max_results).expect("a positive page size");
                assert!(
                    page_sizes.iter().all(|size| *size <= asked.min(PAGE_CEILING)),
                    "case {case:?}: no page of {fleet} may exceed MaxResults={max_results}, \
                     got {page_sizes:?}"
                );
            }
        }

        // One edge removed is one edge gone: every other edge of both of its endpoints is
        // still reported.
        let removed_fleet = fleets[fleets.len() - 1].clone();
        let removed_arn = arns[arns.len() - 1].clone();
        emulator
            .ok(
                case,
                "DisassociateApplicationFleet",
                edge(&removed_fleet, &removed_arn),
            )
            .await;
        for fleet in &fleets {
            let reported = associated(&emulator, case, &json!({ "FleetName": fleet })).await;
            let expected: Vec<Value> = arns
                .iter()
                .filter(|arn| !(*fleet == removed_fleet && **arn == removed_arn))
                .map(|arn| edge(fleet, arn))
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: clearing one edge must leave the others on {fleet}, got \
                 {reported:?}"
            );
        }
        for arn in &arns {
            let reported = associated(&emulator, case, &json!({ "ApplicationArn": arn })).await;
            let expected: Vec<Value> = fleets
                .iter()
                .filter(|fleet| !(**fleet == removed_fleet && *arn == removed_arn))
                .map(|fleet| edge(fleet, arn))
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: clearing one edge must leave the others on {arn}, got \
                 {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_of_associations_holds_at_most_what_the_model_admits() {
    // One Elastic fleet streaming one association more than a page can carry, so the
    // ceiling the model sets is what cuts the page rather than the number of edges.
    let case = "more associations than one page admits";
    let emulator = Emulator::in_region(REGION);
    let app_block_arn = create_app_block(&emulator, case).await;
    create_fleet(&emulator, case, "ceiling-fleet", ELASTIC, PLATFORM).await;

    let mut arns = Vec::new();
    for index in 0..=PAGE_CEILING {
        let arn = create_application(
            &emulator,
            case,
            &format!("ceiling-app-{index:02}"),
            &[PLATFORM],
            &app_block_arn,
        )
        .await;
        emulator
            .ok(case, "AssociateApplicationFleet", edge("ceiling-fleet", &arn))
            .await;
        arns.push(arn);
    }

    let selector = json!({ "FleetName": "ceiling-fleet" });
    let expected: Vec<Value> = arns.iter().map(|arn| edge("ceiling-fleet", arn)).collect();

    // Rows: each `MaxResults` the read is asked for, and the pages it must answer with —
    // the published ceiling, one below it, one above it, far above it, the smallest page a
    // read can answer, and no ask at all.
    let full_pages = vec![PAGE_CEILING, arns.len() - PAGE_CEILING];
    for (max_results, pages) in [
        (None, full_pages.clone()),
        (Some(PAGE_CEILING as i64), full_pages.clone()),
        (Some(PAGE_CEILING as i64 + 1), full_pages.clone()),
        (Some(1_000), full_pages.clone()),
        (
            Some(PAGE_CEILING as i64 - 1),
            vec![PAGE_CEILING - 1, arns.len() - (PAGE_CEILING - 1)],
        ),
        (Some(1), vec![1; arns.len()]),
    ] {
        let (reported, page_sizes) = walk(&emulator, case, &selector, max_results).await;
        assert_eq!(
            reported, expected,
            "case {case:?} (MaxResults={max_results:?}): every edge must be reported \
             exactly once, got {reported:?}"
        );
        assert_eq!(
            page_sizes, pages,
            "case {case:?} (MaxResults={max_results:?}): unexpected page sizes \
             {page_sizes:?}"
        );
    }

    // "The maximum size of each page of results": a page of fewer than one result is not
    // a size the read can answer with, so the asks below the published floor are refused
    // and report nothing.
    for max_results in [0, -1] {
        let message = refusal(
            &emulator,
            case,
            "DescribeApplicationFleetAssociations",
            json!({ "FleetName": "ceiling-fleet", "MaxResults": max_results }),
            400,
            "ValidationException",
        )
        .await;
        assert_eq!(
            message,
            format!("MaxResults must be at least 1, was {max_results}"),
            "case {case:?} (MaxResults={max_results}): the refusal must state the floor, \
             got {message:?}"
        );
    }
}
