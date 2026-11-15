// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `SearchProfiles` resolves a search key.
//!
//! Two rules, both about the KEY a search names rather than about the profiles it finds:
//!
//! 1. a `KeyName` names a key DEFINED in the domain — one the service predefines
//!    (`SearchProfiles$KeyName` lists them), one the domain's own object types declare, or
//!    one associated with a profile of the domain — and a name that is none of those (a
//!    raw profile FIELD name, for instance) is refused `ResourceNotFoundException` (404)
//!    before anything is looked up, never answered as an empty page;
//! 2. a predefined key that names a CLASS of value indexes its values in that key's
//!    canonical form — `_fullName` without regard to case, `_phone` without the
//!    punctuation a number is grouped with — while every other key is an opaque identifier
//!    indexed exactly as written, so it matches only that value; and `FoundByItems` reports
//!    the STORED form, which is what tells a caller which value the profile is held under.
//!
//! Each rule is asserted over a SET of inputs — the kind of key, the written form of the
//! value, and which half of the request carries the key — rather than for one recorded
//! request. Every case goes through the real router, so what is asserted is the wire
//! behaviour a client sees. The HTTP plumbing and the create helpers are this crate's one
//! test harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, item_ids, Emulator};
use crate::profile_objects::put_contact_type;

/// `AddProfileKey` over the wire: `POST /domains/{DomainName}/profiles/keys`.
async fn add_key(
    emulator: &Emulator,
    domain: &str,
    profile_id: &str,
    key_name: &str,
    values: &[&str],
) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/profiles/keys"),
            json!({ "ProfileId": profile_id, "KeyName": key_name, "Values": values }),
        )
        .await
}

/// The `FoundByItems` a search reported for one profile, as (key name, values) pairs.
fn found_by(body: &Value, profile_id: &str) -> Vec<(String, Vec<String>)> {
    body.get("Items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("ProfileId").and_then(Value::as_str) == Some(profile_id))
        .flat_map(|item| {
            item.get("FoundByItems")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .map(|found| {
            (
                found
                    .get("KeyName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                found
                    .get("Values")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| value.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// RULE 1: a key the domain defines is searched, found or not; a key nothing defines is
/// refused, whichever half of the request carries it, and the refusal leaves the index
/// exactly as it was.
#[tokio::test]
async fn a_key_the_domain_does_not_define_is_refused_rather_than_answered_empty() {
    let emulator = Emulator::new();
    emulator.create_domain("keydom", 90).await;
    emulator.create_domain("otherdom", 90).await;
    // An object type declares `_contactId`, which the service does not predefine, so the
    // domain defines it; a key associated with a profile defines `LoyaltyId`.
    put_contact_type(&emulator, "keydom", "Contact_Record", true).await;
    let profile_id = emulator
        .create_profile("keydom", json!({ "AccountNumber": "ACCT-1" }))
        .await;
    let (status, body) = add_key(&emulator, "keydom", &profile_id, "LoyaltyId", &["L-9"]).await;
    assert_eq!(status, 200, "AddProfileKey: expected 200, got {status} {body:?}");

    // Defined keys: the search is answered, whether or not anything is indexed under the
    // value. The rows differ in WHAT defines the key.
    let defined: Vec<(&str, &str, &str, Vec<String>)> = vec![
        (
            "a predefined key the profile's own attributes populate",
            "_account",
            "ACCT-1",
            vec![profile_id.clone()],
        ),
        (
            "a predefined key nothing is indexed under",
            "_shopifyOrderId",
            "4242",
            Vec::new(),
        ),
        (
            "a predefined key only an ingested object can populate",
            "_orderId",
            "order-1",
            Vec::new(),
        ),
        (
            "a key one of the domain's object types declares",
            "_contactId",
            "c-1",
            Vec::new(),
        ),
        (
            "a key associated with a profile of the domain",
            "LoyaltyId",
            "L-9",
            vec![profile_id.clone()],
        ),
    ];
    for (case, key_name, value, expected) in defined {
        let (status, body) = emulator
            .search(
                "keydom",
                json!({ "KeyName": key_name, "Values": [value] }),
                "",
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({key_name}={value}): expected 200, got {status} {body:?}"
        );
        assert_eq!(
            item_ids(&body),
            expected,
            "case {case:?} ({key_name}={value}): expected Items {expected:?}, got {:?}",
            item_ids(&body)
        );
    }

    // Undefined keys: refused. The rows differ in the SHAPE of the name and in why
    // nothing defines it.
    let undefined: Vec<(&str, &str, &str)> = vec![
        (
            "the raw profile field name of a predefined key",
            "keydom",
            "AccountNumber",
        ),
        ("another raw profile field name", "keydom", "FirstName"),
        (
            "an underscore-prefixed name the service does not predefine",
            "keydom",
            "_orderNumber",
        ),
        (
            "a key defined in another domain only",
            "otherdom",
            "LoyaltyId",
        ),
        (
            "a key declared by an object type of another domain only",
            "otherdom",
            "_contactId",
        ),
    ];
    for (case, domain, key_name) in undefined {
        // Carried as the required `KeyName`, and as an `AdditionalSearchKeys` entry: the
        // rule holds for both halves of the request.
        let requests = [
            (
                "the required KeyName",
                json!({ "KeyName": key_name, "Values": ["ACCT-1"] }),
            ),
            (
                "an AdditionalSearchKeys entry",
                json!({
                    "KeyName": "_account",
                    "Values": ["ACCT-1"],
                    "AdditionalSearchKeys": [{ "KeyName": key_name, "Values": ["ACCT-1"] }],
                }),
            ),
        ];
        for (half, request) in requests {
            let (status, body) = emulator.search(domain, request, "").await;
            assert_eq!(
                (status, error_code(&body)),
                (404, "ResourceNotFoundException"),
                "case {case:?} ({key_name} as {half}): expected 404 \
                 ResourceNotFoundException, got {status} {body:?}"
            );
            assert_eq!(
                error_message(&body),
                format!("KeyName {key_name} is not defined in the domain"),
                "case {case:?} ({key_name} as {half}): expected the service's wording, got {:?}",
                error_message(&body)
            );
        }
    }

    // A refused search writes nothing: the profile is still reachable through the keys it
    // was indexed under, and the undefined name is still undefined.
    let (status, body) = emulator
        .search(
            "keydom",
            json!({ "KeyName": "_account", "Values": ["ACCT-1"] }),
            "",
        )
        .await;
    assert_eq!(status, 200, "after the refusals: expected 200, got {status} {body:?}");
    assert_eq!(
        item_ids(&body),
        vec![profile_id],
        "after the refusals: expected the profile to be reachable as before, got {:?}",
        item_ids(&body)
    );
}

/// RULE 2: the written form of a value matters exactly as much as the key says it does —
/// `_fullName` is matched without regard to case, `_phone` without punctuation, and every
/// other key only as written — and `FoundByItems` reports the stored form.
#[tokio::test]
async fn a_predefined_keys_values_are_matched_and_reported_in_the_keys_stored_form() {
    let emulator = Emulator::new();
    emulator.create_domain("normdom", 90).await;
    let profile_id = emulator
        .create_profile(
            "normdom",
            json!({
                "AccountNumber": "ACCT-1",
                "FirstName": "Grace",
                "LastName": "Hopper",
                "PhoneNumber": "+15550100",
                "EmailAddress": "Grace@Example.com",
            }),
        )
        .await;
    // A second profile sharing none of those values, so a hit is a real index lookup
    // rather than "the only profile in the domain".
    let other_id = emulator
        .create_profile(
            "normdom",
            json!({ "AccountNumber": "ACCT-2", "FirstName": "Ada", "LastName": "Lovelace" }),
        )
        .await;

    // (case, key, the value as the request writes it, the stored value FoundByItems must
    // report, or None when the search must find nothing)
    let rows: Vec<(&str, &str, &str, Option<&str>)> = vec![
        (
            "_fullName as the profile's members were written",
            "_fullName",
            "Grace Hopper",
            Some("grace hopper"),
        ),
        (
            "_fullName in upper case",
            "_fullName",
            "GRACE HOPPER",
            Some("grace hopper"),
        ),
        (
            "_fullName already folded",
            "_fullName",
            "grace hopper",
            Some("grace hopper"),
        ),
        (
            "_fullName of nobody",
            "_fullName",
            "margaret hamilton",
            None,
        ),
        (
            "_phone as the profile wrote it, with its plus",
            "_phone",
            "+15550100",
            Some("15550100"),
        ),
        (
            "_phone grouped with punctuation",
            "_phone",
            "+1 (555) 0100",
            Some("15550100"),
        ),
        ("_phone as digits alone", "_phone", "15550100", Some("15550100")),
        ("_phone of nobody", "_phone", "+15559999", None),
        (
            "_account exactly as written",
            "_account",
            "ACCT-1",
            Some("ACCT-1"),
        ),
        ("_account in another case", "_account", "acct-1", None),
        ("_account as a prefix of the stored value", "_account", "ACCT-", None),
        (
            "_account with its punctuation stripped",
            "_account",
            "ACCT1",
            None,
        ),
        (
            "_email exactly as written",
            "_email",
            "Grace@Example.com",
            Some("Grace@Example.com"),
        ),
        ("_email in another case", "_email", "grace@example.com", None),
    ];

    for (case, key_name, written, stored) in rows {
        let (status, body) = emulator
            .search(
                "normdom",
                json!({ "KeyName": key_name, "Values": [written] }),
                "",
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        match stored {
            Some(stored) => {
                assert_eq!(
                    item_ids(&body),
                    vec![profile_id.clone()],
                    "case {case:?}: expected {key_name}={written:?} to find the profile, got {:?}",
                    item_ids(&body)
                );
                assert_eq!(
                    found_by(&body, &profile_id),
                    vec![(key_name.to_string(), vec![stored.to_string()])],
                    "case {case:?}: expected FoundByItems to report the stored value \
                     {stored:?}, got {:?}",
                    found_by(&body, &profile_id)
                );
            }
            None => assert!(
                item_ids(&body).is_empty(),
                "case {case:?}: expected {key_name}={written:?} to find nothing, got {:?}",
                item_ids(&body)
            ),
        }
    }

    // The key values a caller associates are held to the same rule, and the other profile
    // is reachable by its own folded name: the fold belongs to the KEY, not to one profile.
    let (status, body) = emulator
        .search(
            "normdom",
            json!({ "KeyName": "_fullName", "Values": ["ADA LOVELACE"] }),
            "",
        )
        .await;
    assert_eq!(status, 200, "the other profile's name: got {status} {body:?}");
    assert_eq!(
        item_ids(&body),
        vec![other_id.clone()],
        "expected the other profile to be found by its own name, got {:?}",
        item_ids(&body)
    );
    assert_eq!(
        found_by(&body, &other_id),
        vec![("_fullName".to_string(), vec!["ada lovelace".to_string()])],
        "expected FoundByItems to report the other profile's stored name, got {:?}",
        found_by(&body, &other_id)
    );

    let (status, body) = add_key(&emulator, "normdom", &other_id, "_phone", &["+1-555-0200"]).await;
    assert_eq!(status, 200, "AddProfileKey: expected 200, got {status} {body:?}");
    for (case, written) in [
        ("as the association wrote it", "+1-555-0200"),
        ("as digits alone", "15550200"),
    ] {
        let (status, body) = emulator
            .search(
                "normdom",
                json!({ "KeyName": "_phone", "Values": [written] }),
                "",
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        assert_eq!(
            item_ids(&body),
            vec![other_id.clone()],
            "case {case:?}: expected the associated number to find that profile, got {:?}",
            item_ids(&body)
        );
        assert_eq!(
            found_by(&body, &other_id),
            vec![("_phone".to_string(), vec!["15550200".to_string()])],
            "case {case:?}: expected FoundByItems to report the stored value, got {:?}",
            found_by(&body, &other_id)
        );
    }
}
