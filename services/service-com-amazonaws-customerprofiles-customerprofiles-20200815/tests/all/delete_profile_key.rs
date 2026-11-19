// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteProfileKey`.
//!
//! The rule under test: a key value is removable from a profile exactly when that profile
//! carries it under the named key, and removing it un-indexes the pair, so
//! `SearchProfiles` no longer finds the profile through it while every other pair the
//! domain holds is untouched. A value the profile does not carry under that key names no
//! association to remove, so the whole request is refused with
//! `ResourceNotFoundException` (404) and NOTHING is written — the same refusal an unknown
//! domain or profile gets, each with its own wording.
//!
//! Each test asserts that over a SET of inputs — key names and values of different
//! shapes, counts, forms and sizes — rather than for one recorded request. Every case
//! goes through the real router, so what is asserted is the wire behaviour a client sees.
//! The HTTP plumbing and the create helpers are this crate's one test harness, in
//! `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{error_code, error_message, item_ids, Emulator};

/// How this service words a delete of a key value the named profile does not carry under
/// the named key: fixed text for the error class, interpolating nothing from the request.
const KEY_NOT_FOUND: &str = "Failed to delete profile key due to key not found";

/// The fixed acknowledgement a completed delete answers with, the same one every other
/// delete of this service answers (`tests/all/profile_collection_messages.rs` pins it for
/// `DeleteProfile`).
const DELETE_ACKNOWLEDGEMENT: &str = "OK";

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

/// `AddProfileKey`, asserting it succeeded.
async fn associate(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    profile_id: &str,
    key_name: &str,
    values: &[&str],
) {
    let (status, body) = add_key(emulator, domain, profile_id, key_name, values).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AddProfileKey 200 for {key_name:?}={values:?}, got \
         {status} {body:?}"
    );
}

/// `DeleteProfileKey` over the wire: `POST /domains/{DomainName}/profiles/keys/delete`.
async fn delete_key(
    emulator: &Emulator,
    domain: &str,
    profile_id: &str,
    key_name: &str,
    values: &[&str],
) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/profiles/keys/delete"),
            json!({ "ProfileId": profile_id, "KeyName": key_name, "Values": values }),
        )
        .await
}

/// The ids `SearchProfiles` finds for one (key name, value) pair, asserting the search
/// itself was served.
async fn found_ids(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    key_name: &str,
    value: &str,
) -> Vec<String> {
    let (status, body) = emulator
        .search(domain, json!({ "KeyName": key_name, "Values": [value] }), "")
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SearchProfiles 200 for {key_name:?}={value:?}, got \
         {status} {body:?}"
    );
    item_ids(&body)
}

/// Assert a refusal is the modelled key-not-found one, worded as the service words it.
fn assert_key_not_found(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body)),
        (404, "ResourceNotFoundException"),
        "case {case:?}: expected 404 ResourceNotFoundException, got {status} {body:?}"
    );
    assert_eq!(
        error_message(body),
        KEY_NOT_FOUND,
        "case {case:?}: expected the service's key-not-found sentence, got {:?}",
        error_message(body)
    );
}

/// A `ProfileId` that satisfies the modelled pattern `^[a-f0-9]{32}$` but that no create
/// in these tests minted, so the store cannot hold it.
fn unknown_id(seed: u64) -> String {
    format!("{:032x}", 0xfeed_0000_0000_0000u64 + seed)
}

/// One row of the removal table: what is associated, what is then deleted (as the caller
/// writes it), and what still has to match afterwards.
struct Removal {
    case: &'static str,
    key_name: String,
    added: Vec<String>,
    deleted: Vec<String>,
    remaining: Vec<String>,
}

/// Every shape of removal a request may ask for. The key names are predefined ones
/// `DeleteProfileKey$KeyName` admits (`_ctrContactId`, `_orderId`, `_zendeskTicketId`,
/// `_fullName`, `_phone`) and names of a caller's own, at both ends of the member's
/// modelled length (1..64) and over the characters its pattern `^[a-zA-Z0-9_-]+$` admits;
/// the values span one value, a subset of several, all of several, both ends of
/// `requestValueList`'s member length (1..255), and a value written in another form of
/// the one it was associated in.
fn removals() -> Vec<Removal> {
    vec![
        Removal {
            case: "the only value of a predefined contact-record key",
            key_name: "_ctrContactId".to_string(),
            added: vec!["11111111-2222-3333-4444-555555555555".to_string()],
            deleted: vec!["11111111-2222-3333-4444-555555555555".to_string()],
            remaining: vec![],
        },
        Removal {
            case: "one value out of three under one key",
            key_name: "_orderId".to_string(),
            added: vec!["ord-1".to_string(), "ord-2".to_string(), "ord-3".to_string()],
            deleted: vec!["ord-2".to_string()],
            remaining: vec!["ord-1".to_string(), "ord-3".to_string()],
        },
        Removal {
            case: "two values of three at once",
            key_name: "_orderId".to_string(),
            added: vec!["o-1".to_string(), "o-2".to_string(), "o-3".to_string()],
            deleted: vec!["o-1".to_string(), "o-3".to_string()],
            remaining: vec!["o-2".to_string()],
        },
        Removal {
            case: "every value a key carries",
            key_name: "_zendeskTicketId".to_string(),
            added: vec!["42".to_string(), "43".to_string()],
            deleted: vec!["43".to_string(), "42".to_string()],
            remaining: vec![],
        },
        Removal {
            case: "a caller's own key name",
            key_name: "LoyaltyId".to_string(),
            added: vec!["L-9".to_string()],
            deleted: vec!["L-9".to_string()],
            remaining: vec![],
        },
        Removal {
            case: "key name and value of one character each (both minimum lengths)",
            key_name: "k".to_string(),
            added: vec!["v".to_string(), "w".to_string()],
            deleted: vec!["v".to_string()],
            remaining: vec!["w".to_string()],
        },
        Removal {
            case: "key name at the maximum modelled length of 64",
            key_name: "k".repeat(64),
            added: vec!["long-key-name".to_string()],
            deleted: vec!["long-key-name".to_string()],
            remaining: vec![],
        },
        Removal {
            case: "value at the maximum modelled length of 255",
            key_name: "crm-system_7".to_string(),
            added: vec!["v".repeat(255), "short".to_string()],
            deleted: vec!["v".repeat(255)],
            remaining: vec!["short".to_string()],
        },
        Removal {
            case: "every character class the key-name pattern admits",
            key_name: "Mixed-Key_09".to_string(),
            added: vec!["mixed".to_string()],
            deleted: vec!["mixed".to_string()],
            remaining: vec![],
        },
        // A key that names a CLASS of value matches whichever form the class is written
        // in, so a value is removed by any form of itself: `_fullName` disregards case
        // and `_phone` the punctuation a number is grouped with.
        Removal {
            case: "a name key deleted in another casing than it was associated in",
            key_name: "_fullName".to_string(),
            added: vec!["Ada Lovelace".to_string()],
            deleted: vec!["ADA LOVELACE".to_string()],
            remaining: vec![],
        },
        Removal {
            case: "a phone key deleted in another grouping than it was associated in",
            key_name: "_phone".to_string(),
            added: vec!["+1 555-0100".to_string()],
            deleted: vec!["15550100".to_string()],
            remaining: vec![],
        },
    ]
}

/// The rule itself: a deleted key value stops matching `SearchProfiles`, every value the
/// request did not name keeps matching, and nothing else the domain holds is touched.
#[tokio::test]
async fn a_deleted_key_value_stops_finding_the_profile_it_was_associated_with() {
    let emulator = Emulator::new();
    emulator.create_domain("delkeydom", 30).await;

    for (index, row) in removals().into_iter().enumerate() {
        let Removal { case, key_name, added, deleted, remaining } = row;
        // One profile per case, carrying no standard attribute that would populate a
        // predefined key of its own: what the case is about is the keys associated with
        // it.
        let profile_id = emulator
            .create_profile("delkeydom", json!({ "Attributes": { "case": case } }))
            .await;
        // A second profile carries the same KEY NAME under another value, so the key
        // stays a key the domain defines after the removal and a search by it is still
        // served — and so the removal is shown to touch nobody else's association.
        let keeper = emulator
            .create_profile("delkeydom", json!({ "Attributes": { "keeper": case } }))
            .await;
        let keeper_value = format!("keeper-{index}");

        let borrowed: Vec<&str> = added.iter().map(String::as_str).collect();
        associate(&emulator, case, "delkeydom", &profile_id, &key_name, &borrowed).await;
        associate(
            &emulator,
            case,
            "delkeydom",
            &keeper,
            &key_name,
            &[keeper_value.as_str()],
        )
        .await;

        let asked: Vec<&str> = deleted.iter().map(String::as_str).collect();
        let (status, body) =
            delete_key(&emulator, "delkeydom", &profile_id, &key_name, &asked).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteProfileKey 200, got {status} {body:?}"
        );
        let message = body
            .get("Message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            message, DELETE_ACKNOWLEDGEMENT,
            "case {case:?}: expected the fixed delete acknowledgement, got {message:?}"
        );

        for value in &deleted {
            let found = found_ids(&emulator, case, "delkeydom", &key_name, value).await;
            assert!(
                !found.contains(&profile_id),
                "case {case:?}: expected {key_name:?}={value:?} no longer to find \
                 {profile_id}, got {found:?}"
            );
        }
        for value in &remaining {
            assert_eq!(
                found_ids(&emulator, case, "delkeydom", &key_name, value).await,
                vec![profile_id.clone()],
                "case {case:?}: expected {key_name:?}={value:?} still to find {profile_id}, \
                 got {:?}",
                found_ids(&emulator, case, "delkeydom", &key_name, value).await
            );
        }
        // Another profile's association under the same key name is untouched.
        assert_eq!(
            found_ids(&emulator, case, "delkeydom", &key_name, &keeper_value).await,
            vec![keeper.clone()],
            "case {case:?}: expected the other profile's {key_name:?}={keeper_value:?} to be \
             untouched, got {:?}",
            found_ids(&emulator, case, "delkeydom", &key_name, &keeper_value).await
        );

        // The removal happened, so asking for it again names a key value the profile no
        // longer carries: the same request is now the key-not-found refusal.
        let (status, body) =
            delete_key(&emulator, "delkeydom", &profile_id, &key_name, &asked).await;
        assert_key_not_found(case, status, &body);
    }
}

/// A value the profile does not carry under the named key is refused, and refusing writes
/// nothing: every association the request did not remove is still there.
#[tokio::test]
async fn deleting_a_value_the_profile_does_not_carry_is_not_found_and_changes_nothing() {
    let emulator = Emulator::new();
    emulator.create_domain("mismatchdom", 30).await;
    let profile_id = emulator
        .create_profile("mismatchdom", json!({ "Attributes": { "who": "subject" } }))
        .await;
    let other = emulator
        .create_profile("mismatchdom", json!({ "Attributes": { "who": "other" } }))
        .await;
    associate(
        &emulator,
        "setup",
        "mismatchdom",
        &profile_id,
        "_ctrContactId",
        &["ctr-real"],
    )
    .await;
    associate(
        &emulator,
        "setup",
        "mismatchdom",
        &profile_id,
        "LoyaltyId",
        &["L-1"],
    )
    .await;
    associate(
        &emulator,
        "setup",
        "mismatchdom",
        &other,
        "_ctrContactId",
        &["ctr-other"],
    )
    .await;

    // Each row asks for a removal the profile cannot make. They differ in WHY: the value
    // was never associated at all, it belongs to another key of the same profile, it
    // belongs to another profile, it is asked for beside a value that IS carried, the
    // key carries nothing on this profile, and the request names no value at all.
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "a value never associated under that key",
            "_ctrContactId",
            vec!["ctr-never"],
        ),
        (
            "a value this profile carries under ANOTHER key",
            "_ctrContactId",
            vec!["L-1"],
        ),
        (
            "a value ANOTHER profile carries under that key",
            "_ctrContactId",
            vec!["ctr-other"],
        ),
        (
            "a carried value asked for beside one that is not",
            "_ctrContactId",
            vec!["ctr-real", "ctr-never"],
        ),
        (
            "a key this profile carries no value under",
            "_orderId",
            vec!["ord-1"],
        ),
        ("no value at all", "_ctrContactId", vec![]),
    ];
    for (case, key_name, values) in rows {
        let (status, body) =
            delete_key(&emulator, "mismatchdom", &profile_id, key_name, &values).await;
        assert_key_not_found(case, status, &body);

        // The refusal wrote nothing: every association made above still finds its
        // profile.
        assert_eq!(
            found_ids(&emulator, case, "mismatchdom", "_ctrContactId", "ctr-real").await,
            vec![profile_id.clone()],
            "case {case:?}: expected the real value still to find {profile_id}, got {:?}",
            found_ids(&emulator, case, "mismatchdom", "_ctrContactId", "ctr-real").await
        );
        assert_eq!(
            found_ids(&emulator, case, "mismatchdom", "LoyaltyId", "L-1").await,
            vec![profile_id.clone()],
            "case {case:?}: expected the other key still to find {profile_id}, got {:?}",
            found_ids(&emulator, case, "mismatchdom", "LoyaltyId", "L-1").await
        );
        assert_eq!(
            found_ids(&emulator, case, "mismatchdom", "_ctrContactId", "ctr-other").await,
            vec![other.clone()],
            "case {case:?}: expected the other profile still to be found, got {:?}",
            found_ids(&emulator, case, "mismatchdom", "_ctrContactId", "ctr-other").await
        );
    }

    // The positive control: the value that IS carried is removed, which is what shows
    // every refusal above was about the value and not about the operation.
    let (status, body) = delete_key(
        &emulator,
        "mismatchdom",
        &profile_id,
        "_ctrContactId",
        &["ctr-real"],
    )
    .await;
    assert_eq!(
        status, 200,
        "the carried value: expected DeleteProfileKey 200, got {status} {body:?}"
    );
    let found = found_ids(
        &emulator,
        "the carried value",
        "mismatchdom",
        "_ctrContactId",
        "ctr-real",
    )
    .await;
    assert!(
        found.is_empty(),
        "the carried value: expected it to find nothing after the delete, got {found:?}"
    );
}

/// A domain or a profile the store does not hold is refused with ITS own wording, not the
/// key-not-found one, and writes nothing.
#[tokio::test]
async fn deleting_a_key_of_an_absent_domain_or_profile_is_not_found() {
    let emulator = Emulator::new();
    emulator.create_domain("absentdom", 30).await;
    emulator.create_domain("otherdom", 30).await;
    let here = emulator
        .create_profile("absentdom", json!({ "Attributes": { "who": "here" } }))
        .await;
    let elsewhere = emulator
        .create_profile("otherdom", json!({ "Attributes": { "who": "elsewhere" } }))
        .await;
    associate(&emulator, "setup", "absentdom", &here, "LoyaltyId", &["L-1"]).await;
    associate(
        &emulator,
        "setup",
        "otherdom",
        &elsewhere,
        "LoyaltyId",
        &["L-2"],
    )
    .await;

    let absent = unknown_id(1);
    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "a domain the store does not hold",
            "nosuchdom",
            here.clone(),
            "Domain nosuchdom is not found".to_string(),
        ),
        (
            "a profile id no create minted",
            "absentdom",
            absent.clone(),
            format!("Profile with profileId {absent} is not found in the domain"),
        ),
        (
            "a profile another domain holds",
            "absentdom",
            elsewhere.clone(),
            format!("Profile with profileId {elsewhere} is not found in the domain"),
        ),
    ];
    for (case, domain, profile_id, message) in rows {
        let (status, body) = delete_key(&emulator, domain, &profile_id, "LoyaltyId", &["L-1"]).await;
        assert_eq!(
            (status, error_code(&body)),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} {body:?}"
        );
        assert_eq!(
            error_message(&body),
            message,
            "case {case:?}: expected the service's wording, got {:?}",
            error_message(&body)
        );
    }

    // Nothing was written: both associations still find their profiles.
    assert_eq!(
        found_ids(&emulator, "after the refusals", "absentdom", "LoyaltyId", "L-1").await,
        vec![here],
        "expected the association in the named domain to survive the refusals"
    );
    assert_eq!(
        found_ids(&emulator, "after the refusals", "otherdom", "LoyaltyId", "L-2").await,
        vec![elsewhere],
        "expected the association in the other domain to survive the refusals"
    );
}

/// A pair the profile's OWN attributes populate is the profile's data, not an
/// association: removing the association leaves the attribute's pair indexed, and where
/// there is no association at all there is nothing to remove.
#[tokio::test]
async fn a_key_value_the_profiles_own_attributes_populate_outlives_the_association() {
    let emulator = Emulator::new();
    emulator.create_domain("deriveddom", 30).await;

    // Each row is a predefined key one of a profile's own members populates, with the
    // member that populates it and the value a search by that key matches (the key's own
    // canonical form of it): `_email` from `EmailAddress`, `_phone` from `PhoneNumber`,
    // `_account` from `AccountNumber` (`SearchProfiles$KeyName` lists the keys; the
    // members come from `Profile`).
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "_email",
            "_email",
            json!({ "EmailAddress": "ada@example.invalid" }),
            "ada@example.invalid",
        ),
        (
            "_phone",
            "_phone",
            json!({ "PhoneNumber": "+15550100" }),
            "+15550100",
        ),
        (
            "_account",
            "_account",
            json!({ "AccountNumber": "acct-7" }),
            "acct-7",
        ),
    ];
    for (case, key_name, profile, value) in rows {
        // Nothing was associated with this profile: the pair it is found by comes from
        // its own member, so there is no association to remove.
        let derived_only = emulator.create_profile("deriveddom", profile.clone()).await;
        let (status, body) =
            delete_key(&emulator, "deriveddom", &derived_only, key_name, &[value]).await;
        assert_key_not_found(case, status, &body);
        assert_eq!(
            found_ids(&emulator, case, "deriveddom", key_name, value).await,
            vec![derived_only.clone()],
            "case {case:?}: expected the member's own pair to be untouched, got {:?}",
            found_ids(&emulator, case, "deriveddom", key_name, value).await
        );

        // The same pair ALSO associated by hand: the association is removable, and
        // removing it leaves the member's pair, so the profile is still found by it.
        let both = emulator.create_profile("deriveddom", profile.clone()).await;
        associate(&emulator, case, "deriveddom", &both, key_name, &[value]).await;
        let (status, body) = delete_key(&emulator, "deriveddom", &both, key_name, &[value]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteProfileKey 200 for the association, got \
             {status} {body:?}"
        );
        let found = found_ids(&emulator, case, "deriveddom", key_name, value).await;
        assert!(
            found.contains(&both),
            "case {case:?}: expected {both} still to be found by its own member's pair, \
             got {found:?}"
        );
    }
}

/// How many `DELETED_PROFILE_KEY` records one profile's history carries:
/// `ListProfileHistoryRecords` filtered to that one action class, which is the number of
/// removals recorded against the profile.
async fn removals_recorded(emulator: &Emulator, domain: &str, profile_id: &str) -> usize {
    let (status, body) = emulator
        .post(
            &format!("/domains/{domain}/profiles/history-records"),
            json!({ "ProfileId": profile_id, "ActionType": "DELETED_PROFILE_KEY" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected ListProfileHistoryRecords 200, got {status} {body:?}"
    );
    body.get("ProfileHistoryRecords")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// The removal is recorded in the profile's history, as the action the service models for
/// it: one `DELETED_PROFILE_KEY` record per completed delete, and none for a refused one.
#[tokio::test]
async fn a_completed_removal_appends_one_history_record() {
    let emulator = Emulator::new();
    emulator.create_domain("histkeydom", 30).await;
    let profile_id = emulator
        .create_profile("histkeydom", json!({ "Attributes": { "who": "history" } }))
        .await;
    associate(
        &emulator,
        "setup",
        "histkeydom",
        &profile_id,
        "_orderId",
        &["h-1", "h-2"],
    )
    .await;

    let recorded = removals_recorded(&emulator, "histkeydom", &profile_id).await;
    assert_eq!(
        recorded, 0,
        "before any delete: expected no removal recorded, got {recorded}"
    );

    // A refused removal records nothing: no key value was removed.
    let (status, body) = delete_key(
        &emulator,
        "histkeydom",
        &profile_id,
        "_orderId",
        &["h-never"],
    )
    .await;
    assert_key_not_found("a refused removal", status, &body);
    let recorded = removals_recorded(&emulator, "histkeydom", &profile_id).await;
    assert_eq!(
        recorded, 0,
        "after a refused delete: expected no removal recorded, got {recorded}"
    );

    // One record per completed delete, whatever the number of values it removed.
    for (case, values, expected) in [
        ("one value", vec!["h-1"], 1usize),
        ("the remaining value", vec!["h-2"], 2usize),
    ] {
        let (status, body) =
            delete_key(&emulator, "histkeydom", &profile_id, "_orderId", &values).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteProfileKey 200, got {status} {body:?}"
        );
        let recorded = removals_recorded(&emulator, "histkeydom", &profile_id).await;
        assert_eq!(
            recorded, expected,
            "case {case:?}: expected {expected} removal(s) recorded, got {recorded}"
        );
    }
}
