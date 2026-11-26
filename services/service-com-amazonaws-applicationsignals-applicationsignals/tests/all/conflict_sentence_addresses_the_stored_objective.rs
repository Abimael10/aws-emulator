// SPDX-License-Identifier: CC0-1.0

//! The sentence a 409 `ConflictException` states, against the sentence the 404 of the SAME
//! objective states.
//!
//! RULE: a create refused because the identifier it sent is already taken states the FIXED
//! sentence of that conflict class — `A resource with the same identifier already exists`, no
//! variable part, no trailing period — while a refusal this service words about ONE
//! PARTICULAR objective the caller addressed (a read of an objective nothing holds) addresses
//! that objective by its ARN. `ConflictException` carries one `Message` member and the model
//! states no text for it, so the wording IS the contract a client reads; the conflict states
//! no identifier, so nothing of the refused request and nothing of the record it collided
//! with reaches it.
//!
//! The rows span what the rule generalises over: the form the objective's `Name` takes (the
//! character classes the modelled `ServiceLevelObjectiveName` pattern
//! `^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$` admits, including the shortest name and
//! one of the maximum length), which service level indicator the STORED objective and the
//! REFUSED create each define, and what else the refused create sends — none of which the
//! sentence may depend on.
//!
//! The harness is [`crate::slo_lifecycle`]'s and the two sentences are read through
//! [`crate::refusal_prose`]'s own spellings of them, each reused rather than copied.

use serde_json::{json, Value};

use crate::refusal_prose::SAME_IDENTIFIER_ALREADY_EXISTS;
use crate::resource_tags::slo_arn;
use crate::slo_evaluation_kind::request_based_sli_config;
use crate::slo_lifecycle::{call, create, name_path_segment, router, sli_config};

/// One row: the name both creates carry, the objective the store holds under it, and the
/// create that is refused.
struct Row {
    case: &'static str,
    name: String,
    stored: Value,
    refused: Value,
}

/// The rows, spanning the name forms the modelled pattern admits crossed with the two
/// service level indicators a create may define and the other members it may send.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a hyphenated name, the same period-based create twice",
            name: "conflict-sentence-slo".to_string(),
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        },
        Row {
            case: "a dotted name, a request-based create over a period-based objective",
            name: "conflict.sentence.slo".to_string(),
            stored: json!({ "SliConfig": sli_config("orders", "POST /orders", 9.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("orders") }),
        },
        Row {
            case: "an underscored name, a period-based create over a request-based objective",
            name: "conflict_sentence_slo".to_string(),
            stored: json!({ "RequestBasedSliConfig": request_based_sli_config("payments") }),
            refused: json!({ "SliConfig": sli_config("payments", "GET /pay", 1.0) }),
        },
        Row {
            case: "a name carrying a space, with other members along",
            name: "conflict sentence slo".to_string(),
            stored: json!({ "SliConfig": sli_config("carts", "GET /c", 1.0) }),
            refused: json!({
                "SliConfig": sli_config("carts", "GET /c", 42.0),
                "Description": "a second attempt",
                "Goal": { "AttainmentGoal": 95.0 },
                "Tags": [{ "Key": "team", "Value": "carts" }],
            }),
        },
        Row {
            case: "the shortest name the pattern admits",
            name: "7q".to_string(),
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("checkout") }),
        },
        Row {
            case: "a name of the maximum length the pattern admits",
            name: format!("c{}z", "o".repeat(126)),
            stored: json!({ "SliConfig": sli_config("long", "GET /l", 2.0) }),
            refused: json!({ "SliConfig": sli_config("long", "GET /l", 3.0) }),
        },
    ]
}

/// The 409 a taken `Name` earns states the conflict class's fixed sentence, naming no
/// identifier at all — and the 404 a name nothing holds earns names the objective it
/// addressed by its ARN.
#[tokio::test]
async fn a_conflict_states_no_identifier_while_a_not_found_states_one() {
    for row in rows() {
        let router = router();
        let mut stored = row.stored.clone();
        stored["Name"] = json!(row.name);
        let (status, created) = create(&router, stored).await;
        assert_eq!(
            status, 200,
            "case {}: expected the first create to succeed, got {status} with {created}",
            row.case
        );
        let stored_arn = created["Slo"]["Arn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            stored_arn,
            slo_arn(&row.name),
            "case {}: expected the stored objective to carry the ARN of its own name, got \
             {stored_arn:?}",
            row.case
        );

        let mut refused = row.refused.clone();
        refused["Name"] = json!(row.name);
        let (status, answer) = create(&router, refused).await;
        assert_eq!(
            status, 409,
            "case {}: expected HTTP 409 for a name already held, got {status} with {answer}",
            row.case
        );
        assert_eq!(
            answer["__type"].as_str(),
            Some("ConflictException"),
            "case {}: expected a ConflictException, got {answer}",
            row.case
        );
        let message = answer["Message"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            message, SAME_IDENTIFIER_ALREADY_EXISTS,
            "case {}: expected {SAME_IDENTIFIER_ALREADY_EXISTS:?}, got {message:?}",
            row.case
        );
        // There is no variable part: neither the ARN of the record the identifier collided
        // with nor anything of the refused create's own body reaches the sentence.
        assert!(
            !message.contains(&stored_arn) && !message.contains("a second attempt"),
            "case {}: expected a sentence naming neither {stored_arn:?} nor the refused \
             body, got {message:?}",
            row.case
        );

        // The other refusal class, the one worded about one particular objective: a read of a
        // name nothing holds, which DOES state the ARN it addressed. That difference is what
        // tells a client which of the two happened.
        let (status, missing) = call(
            &router,
            "GET",
            &format!("/slo/{}", name_path_segment("free-conflict-sentence-slo")),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {}: expected HTTP 404 for a name nothing holds, got {status} with {missing}",
            row.case
        );
        let not_held = missing["Message"].as_str().unwrap_or_default().to_string();
        let free_arn = slo_arn("free-conflict-sentence-slo");
        assert_eq!(
            not_held,
            format!("Slo with arn '{free_arn}' does not exist."),
            "case {}: expected the not-found predicate about {free_arn:?}, got {not_held:?}",
            row.case
        );

        // Round trip: the refused create wrote nothing, so the record the identifier collided
        // with is still the one the admitted create stored.
        let (status, fetched) = call(
            &router,
            "GET",
            &format!("/slo/{}", name_path_segment(&row.name)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: expected the stored SLO to still be readable, got {status} with {fetched}",
            row.case
        );
        assert_eq!(
            fetched["Slo"], created["Slo"],
            "case {}: expected a refused create to leave the record untouched, got {:?}",
            row.case, fetched["Slo"]
        );
    }
}
