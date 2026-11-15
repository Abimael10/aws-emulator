// SPDX-License-Identifier: CC0-1.0

//! End-to-end test for how `CreateDomain` refuses a name the account already holds.
//!
//! The rule under test: the `DomainName` is the domain's identity, so a second create of a
//! name already held is refused `BadRequestException` (400) with the service's own
//! sentence — the fixed text `Domain ` / ` already exists`, whose one variable part is the
//! `DomainName` the request carried — and the domain the first create stored is left
//! exactly as it was.
//!
//! The rows differ along what the sentence's variable part can be: names of different
//! lengths, at both ends of the member's modelled `@length` (1..64), over every character
//! class its pattern `^[a-zA-Z0-9_-]+$` admits. Every case goes through the real router, so
//! what is asserted is the wire behaviour a client sees. The HTTP plumbing and the create
//! helpers are this crate's one test harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};

/// The names the table creates twice. The shapes span the member's modelled bounds and
/// character classes (`smithy_query(query="operation", operation="CreateDomain")`).
fn held_names() -> Vec<(&'static str, String)> {
    vec![
        ("a name of one character, the modelled minimum", "d".to_string()),
        ("an ordinary name", "retail-crm".to_string()),
        ("a name carrying digits and underscores", "crm_2026_eu".to_string()),
        ("a name of mixed case", "RetailCRM".to_string()),
        ("a name at the modelled maximum length of 64", "d".repeat(64)),
    ]
}

/// The rule itself: the second create is refused, the refusal names the requested domain
/// and nothing else, and the stored domain is untouched.
#[tokio::test]
async fn a_second_create_of_a_held_domain_name_is_refused_naming_that_name() {
    let emulator = Emulator::new();

    for (case, domain_name) in held_names() {
        let (status, created) = emulator
            .post(
                &format!("/domains/{domain_name}"),
                json!({ "DefaultExpirationDays": 366, "Tags": { "env": "dev" } }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first CreateDomain to be served, got {status} {created:?}"
        );

        // A second create of the same name, carrying DIFFERENT configuration: it is not an
        // update, so nothing of it is stored.
        let (status, refused) = emulator
            .post(
                &format!("/domains/{domain_name}"),
                json!({ "DefaultExpirationDays": 1, "Tags": { "env": "prod" } }),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {status} {refused:?}"
        );
        assert_eq!(
            error_message(&refused),
            format!("Domain {domain_name} already exists"),
            "case {case:?}: expected the service's wording, got {:?}",
            error_message(&refused)
        );

        // The first create's record stands: the refusal changed nothing.
        let (status, fetched) = emulator
            .send("GET", &format!("/domains/{domain_name}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetDomain 200, got {status} {fetched:?}"
        );
        for member in ["DomainName", "DefaultExpirationDays", "Tags", "CreatedAt"] {
            assert_eq!(
                fetched.get(member),
                created.get(member),
                "case {case:?}: expected GetDomain {member} = {:?} (as the first create stored \
                 it), got {:?}",
                created.get(member),
                fetched.get(member)
            );
        }
    }

    // The name is the identity: a name no create has taken is still created.
    let (status, body) = emulator
        .post("/domains/not-taken", json!({ "DefaultExpirationDays": 366 }))
        .await;
    assert_eq!(
        status, 200,
        "case \"a name no create has taken\": expected 200, got {status} {body:?}"
    );
    assert_eq!(
        body.get("DomainName").and_then(Value::as_str),
        Some("not-taken"),
        "case \"a name no create has taken\": expected the name echoed, got {:?}",
        body.get("DomainName")
    );
}
