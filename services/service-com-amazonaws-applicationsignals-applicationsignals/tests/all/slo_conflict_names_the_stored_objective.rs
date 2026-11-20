// SPDX-License-Identifier: CC0-1.0

//! How this service words a refusal ABOUT ONE PARTICULAR service level objective, over both
//! ways a name can be wrong.
//!
//! The rule under test: the two ways a name can be wrong are worded by two DIFFERENT error
//! classes, and each states its own class's sentence:
//!
//! * a create under a name the store ALREADY HOLDS states the FIXED sentence for an identifier
//!   that is already taken, which names neither the resource nor the identifier;
//! * a read of a name the store does NOT hold states the sentence this service words a refusal
//!   ABOUT ONE PARTICULAR objective with — the capitalised type name, the ARN that addresses
//!   the objective in single quotes and the predicate for a resource that is not there, closed
//!   by a period.
//!
//! The refused create writes nothing, which the round trip below checks by reading the stored
//! record back unchanged through the ARN the admitted create returned.
//!
//! The rule is asserted over a TABLE spanning what it generalises over: the name's form (the
//! character classes the modelled `ServiceLevelObjectiveName` pattern
//! `^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$` admits, including the shortest name and
//! one of the maximum length), which service level indicator the STORED record carries, which
//! one the REFUSED create carries, and what else that create sends. The admitted side is
//! driven too: a free name is still created on the same store.

use serde_json::{json, Value};

use crate::refusal_prose::SAME_IDENTIFIER_ALREADY_EXISTS;
use crate::resource_tags::{slo_arn, SLO_RESOURCE_TYPE};
use crate::slo_evaluation_kind::request_based_sli_config;
use crate::slo_lifecycle::{call, create, name_path_segment, router, sli_config};

/// One row: how the case is named, the name both creates carry, the body the FIRST create
/// stores and the body the refused create sends.
struct Row {
    case: &'static str,
    name: String,
    stored: Value,
    refused: Value,
}

/// The rows, spanning the name forms the modelled pattern admits crossed with the two service
/// level indicators a create may define, in both orders.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a hyphenated name, the same period-based body twice",
            name: "held-slo".to_string(),
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        },
        Row {
            case: "a dotted name, a request-based create over a period-based record",
            name: "held.slo.dotted".to_string(),
            stored: json!({ "SliConfig": sli_config("orders", "POST /orders", 9.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("orders") }),
        },
        Row {
            case: "an underscored name, a period-based create over a request-based record",
            name: "held_slo_underscored".to_string(),
            stored: json!({ "RequestBasedSliConfig": request_based_sli_config("payments") }),
            refused: json!({ "SliConfig": sli_config("payments", "GET /pay", 1.0) }),
        },
        Row {
            case: "a name carrying a space, with other members along",
            name: "held slo spaced".to_string(),
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
            name: "9z".to_string(),
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("x") }),
        },
        Row {
            case: "a name of the maximum length the pattern admits",
            name: format!("a{}z", "b".repeat(126)),
            stored: json!({ "RequestBasedSliConfig": request_based_sli_config("checkout") }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("checkout") }),
        },
    ]
}

/// The sentence a read of an SLO this service does NOT hold is refused with: the type name, the
/// ARN in single quotes and the predicate for a resource that is not there — the sentence this
/// service words every refusal ABOUT ONE PARTICULAR objective with, which the conflict class
/// does not share ([`crate::refusal_prose::SAME_IDENTIFIER_ALREADY_EXISTS`]).
fn name_not_held(arn: &str) -> String {
    format!("{SLO_RESOURCE_TYPE} with arn '{arn}' does not exist.")
}

/// RULE: the conflict a taken name earns states the fixed sentence of its own error class,
/// naming neither the objective that holds the name nor the name itself.
#[tokio::test]
async fn a_taken_name_is_refused_with_the_taken_identifier_sentence() {
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
            "case {}: expected the stored record to carry the minted ARN, got {stored_arn:?}",
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

        // The two classes are what tell the two refusals apart: this one states no ARN at
        // all, where a name the store does not hold is refused about one particular
        // objective.
        assert_ne!(
            message,
            name_not_held(&stored_arn),
            "case {}: expected the taken-identifier sentence, got {message:?}",
            row.case
        );

        // Round trip: the refused create wrote nothing, and the ARN its refusal stated is the
        // one that addresses the stored record.
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

        // The admitted side, on the same store: a FREE name is still created, so the refusal
        // is about the name that was held rather than about the request.
        let mut free = row.refused.clone();
        free["Name"] = json!("free-slo");
        let (status, admitted) = create(&router, free).await;
        assert_eq!(
            status, 200,
            "case {}: expected a free name to be created, got {status} with {admitted}",
            row.case
        );
    }
}

/// RULE, the other class: a read of a name the store does not hold states the ARN the
/// request addressed and the predicate for a resource that is not there — so a caller learns
/// which objective is meant, where the conflict class states no identifier.
///
/// The rows are the same name forms, none of them created, so what differs from the test above
/// is only whether the store holds the name.
#[tokio::test]
async fn a_name_the_store_does_not_hold_is_refused_about_that_objective() {
    let router = router();
    for row in rows() {
        let arn = slo_arn(&row.name);
        let (status, answer) = call(
            &router,
            "GET",
            &format!("/slo/{}", name_path_segment(&row.name)),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {}: expected HTTP 404 for a name nothing holds, got {status} with {answer}",
            row.case
        );
        let message = answer["Message"].as_str().unwrap_or_default().to_string();
        let expected = name_not_held(&arn);
        assert_eq!(
            message, expected,
            "case {}: expected {expected:?}, got {message:?}",
            row.case
        );
        assert_ne!(
            message, SAME_IDENTIFIER_ALREADY_EXISTS,
            "case {}: expected the sentence for a resource that is not there, got {message:?}",
            row.case
        );
    }
}
