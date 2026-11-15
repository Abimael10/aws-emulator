// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the item `SearchProfiles` returns after a profile WRITE.
//!
//! The rule under test: every profile write — a create or an update — leaves the domain's
//! search-key index holding exactly the key values the stored profile's own members
//! populate, so a search by one of those keys returns that profile as ONE COMPLETE item:
//! every member the write left on the record, plus `FoundByItems` naming the key and the
//! STORED form of the value that matched. Nothing a write superseded is still reachable,
//! and no member a write stored is dropped from the projection.
//!
//! The predefined keys a profile's own members populate are the ones
//! `SearchProfiles$KeyName` lists — `_account`, `_email`, `_phone`, `_fullName` and
//! `_profileId` (`smithy_query(query="operation", operation="SearchProfiles")`) — and the
//! members the projection carries are `Profile`'s own, including the string alternatives
//! `PartyTypeString` and `GenderString` ("An alternative to PartyType which accepts any
//! string as input").
//!
//! The item is compared against the SAME profile read back through `BatchGetProfile`, so
//! what is asserted is that the search view and the stored record agree member for
//! member rather than that one hand-written item matched. Every case goes through the real
//! router; the HTTP plumbing and the create helpers are this crate's one test harness in
//! `tests/all/domain_profile_lifecycle.rs`, and the `UpdateProfile` call and the
//! `BatchGetProfile` read are the ones in `tests/all/update_profile.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::Emulator;
use crate::update_profile::{read_profile, update};

/// The domain every row writes its profile into.
const DOMAIN: &str = "writesearchdom";

/// One row of the table: a profile written one way, and the search that must find it.
struct Row<'a> {
    /// What the row varies, named in every assertion.
    case: &'a str,
    /// The `CreateProfile` body.
    created: Value,
    /// The `UpdateProfile` patch applied after the create, where the row has one.
    update: Option<Value>,
    /// The predefined key the search names.
    key: &'a str,
    /// The value as the SEARCH request writes it.
    searched: &'a str,
    /// The value the domain holds the profile under, which `FoundByItems` must report.
    stored: &'a str,
    /// Members the write persisted, each of which the returned item must carry with that
    /// value — the projection asserted member by member, beside the whole-record
    /// comparison below.
    reported: Value,
    /// A value the write superseded, which must now find nothing.
    superseded: Option<&'a str>,
}

/// The `Items` of a `SearchProfiles` call for one key and one value.
async fn search_items(emulator: &Emulator, key: &str, value: &str) -> Vec<Value> {
    let (status, body) = emulator
        .search(DOMAIN, json!({ "KeyName": key, "Values": [value] }), "")
        .await;
    assert_eq!(
        status, 200,
        "search {key}={value:?}: expected SearchProfiles 200, got {status} {body:?}"
    );
    body.get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The rule, over a table whose rows differ in WHICH write set the key value (a create, an
/// update that adds one the create left unset, an update that replaces one), in WHICH
/// predefined key it is, in the FORM the search writes the value in, and in how much the
/// profile carries beside it.
#[tokio::test]
async fn a_profile_write_leaves_the_whole_profile_findable_by_the_key_it_set() {
    let emulator = Emulator::new();
    emulator.create_domain(DOMAIN, 90).await;

    let rows = vec![
        Row {
            case: "a create sets the account number the search names",
            created: json!({
                "AccountNumber": "acct-created-1",
                "Attributes": { "AccountNumber": "acct-created-1" },
            }),
            update: None,
            key: "_account",
            searched: "acct-created-1",
            stored: "acct-created-1",
            reported: json!({
                "AccountNumber": "acct-created-1",
                "Attributes": { "AccountNumber": "acct-created-1" },
            }),
            superseded: None,
        },
        Row {
            case: "an update sets an account number the create left unset",
            created: json!({ "Attributes": { "AccountNumber": "acct-updated-2" } }),
            update: Some(json!({ "AccountNumber": "acct-updated-2" })),
            key: "_account",
            searched: "acct-updated-2",
            stored: "acct-updated-2",
            reported: json!({
                "AccountNumber": "acct-updated-2",
                "Attributes": { "AccountNumber": "acct-updated-2" },
            }),
            superseded: None,
        },
        Row {
            case: "an update replaces the account number the create set",
            created: json!({ "AccountNumber": "acct-old-3" }),
            update: Some(json!({ "AccountNumber": "acct-new-3" })),
            key: "_account",
            searched: "acct-new-3",
            stored: "acct-new-3",
            reported: json!({ "AccountNumber": "acct-new-3" }),
            superseded: Some("acct-old-3"),
        },
        Row {
            case: "an update sets an email address the create left unset",
            created: json!({ "FirstName": "Mary", "LastName": "Jackson" }),
            update: Some(json!({ "EmailAddress": "mary4@example.com" })),
            key: "_email",
            searched: "mary4@example.com",
            stored: "mary4@example.com",
            reported: json!({
                "EmailAddress": "mary4@example.com",
                "FirstName": "Mary",
                "LastName": "Jackson",
            }),
            superseded: None,
        },
        Row {
            case: "an update sets a phone number the search writes in other punctuation",
            created: json!({ "AccountNumber": "acct-5" }),
            update: Some(json!({ "PhoneNumber": "+1 555-0188" })),
            key: "_phone",
            searched: "1 (555) 0188",
            stored: "15550188",
            // The member keeps the value as it was written; only the index folds it.
            reported: json!({ "PhoneNumber": "+1 555-0188" }),
            superseded: None,
        },
        Row {
            case: "an update completes a full name through one of its parts",
            created: json!({ "FirstName": "Grace" }),
            update: Some(json!({ "LastName": "Hopper" })),
            key: "_fullName",
            searched: "GRACE HOPPER",
            stored: "grace hopper",
            reported: json!({ "FirstName": "Grace", "LastName": "Hopper" }),
            superseded: Some("Grace"),
        },
        Row {
            case: "a write that also stores the string alternatives of two enum members",
            created: json!({
                "AccountNumber": "acct-7",
                "PartyTypeString": "Sole Trader",
                "Attributes": { "tier": "gold" },
            }),
            update: Some(json!({ "GenderString": "nonbinary", "BusinessName": "Engines" })),
            key: "_account",
            searched: "acct-7",
            stored: "acct-7",
            reported: json!({
                "PartyTypeString": "Sole Trader",
                "GenderString": "nonbinary",
                "BusinessName": "Engines",
                "Attributes": { "tier": "gold" },
            }),
            superseded: None,
        },
        Row {
            case: "a write that leaves every standard member but one unset",
            created: json!({ "AccountNumber": "acct-8" }),
            update: None,
            key: "_account",
            searched: "acct-8",
            stored: "acct-8",
            reported: json!({ "AccountNumber": "acct-8" }),
            superseded: None,
        },
    ];

    for row in rows {
        let Row {
            case,
            created,
            update: patch,
            key,
            searched,
            stored: stored_value,
            reported,
            superseded,
        } = row;
        let profile_id = emulator.create_profile(DOMAIN, created).await;
        if let Some(patch) = patch {
            let (status, body) = update(&emulator, DOMAIN, &profile_id, patch).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected UpdateProfile 200, got {status} {body:?}"
            );
        }
        let record = read_profile(&emulator, DOMAIN, &profile_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the written profile must be readable"));

        // The profile the write left behind is findable by the key that write populated.
        let items = search_items(&emulator, key, searched).await;
        let ids: Vec<&str> = items
            .iter()
            .map(|item| {
                item.get("ProfileId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            ids,
            vec![profile_id.as_str()],
            "case {case:?}: expected {key}={searched:?} to find exactly the written profile \
             {profile_id:?}, got {ids:?} (items {items:?})"
        );

        // `FoundByItems` names the key that matched and the value the domain holds.
        let item = items[0].clone();
        let expected_found_by = json!([{ "KeyName": key, "Values": [stored_value] }]);
        assert_eq!(
            item.get("FoundByItems"),
            Some(&expected_found_by),
            "case {case:?}: expected FoundByItems {expected_found_by:?}, got {:?}",
            item.get("FoundByItems")
        );

        // Each member the write persisted is carried by the item with the value stored.
        for (member, value) in reported
            .as_object()
            .expect("the reported members are an object")
        {
            let actual = item.get(member);
            assert_eq!(
                actual,
                Some(value),
                "case {case:?}: expected {member} to be reported as {value:?}, got {actual:?} \
                 (whole item {item:?})"
            );
        }

        // Every member the write stored is projected into the item: the search view and
        // the stored record are the same profile.
        let mut projected = item
            .as_object()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the item must be an object, got {item:?}"));
        projected.remove("FoundByItems");
        let projected = Value::Object(projected);
        assert_eq!(
            projected, record,
            "case {case:?}: expected the item to carry the whole stored profile {record:?}, \
             got {projected:?}"
        );

        // The profile is reachable under its own minted id too, with the same whole item.
        let by_id = search_items(&emulator, "_profileId", &profile_id).await;
        let mut by_id_members = by_id
            .first()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected _profileId to find the profile, got {by_id:?}")
            });
        by_id_members.remove("FoundByItems");
        assert_eq!(
            Value::Object(by_id_members),
            record,
            "case {case:?}: expected the _profileId search to carry the same whole profile \
             {record:?}, got {by_id:?}"
        );

        // A value the write superseded reaches nothing: the index holds what is stored now.
        if let Some(superseded) = superseded {
            let stale = search_items(&emulator, key, superseded).await;
            assert!(
                stale.is_empty(),
                "case {case:?}: expected {key}={superseded:?} to find nothing after the write, \
                 got {stale:?}"
            );
        }
    }
}
