// SPDX-License-Identifier: CC0-1.0

//! The INVARIANCE of the taken-name conflict: one sentence SHAPE for every refused create.
//!
//! The rule under test: a create whose `Name` is already stored is refused
//! `ConflictException` with this service's one sentence about the SLO it collided with — that
//! stored objective's ARN, and no other variable part. So the message a client reads carries
//! nothing of the REFUSED request: not its description, not its service level indicator, not
//! the goal or the tags it sent.
//!
//! Where [`super::refusal_prose`] pins the WORDING of that sentence, this suite pins its
//! invariance: a table of refused creates that differ in everything a conflict could have
//! been worded from must read the SAME sentence about the stored record, and nothing of the
//! refused body may appear in it. The rows differ along the name's form (the character classes
//! the modelled `ServiceLevelObjectiveName` pattern `^[0-9A-Za-z][-._0-9A-Za-z ]{0,126}[0-9A-Za-z]$`
//! admits, including its shortest name), along which SLI member the stored record and the
//! refused request carry, and along the rest of the refused body.
//!
//! The admitted side is driven too: the same store serves a create under a FREE name, so
//! what the rows assert is the identifier and not the request.

use serde_json::{json, Value};

use crate::refusal_prose::{assert_name_already_taken, same_identifier_already_exists};
use crate::slo_evaluation_kind::request_based_sli_config;
use crate::slo_lifecycle::{call, create, name_path_segment, router, sli_config};

/// One refused create: how the case is named, the name both creates carry, the body the
/// FIRST create stores, and the body the refused create sends.
struct Row {
    case: &'static str,
    name: &'static str,
    stored: Value,
    refused: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the same period-based request twice",
            name: "taken-slo",
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
        },
        Row {
            case: "a request-based config over a period-based record",
            name: "taken.slo.dotted",
            stored: json!({ "SliConfig": sli_config("orders", "POST /orders", 9.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("orders") }),
        },
        Row {
            case: "a period-based config over a request-based record",
            name: "taken_slo_underscored",
            stored: json!({ "RequestBasedSliConfig": request_based_sli_config("payments") }),
            refused: json!({ "SliConfig": sli_config("payments", "GET /pay", 1.0) }),
        },
        Row {
            case: "a name carrying a space, with other members along",
            name: "taken slo spaced",
            stored: json!({ "SliConfig": sli_config("carts", "GET /c", 1.0) }),
            refused: json!({
                "SliConfig": sli_config("carts", "GET /c", 42.0),
                "Description": "a different description",
                "Goal": { "AttainmentGoal": 95.0 },
                "Tags": [{ "Key": "team", "Value": "carts" }],
            }),
        },
        Row {
            case: "the shortest name the pattern admits",
            name: "9z",
            stored: json!({ "SliConfig": sli_config("checkout", "GET /cart", 100.0) }),
            refused: json!({ "RequestBasedSliConfig": request_based_sli_config("x") }),
        },
    ]
}

/// RULE: every refused create reads the one sentence of the conflict class about the stored
/// record, and nothing of the refused request reaches it.
#[tokio::test]
async fn every_taken_identifier_refusal_reads_one_sentence_about_the_stored_record() {
    let mut messages: Vec<(&str, &str, String)> = Vec::new();

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

        let mut refused = row.refused.clone();
        refused["Name"] = json!(row.name);
        let (status, answer) = create(&router, refused).await;
        assert_eq!(
            status, 409,
            "case {}: expected HTTP 409 for a taken identifier, got {status} with {answer}",
            row.case
        );
        assert_eq!(
            answer["__type"].as_str(),
            Some("ConflictException"),
            "case {}: expected a ConflictException, got {answer}",
            row.case
        );
        let message = answer["Message"].as_str().unwrap_or_default().to_string();
        assert_name_already_taken(row.case, row.name, &message);
        // Nothing of the REFUSED request is in the sentence; the stored record's own ARN is.
        for withheld in ["a different description", "95", "team"] {
            assert!(
                !message.contains(withheld),
                "case {}: expected a sentence carrying no {withheld:?}, got {message:?}",
                row.case
            );
        }
        assert!(
            message.contains(&stored_arn),
            "case {}: expected the stored record's ARN {stored_arn:?}, got {message:?}",
            row.case
        );
        messages.push((row.case, row.name, message));

        // The refused create wrote nothing: the record under that name is still the one the
        // admitted create stored, read back through the Get.
        let (status, fetched) = call(
            &router,
            "GET",
            &format!("/slo/{}", name_path_segment(row.name)),
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

        // The admitted side, on the same store: a FREE name is created, so the refusal above
        // is about the identifier rather than about the request.
        let mut free = row.refused.clone();
        free["Name"] = json!("free-slo");
        let (status, admitted) = create(&router, free).await;
        assert_eq!(
            status, 200,
            "case {}: expected a free name to be created, got {status} with {admitted}",
            row.case
        );
    }

    // One sentence over every row, byte for byte: this service's sentence about the stored
    // objective the create collided with.
    for (case, name, message) in &messages {
        let expected = same_identifier_already_exists(name);
        assert_eq!(
            message, &expected,
            "case {case}: expected every refused create to read {expected:?}, got {message:?}"
        );
    }
}
