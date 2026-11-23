// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHOSE mint a `ListRules` page cursor may name, when one
//! store holds the retention rules of several Regions at once.
//!
//! The rule asserted here: a page cursor is honoured only by the endpoint that issued
//! it and only for a position THAT endpoint handed out, so a cursor carrying the Region
//! the request is signed for but naming a position only another Region's endpoint minted
//! is refused with the modelled `ValidationException`, HTTP 400 and
//! `Reason=INVALID_PAGE_TOKEN` — never answered with a page that resumes "after" a
//! position this Region's pager never emitted, which would silently drop every rule of
//! it sorting at or before that position.
//!
//! The tables span the dimension the rule generalises over — WHO minted the position a
//! cursor names, and WHERE that position falls in the caller's own walk: the mints of
//! every other Region the store holds, interleaved with the caller's own so a foreign
//! position falls before, between and after them; crossed with the order the Regions
//! created their rules in, with the Region the request is signed for, and with whether
//! the rule that occupied the foreign position is still stored. The positive rows pin
//! what the refusal must not break: every position the calling Region itself minted
//! still resumes its walk right after it, and the Region's whole walk still reports
//! every rule it holds. A last test pins how far the record of retired positions
//! reaches: the store keeps a bounded number of them, the oldest is forgotten first,
//! and a cursor at a forgotten position is REFUSED rather than resumed.
//!
//! Every case drives the service through the real router, exactly as a signed SDK
//! request would, and states its own case so a failure names the input.

use crate::list_rules_page_token_authenticity::{
    assert_refused, create_rules_in, delete_rule_in_expecting_no_content, page_in, REGIONS,
};
use crate::retention_rule_lifecycle::{router, RESOURCE_TYPES};
use service_com_amazonaws_rbin_amazonrecyclebin::state::{
    encode_page_token, RETIRED_POSITION_MEMORY,
};
use smithy_server_runtime::MultiServiceRouter;

/// The orders the Regions of [`REGIONS`] create their rules in, as indexes into it. Each
/// row is one interleaving of the shared identifier order the store mints positions from,
/// so across the rows a position another Region minted falls before, between and after
/// the positions the calling Region minted.
const CREATION_ORDERS: [&[usize]; 4] = [
    // Round robin: every Region's positions are interleaved with every other's.
    &[0, 1, 2, 0, 1, 2, 0, 1, 2],
    // One block per Region, in order: the first Region's positions all sort below the
    // others', the last Region's all above.
    &[0, 0, 0, 1, 1, 1, 2, 2, 2],
    // The same blocks, last Region first.
    &[2, 2, 2, 1, 1, 1, 0, 0, 0],
    // Uneven, so a Region's own positions are not evenly spaced in the shared order.
    &[1, 0, 1, 1, 2, 0, 2, 0, 0],
];

/// Create the rules one creation order names and hand back, per Region, the positions
/// that Region's endpoint minted, in creation order.
async fn create_in_order(
    router: &MultiServiceRouter,
    resource_type: &str,
    order: &[usize],
    case: &str,
) -> Vec<(&'static str, Vec<String>)> {
    let mut minted: Vec<(&'static str, Vec<String>)> =
        REGIONS.iter().map(|region| (*region, Vec::new())).collect();
    for index in order {
        let region = REGIONS[*index];
        let created = create_rules_in(router, region, resource_type, 1, case).await;
        for (held, identifiers) in minted.iter_mut() {
            if *held == region {
                identifiers.extend(created.iter().cloned());
            }
        }
    }
    minted
}

#[tokio::test]
async fn a_position_only_another_region_minted_is_no_cursor_of_this_regions_walk() {
    for (row, order) in CREATION_ORDERS.iter().enumerate() {
        let resource_type = RESOURCE_TYPES[row % RESOURCE_TYPES.len()];
        let router = router();
        let setup = format!("rules retaining {resource_type} created in the order {order:?}");
        let minted = create_in_order(&router, resource_type, order, &setup).await;

        for (region, own) in &minted {
            for (issuer, foreign) in &minted {
                if issuer == region {
                    continue;
                }
                for identifier in foreign {
                    assert!(
                        !own.contains(identifier),
                        "setup {setup:?}: {identifier:?} was minted by {issuer}, so {region} \
                         must not hold it, got {own:?}"
                    );
                    // A cursor carrying the CALLER's own Region, naming a position only
                    // another Region's endpoint handed out.
                    let case = format!(
                        "a cursor of {region} naming {identifier:?}, a position {issuer} \
                         minted, with {setup}"
                    );
                    assert_refused(
                        &router,
                        region,
                        resource_type,
                        &encode_page_token(region, identifier),
                        &case,
                    )
                    .await;
                }
            }

            // Every refusal left the walk alone: the Region still reports every rule it
            // holds, in creation order, and a page holding them all ends the walk.
            let case = format!("the whole walk of {region}, with {setup}");
            let (reported, token) =
                page_in(&router, region, resource_type, None, None, &case).await;
            assert_eq!(
                reported, *own,
                "case {case:?}: the walk must report every rule of the Region, got {reported:?}"
            );
            assert_eq!(
                token, None,
                "case {case:?}: a page holding every rule hands back no cursor, got {token:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_position_a_region_minted_still_resumes_its_own_walk() {
    // The other half of the rule: refusing another Region's positions must not cost a
    // Region one of its own, whichever order the Regions minted in.
    for (row, order) in CREATION_ORDERS.iter().enumerate() {
        let resource_type = RESOURCE_TYPES[row % RESOURCE_TYPES.len()];
        let router = router();
        let setup = format!("rules retaining {resource_type} created in the order {order:?}");
        let minted = create_in_order(&router, resource_type, order, &setup).await;

        for (region, own) in &minted {
            for (nth, identifier) in own.iter().enumerate() {
                let case = format!(
                    "a cursor of {region} at its own position #{nth} ({identifier:?}), \
                     with {setup}"
                );
                let (reported, _token) = page_in(
                    &router,
                    region,
                    resource_type,
                    Some(own.len() + 1),
                    Some(&encode_page_token(region, identifier)),
                    &case,
                )
                .await;
                assert_eq!(
                    reported,
                    own[nth + 1..].to_vec(),
                    "case {case:?}: the walk must resume right after the cursor and report \
                     the Region's own rules alone, got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn deleting_a_rule_changes_whose_position_its_identifier_is_for_no_region() {
    // A position is handed out once and for all: deleting the rule that occupied it
    // neither retracts it from the walk of the Region that minted it nor makes it a
    // position of any other Region's walk.
    let resource_type = RESOURCE_TYPES[0];
    let order = CREATION_ORDERS[0];
    let router = router();
    let setup = format!("rules retaining {resource_type} created in the order {order:?}");
    let minted = create_in_order(&router, resource_type, order, &setup).await;

    // Each Region deletes the first rule it holds, so every Region has a position whose
    // rule is gone.
    let mut deleted: Vec<(&'static str, String)> = Vec::new();
    for (region, own) in &minted {
        let identifier = own
            .first()
            .unwrap_or_else(|| panic!("setup {setup:?}: {region} must hold a rule"));
        delete_rule_in_expecting_no_content(&router, region, identifier, &setup).await;
        deleted.push((region, identifier.clone()));
    }

    for (issuer, identifier) in &deleted {
        // The Region that minted the position still resumes at it.
        let own = &minted
            .iter()
            .find(|(region, _)| region == issuer)
            .unwrap_or_else(|| panic!("setup {setup:?}: {issuer} must be one of the Regions"))
            .1;
        let case = format!(
            "a cursor of {issuer} at its own position {identifier:?}, whose rule is deleted, \
             with {setup}"
        );
        let (reported, _token) = page_in(
            &router,
            issuer,
            resource_type,
            Some(own.len() + 1),
            Some(&encode_page_token(issuer, identifier)),
            &case,
        )
        .await;
        assert_eq!(
            reported,
            own[1..].to_vec(),
            "case {case:?}: the issuing Region must still resume right after the position, \
             got {reported:?}"
        );

        // No other Region's endpoint ever handed it out, deleted or not.
        for (region, _) in &minted {
            if region == issuer {
                continue;
            }
            let case = format!(
                "a cursor of {region} naming {identifier:?}, a deleted position {issuer} \
                 minted, with {setup}"
            );
            assert_refused(
                &router,
                region,
                resource_type,
                &encode_page_token(region, identifier),
                &case,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn the_store_remembers_a_bounded_number_of_retired_positions() {
    // How far the record of retired positions reaches: the store keeps the most recent
    // `RETIRED_POSITION_MEMORY` of them, so once one more position than that has been
    // retired the OLDEST is forgotten and a cursor at it is refused rather than resumed,
    // while the newest retired position and the position a stored rule still occupies are
    // both honoured. Each round creates one rule and deletes it, so the store itself
    // never holds more than the two rules below.
    let region = REGIONS[0];
    let resource_type = RESOURCE_TYPES[0];
    let router = router();
    let setup = format!("one rule of {resource_type} kept in {region}, then one retired \
                         position more than the store remembers");
    let kept = create_rules_in(&router, region, resource_type, 1, &setup)
        .await
        .first()
        .unwrap_or_else(|| panic!("setup {setup:?}: the Region must hold the kept rule"))
        .clone();

    let mut oldest_retired = None;
    let mut newest_retired = String::new();
    for _ in 0..=RETIRED_POSITION_MEMORY {
        let identifier = create_rules_in(&router, region, resource_type, 1, &setup)
            .await
            .first()
            .unwrap_or_else(|| panic!("setup {setup:?}: each round must mint a position"))
            .clone();
        delete_rule_in_expecting_no_content(&router, region, &identifier, &setup).await;
        oldest_retired.get_or_insert(identifier.clone());
        newest_retired = identifier;
    }
    let oldest_retired = oldest_retired
        .unwrap_or_else(|| panic!("setup {setup:?}: a round must have retired a position"));
    // One rule minted AFTER every retired position, so a cursor that IS honoured resumes
    // on a non-empty tail and the page it answers is compared against a list rather than
    // against nothing.
    let tail = create_rules_in(&router, region, resource_type, 1, &setup)
        .await
        .first()
        .unwrap_or_else(|| panic!("setup {setup:?}: the Region must hold the tail rule"))
        .clone();

    let case = format!("a cursor at the oldest retired position {oldest_retired:?}, {setup}");
    assert_refused(
        &router,
        region,
        resource_type,
        &encode_page_token(region, &oldest_retired),
        &case,
    )
    .await;

    for (what, identifier) in [
        ("the newest retired position", &newest_retired),
        ("the position the kept rule still occupies", &kept),
    ] {
        let case = format!("a cursor at {what} ({identifier:?}), {setup}");
        let (reported, token) = page_in(
            &router,
            region,
            resource_type,
            None,
            Some(&encode_page_token(region, identifier)),
            &case,
        )
        .await;
        assert_eq!(
            reported,
            vec![tail.clone()],
            "case {case:?}: the walk must resume after the cursor and report the rules \
             minted after it, got {reported:?}"
        );
        assert_eq!(
            token, None,
            "case {case:?}: a page holding every rule after the cursor hands back no \
             cursor, got {token:?}"
        );
    }

    // Those two rules are the Region's whole walk: every round's rule was deleted.
    let case = format!("the whole walk of {region}, {setup}");
    let (reported, _token) = page_in(&router, region, resource_type, None, None, &case).await;
    assert_eq!(
        reported,
        vec![kept.clone(), tail.clone()],
        "case {case:?}: the walk must report the kept and the tail rule alone, got \
         {reported:?}"
    );
}
