// SPDX-License-Identifier: CC0-1.0

//! The sentence this service answers with when a purchase names an offering the
//! catalog does not sell.
//!
//! The rule: `CreateSavingsPlan` answers a `savingsPlanOfferingId` the catalog
//! holds no offering for with a 404 `ResourceNotFoundException` whose message is
//! the FIXED sentence [`state::OFFERING_NOT_FOUND_MESSAGE`] — the remediation
//! hint included, and nothing taken from the request: the submitted offering id
//! is never interpolated into it, so the sentence is the same for every id the
//! catalog does not sell.
//!
//! The table spans what the rule generalises over — what the unknown id looks
//! like (a UUID the catalog never minted, a near miss on one it did, a plan id
//! where an offering id belongs, an ARN, a long opaque value, a word that is no
//! id at all) — and each row is checked against the whole sentence, not a
//! fragment of it. Everything goes through the same `MultiServiceRouter` the
//! emulator boots, reusing the harness in [`crate::savings_plans_lifecycle`].

use serde_json::json;
use service_com_amazonaws_savingsplans_awssavingsplan::state;

use crate::savings_plans_lifecycle::{text, Emulator, ONE_YEAR};

/// A purchase that names an offering the catalog does not sell is refused with
/// one fixed sentence, whatever the id it named looks like, and buys nothing.
#[tokio::test]
async fn an_offering_the_catalog_does_not_sell_is_refused_with_one_sentence() {
    let emulator = Emulator::new();
    let sold = emulator.offering("Compute", "No Upfront", ONE_YEAR).await;
    let sold_id = text(&sold, "offeringId");
    // One plan this account really holds, so the rows below are refused for the
    // offering they name rather than because nothing can be bought.
    let mine = emulator
        .purchase(json!({
            "savingsPlanOfferingId": sold_id,
            "commitment": "0.1",
        }))
        .await;
    let arn = text(&emulator.plan(&mine).await, "savingsPlanArn");

    let unknown_ids: [(&str, String); 6] = [
        (
            "a UUID the catalog never minted",
            "11111111-1111-1111-8111-111111111111".to_string(),
        ),
        ("a near miss on one it did", format!("{sold_id}0")),
        ("the same id upper-cased", sold_id.to_uppercase()),
        ("a plan id where an offering id belongs", mine.clone()),
        ("an ARN", arn.clone()),
        ("not an id at all", "not-an-offering".to_string()),
    ];

    for (case, offering_id) in &unknown_ids {
        assert_ne!(
            offering_id, &sold_id,
            "case {case:?}: the row must name an offering the catalog does not sell"
        );
        let answer = emulator
            .call(
                "CreateSavingsPlan",
                json!({ "savingsPlanOfferingId": offering_id, "commitment": "0.1" }),
            )
            .await;
        assert_eq!(
            (answer.status, answer.code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected a 404 ResourceNotFoundException, got {} {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            text(&answer.body, "message"),
            state::OFFERING_NOT_FOUND_MESSAGE,
            "case {case:?}: expected {:?}, got {:?}",
            state::OFFERING_NOT_FOUND_MESSAGE,
            text(&answer.body, "message")
        );
        assert!(
            !text(&answer.body, "message").contains(offering_id.as_str()),
            "case {case:?}: the sentence has no variable parts, so it must not echo the id, \
             got {:?}",
            text(&answer.body, "message")
        );
    }

    // And no refusal bought anything: the account still holds the one plan it
    // bought from the offering the catalog does sell.
    let answer = emulator.call("DescribeSavingsPlans", json!({})).await;
    let plans = answer.list("savingsPlans");
    assert_eq!(
        plans.len(),
        1,
        "a refused purchase must buy nothing, got {} plans",
        plans.len()
    );
    assert_eq!(
        text(&plans[0], "savingsPlanId"),
        mine,
        "the one plan held must be the one that was bought, got {:?}",
        plans[0]
    );
}
