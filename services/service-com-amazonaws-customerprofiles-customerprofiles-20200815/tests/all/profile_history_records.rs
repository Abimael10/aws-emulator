// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListProfileHistoryRecords`.
//!
//! The rule under test: a domain keeps, for each profile it holds, one history record per
//! ACTION taken on that profile — its creation, a write of its own members, a key value
//! associated with it, an object ingested onto it, an object the caller deleted, an object
//! whose retention ran out, and a merge that absorbed another profile — and
//! `ListProfileHistoryRecords` serves those records out of that store, oldest action
//! first, narrowed by each filter the request carries (`ActionType`, `ObjectTypeName`,
//! `PerformedBy`) and paged by `MaxResults`/`NextToken`. A record names the object type
//! the action was about (the standard profile for an action on the profile itself), the
//! object's unique key when it was about one object, and the principal whose request
//! performed it — nobody, for an action the clock owed. A request naming a domain or a
//! profile the store does not hold is `ResourceNotFoundException` (404), and a `NextToken`
//! the service did not issue is `BadRequestException` (400).
//!
//! Each test asserts that over a SET of inputs — every action class, every filter, several
//! page sizes — rather than for one recorded request. Every case goes through the real
//! router, so what is asserted is the wire behaviour a client sees. The HTTP plumbing and
//! the create helpers are this crate's one test harness, in
//! `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};
use std::time::{Duration, SystemTime};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    service_state, ProfileAction, ACTION_ADDED_PROFILE_KEY, ACTION_UPDATED,
    MAX_PROFILE_HISTORY_RECORDS, SECONDS_PER_DAY, STANDARD_PROFILE_OBJECT_TYPE,
};

use crate::domain_profile_lifecycle::{error_code, error_message, Emulator, REGION};
use crate::profile_objects::{contact, ingest, put_contact_type};
use crate::update_profile::update;

/// The object type every ingest in this suite goes through. Inside the modelled
/// `typeName` pattern `^[a-zA-Z_][a-zA-Z_0-9-]*$`.
const CONTACT_TYPE: &str = "Contact_Record";

/// A second object type, so a row can tell the `ObjectTypeName` filter apart from "every
/// record".
const TICKET_TYPE: &str = "Ticket_Record";

/// The largest page `MaxResults` admits (`@range(1..100)`), used wherever a test wants the
/// whole history in as few calls as possible.
const MAX_PAGE: i64 = 100;

/// A `ProfileId` that satisfies the modelled pattern `^[a-f0-9]{32}$` but that no create
/// in these tests minted, so the store cannot hold it.
fn unknown_id(seed: u64) -> String {
    format!("{:032x}", 0xfeed_0000_0000_0000u64 + seed)
}

/// The principal every caller-performed record reports: the one identity this emulator's
/// credential speaks for.
fn caller_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The sentence this service reports an absent profile with, wherever one is addressed by
/// id (`tests/all/profile_not_found_message.rs` pins it across those operations).
fn profile_not_found(profile_id: &str) -> String {
    format!("Profile with profileId {profile_id} is not found in the domain")
}

/// `ListProfileHistoryRecords` over the wire:
/// `POST /domains/{DomainName}/profiles/history-records`.
async fn list_history(
    emulator: &Emulator,
    domain: &str,
    profile_id: &str,
    filters: Value,
    query: &str,
) -> (u16, Value) {
    let mut body = json!({ "ProfileId": profile_id });
    for (member, value) in filters.as_object().expect("an object") {
        body[member] = value.clone();
    }
    emulator
        .post(
            &format!("/domains/{domain}/profiles/history-records{query}"),
            body,
        )
        .await
}

/// The records of one listing, asserting it succeeded.
async fn records(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    profile_id: &str,
    filters: Value,
) -> Vec<Value> {
    let (status, body) = list_history(
        emulator,
        domain,
        profile_id,
        filters.clone(),
        &format!("?max-results={MAX_PAGE}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListProfileHistoryRecords 200 for {filters:?}, got {status} {body:?}"
    );
    body.get("ProfileHistoryRecords")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ProfileHistoryRecords in {body:?}"))
}

/// Every record of a profile's history, walked page by page through the `NextToken` the
/// previous page handed back.
async fn every_record(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    profile_id: &str,
    page_size: i64,
) -> Vec<Value> {
    let mut walked = Vec::new();
    let mut query = format!("?max-results={page_size}");
    loop {
        let (status, body) = list_history(emulator, domain, profile_id, json!({}), &query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 walking {query:?}, got {status} {body:?}"
        );
        let page = body
            .get("ProfileHistoryRecords")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            page.len() <= page_size as usize,
            "case {case:?}: expected at most {page_size} records per page, got {}",
            page.len()
        );
        walked.extend(page);
        match body.get("NextToken").and_then(Value::as_str) {
            Some(token) => query = format!("?max-results={page_size}&next-token={token}"),
            None => return walked,
        }
    }
}

/// One record as a case compares it: the action, the object type it was about, and whether
/// it named one object's unique key.
fn shape_of(record: &Value) -> (String, String, bool) {
    (
        member(record, "ActionType"),
        member(record, "ObjectTypeName"),
        record.get("ProfileObjectUniqueKey").is_some(),
    )
}

/// One string member of a record, or the empty string when it carries none.
fn member(record: &Value, name: &str) -> String {
    record
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `ActionType`s of a listing, in the order it reported them.
fn action_types(records: &[Value]) -> Vec<String> {
    records.iter().map(|record| member(record, "ActionType")).collect()
}

/// Assert the history of `profile_id` is exactly `expected`, in order, and that every
/// record carries the members a record always carries: an `Id` of the modelled shape, a
/// `CreatedAt`, and the principal that performed the action.
async fn assert_history(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    profile_id: &str,
    expected: &[(&str, &str, bool)],
) -> Vec<Value> {
    let listed = records(emulator, case, domain, profile_id, json!({})).await;
    let shapes: Vec<(String, String, bool)> = listed.iter().map(shape_of).collect();
    let wanted: Vec<(String, String, bool)> = expected
        .iter()
        .map(|(action, object_type, keyed)| {
            ((*action).to_string(), (*object_type).to_string(), *keyed)
        })
        .collect();
    assert_eq!(
        shapes, wanted,
        "case {case:?}: expected the history {wanted:?}, got {shapes:?} from {listed:?}"
    );

    let mut ids = std::collections::BTreeSet::new();
    for record in &listed {
        let id = member(record, "Id");
        assert!(
            id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "case {case:?}: expected an Id matching ^[a-f0-9]{{32}}$, got {id:?}"
        );
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: expected every record to carry its own Id, got {id:?} twice"
        );
        assert!(
            record.get("CreatedAt").is_some(),
            "case {case:?}: CreatedAt is required, got {record:?}"
        );
        assert_eq!(
            member(record, "PerformedBy"),
            caller_principal(),
            "case {case:?}: expected the calling principal in PerformedBy, got {record:?}"
        );
    }
    listed
}

/// The `Ticket_Record` object type: the same definition every ingest here uses, under a
/// second name, so a row can filter by `ObjectTypeName`.
async fn seed_object_types(emulator: &Emulator, domain: &str) {
    put_contact_type(emulator, domain, CONTACT_TYPE, false).await;
    put_contact_type(emulator, domain, TICKET_TYPE, false).await;
}

/// `DeleteProfileObject` over the wire:
/// `POST /domains/{DomainName}/profiles/objects/delete`.
async fn delete_object(
    emulator: &Emulator,
    domain: &str,
    object_type: &str,
    profile_id: &str,
    unique_key: &str,
) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/profiles/objects/delete"),
            json!({
                "ObjectTypeName": object_type,
                "ProfileId": profile_id,
                "ProfileObjectUniqueKey": unique_key,
            }),
        )
        .await
}

/// `AddProfileKey` over the wire: `POST /domains/{DomainName}/profiles/keys`.
async fn add_key(
    emulator: &Emulator,
    domain: &str,
    profile_id: &str,
    key_name: &str,
    value: &str,
) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/profiles/keys"),
            json!({ "ProfileId": profile_id, "KeyName": key_name, "Values": [value] }),
        )
        .await
}

/// Every class of action a profile can be subject to appends exactly one record, in the
/// order the actions happened, naming the object type and object the action was about.
///
/// The table is the sequence of action classes: each step performs one, then the WHOLE
/// history is read back, so a step that recorded nothing, recorded twice, or recorded the
/// wrong subject fails on the step that did it.
#[tokio::test]
async fn every_action_on_a_profile_appends_one_history_record() {
    let emulator = Emulator::new();
    let domain = "actiondom";
    emulator.create_domain(domain, 90).await;
    seed_object_types(&emulator, domain).await;

    let profile = emulator
        .create_profile(
            domain,
            json!({ "PhoneNumber": "+15550100", "EmailAddress": "ada@example.com" }),
        )
        .await;
    let standard = STANDARD_PROFILE_OBJECT_TYPE;
    let mut expected: Vec<(&str, &str, bool)> = vec![("CREATED", standard, false)];
    assert_history(&emulator, "after CreateProfile", domain, &profile, &expected).await;

    let (status, body) = update(&emulator, domain, &profile, json!({ "LastName": "Lovelace" })).await;
    assert_eq!(status, 200, "UpdateProfile: expected 200, got {status} {body:?}");
    expected.push(("UPDATED", standard, false));
    assert_history(&emulator, "after UpdateProfile", domain, &profile, &expected).await;

    let (status, body) = add_key(&emulator, domain, &profile, "_contactId", "c-99").await;
    assert_eq!(status, 200, "AddProfileKey: expected 200, got {status} {body:?}");
    expected.push(("ADDED_PROFILE_KEY", standard, false));
    assert_history(&emulator, "after AddProfileKey", domain, &profile, &expected).await;

    let contact_key = ingest(
        &emulator,
        domain,
        CONTACT_TYPE,
        &contact("c-1", json!({ "phone": "+15550100" }), json!({})),
    )
    .await;
    expected.push(("INGESTED", CONTACT_TYPE, true));
    let listed =
        assert_history(&emulator, "after PutProfileObject", domain, &profile, &expected).await;
    assert_eq!(
        member(listed.last().expect("the ingest record"), "ProfileObjectUniqueKey"),
        contact_key,
        "after PutProfileObject: expected the record to name the ingested object, got {listed:?}"
    );

    let ticket_key = ingest(
        &emulator,
        domain,
        TICKET_TYPE,
        &contact("t-1", json!({ "phone": "+15550100" }), json!({})),
    )
    .await;
    expected.push(("INGESTED", TICKET_TYPE, true));
    assert_history(
        &emulator,
        "after a second object type's ingest",
        domain,
        &profile,
        &expected,
    )
    .await;

    let (status, body) = delete_object(&emulator, domain, TICKET_TYPE, &profile, &ticket_key).await;
    assert_eq!(status, 200, "DeleteProfileObject: expected 200, got {status} {body:?}");
    expected.push(("DELETED_BY_CUSTOMER", TICKET_TYPE, true));
    assert_history(
        &emulator,
        "after DeleteProfileObject",
        domain,
        &profile,
        &expected,
    )
    .await;

    // The merged profile's own history moves onto the profile that absorbed it, as its
    // keys and its objects do, and ends with the record the merge wrote for it.
    let absorbed = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550199" }))
        .await;
    let (status, body) = emulator
        .post(
            &format!("/domains/{domain}/profiles/objects/merge"),
            json!({ "MainProfileId": profile, "ProfileIdsToBeMerged": [absorbed] }),
        )
        .await;
    assert_eq!(status, 200, "MergeProfiles: expected 200, got {status} {body:?}");
    expected.push(("CREATED", standard, false));
    expected.push(("DELETED_BY_MERGE", standard, false));
    expected.push(("MERGED", standard, false));
    assert_history(&emulator, "after MergeProfiles", domain, &profile, &expected).await;
}

/// An object dropped because its retention ran out leaves an `EXPIRED` record performed by
/// nobody: the clock owed that removal, so no principal asked for it.
///
/// The retention is measured in days, so the stored object is aged through the same shared
/// engine the router handed the service rather than waited out.
#[tokio::test]
async fn an_expired_object_records_an_action_nobody_performed() {
    let emulator = Emulator::new();
    let domain = "expirehistdom";
    emulator.create_domain(domain, 90).await;
    put_contact_type(&emulator, domain, CONTACT_TYPE, false).await;
    let profile = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550300" }))
        .await;
    let unique_key = ingest(
        &emulator,
        domain,
        CONTACT_TYPE,
        &contact("e-1", json!({ "phone": "+15550300" }), json!({})),
    )
    .await;

    assert_eq!(
        action_types(&records(&emulator, "before the retention ran out", domain, &profile, json!({})).await),
        vec!["CREATED".to_string(), "INGESTED".to_string()],
        "before the retention ran out: expected only the actions that happened"
    );

    {
        let state = service_state(&*emulator.engine).expect("the service's store");
        let mut store = state.lock();
        let stored = store
            .domain_mut(REGION, domain)
            .expect("the domain created above")
            .profile_objects
            .get_mut(&(CONTACT_TYPE.to_string(), profile.clone(), unique_key.clone()))
            .expect("the ingested object is stored");
        stored.created_at = SystemTime::now() - Duration::from_secs(91 * SECONDS_PER_DAY);
    }

    let listed = records(&emulator, "after the retention ran out", domain, &profile, json!({})).await;
    assert_eq!(
        action_types(&listed),
        vec![
            "CREATED".to_string(),
            "INGESTED".to_string(),
            "EXPIRED".to_string()
        ],
        "after the retention ran out: expected the expiry recorded last, got {listed:?}"
    );
    let expired = listed.last().expect("the expiry record");
    assert_eq!(
        (
            member(expired, "ObjectTypeName"),
            member(expired, "ProfileObjectUniqueKey"),
            expired.get("PerformedBy").cloned(),
        ),
        (CONTACT_TYPE.to_string(), unique_key.clone(), None),
        "the expiry record: expected the object it was about and no principal, got {expired:?}"
    );

    // Reading twice reports the same history: the expiry is materialised once, not once
    // per read.
    let again = records(&emulator, "read again", domain, &profile, json!({})).await;
    assert_eq!(
        action_types(&again),
        action_types(&listed),
        "read again: expected the same history, got {again:?}"
    );
}

/// Each filter includes only the records carrying the value it names, and the filters a
/// request carries all hold at once.
#[tokio::test]
async fn each_filter_includes_only_the_records_carrying_its_value() {
    let emulator = Emulator::new();
    let domain = "filterdom";
    emulator.create_domain(domain, 90).await;
    seed_object_types(&emulator, domain).await;
    let profile = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550200" }))
        .await;
    update(&emulator, domain, &profile, json!({ "LastName": "Hopper" })).await;
    ingest(
        &emulator,
        domain,
        CONTACT_TYPE,
        &contact("f-1", json!({ "phone": "+15550200" }), json!({})),
    )
    .await;
    ingest(
        &emulator,
        domain,
        TICKET_TYPE,
        &contact("f-2", json!({ "phone": "+15550200" }), json!({})),
    )
    .await;

    let standard = STANDARD_PROFILE_OBJECT_TYPE.to_string();
    let caller = caller_principal();
    // The filter members and their targets come from the operation's own model
    // (`smithy_query(query="operation", operation="ListProfileHistoryRecords")`):
    // `ActionType` takes one `ActionType` value, `ObjectTypeName` one `typeName`, and
    // `PerformedBy` one `string1To255`.
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        ("no filter at all", json!({}), vec!["CREATED", "UPDATED", "INGESTED", "INGESTED"]),
        ("ActionType CREATED", json!({ "ActionType": "CREATED" }), vec!["CREATED"]),
        (
            "ActionType INGESTED, which two records carry",
            json!({ "ActionType": "INGESTED" }),
            vec!["INGESTED", "INGESTED"],
        ),
        (
            "ActionType no record carries",
            json!({ "ActionType": "DELETED_PROFILE_KEY" }),
            vec![],
        ),
        (
            "ObjectTypeName of the standard profile",
            json!({ "ObjectTypeName": standard.clone() }),
            vec!["CREATED", "UPDATED"],
        ),
        (
            "ObjectTypeName of one ingested type",
            json!({ "ObjectTypeName": CONTACT_TYPE }),
            vec!["INGESTED"],
        ),
        (
            "ObjectTypeName the domain never ingested under",
            json!({ "ObjectTypeName": "Never_Used" }),
            vec![],
        ),
        (
            "PerformedBy the calling principal",
            json!({ "PerformedBy": caller.clone() }),
            vec!["CREATED", "UPDATED", "INGESTED", "INGESTED"],
        ),
        (
            "PerformedBy another principal",
            json!({ "PerformedBy": "arn:aws:iam::000000000000:user/someone-else" }),
            vec![],
        ),
        (
            "both an ActionType and an ObjectTypeName",
            json!({ "ActionType": "INGESTED", "ObjectTypeName": TICKET_TYPE }),
            vec!["INGESTED"],
        ),
        (
            "two filters no one record satisfies together",
            json!({ "ActionType": "CREATED", "ObjectTypeName": CONTACT_TYPE }),
            vec![],
        ),
    ];

    for (case, filters, expected) in rows {
        let listed = records(&emulator, case, domain, &profile, filters.clone()).await;
        assert_eq!(
            action_types(&listed),
            expected.iter().map(|action| (*action).to_string()).collect::<Vec<String>>(),
            "case {case:?}: expected {expected:?} for {filters:?}, got {listed:?}"
        );
        if let Some(wanted) = filters.get("ObjectTypeName").and_then(Value::as_str) {
            for record in &listed {
                assert_eq!(
                    member(record, "ObjectTypeName"),
                    wanted,
                    "case {case:?}: expected every record to carry {wanted:?}, got {record:?}"
                );
            }
        }
    }
}

/// One page carries at most `MaxResults` records and its `NextToken` resumes exactly where
/// it stopped, so walking the pages of any page size yields the whole history in order,
/// once each.
#[tokio::test]
async fn a_page_size_walks_the_whole_history_in_order() {
    let emulator = Emulator::new();
    let domain = "pagedom";
    emulator.create_domain(domain, 90).await;
    let profile = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550400" }))
        .await;
    // Five actions in all: the create plus four writes, so a page size of 1, 2, 4, 5 and
    // the model's maximum each splits the history differently.
    for suffix in ["a", "b", "c", "d"] {
        let (status, body) = update(
            &emulator,
            domain,
            &profile,
            json!({ "LastName": format!("Lamarr-{suffix}") }),
        )
        .await;
        assert_eq!(status, 200, "seeding an update: expected 200, got {status} {body:?}");
    }

    let whole = records(&emulator, "the whole history", domain, &profile, json!({})).await;
    let expected: Vec<String> = whole.iter().map(|record| member(record, "Id")).collect();
    assert_eq!(
        expected.len(),
        5,
        "the whole history: expected the five actions taken, got {whole:?}"
    );

    for page_size in [1_i64, 2, 4, 5, MAX_PAGE] {
        let case = format!("page size {page_size}");
        let walked = every_record(&emulator, &case, domain, &profile, page_size).await;
        let ids: Vec<String> = walked.iter().map(|record| member(record, "Id")).collect();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected the whole history once, in order, got {ids:?}"
        );
    }

    // A page that ends the history offers no token to resume from.
    let (status, body) = list_history(
        &emulator,
        domain,
        &profile,
        json!({}),
        &format!("?max-results={MAX_PAGE}"),
    )
    .await;
    assert_eq!(status, 200, "the last page: expected 200, got {status} {body:?}");
    assert_eq!(
        body.get("NextToken"),
        None,
        "the last page: expected no NextToken, got {body:?}"
    );

    // A token this service did not issue names a position it never offered.
    for (case, token) in [
        ("text that is not a token at all", "not-a-token"),
        ("hexadecimal of the right alphabet", "00112233445566778899aabbccddeeff"),
    ] {
        let (status, body) = list_history(
            &emulator,
            domain,
            &profile,
            json!({}),
            &format!("?next-token={token}"),
        )
        .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {status} {body:?}"
        );
        assert_eq!(
            error_message(&body),
            "ListProfileHistoryRecords failed due to the nextToken or the maxResults is invalid",
            "case {case:?}: expected the pagination refusal, got {body:?}"
        );
    }
}

/// A listing answers from the store, so a domain or profile the store does not hold is the
/// absent resource rather than an empty page — including a profile whose deletion took its
/// history with it. The sentence is the one this service reports that resource's absence
/// with wherever it is addressed, so the same unheld name or id reads the same whichever
/// operation was called.
#[tokio::test]
async fn a_domain_or_profile_the_store_does_not_hold_is_not_found() {
    let emulator = Emulator::new();
    let domain = "refusedom";
    emulator.create_domain(domain, 90).await;
    let profile = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550500" }))
        .await;
    let deleted = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550501" }))
        .await;
    let (status, body) = emulator.delete_profile(domain, &deleted).await;
    assert_eq!(status, 200, "DeleteProfile: expected 200, got {status} {body:?}");

    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "a domain nothing created",
            "nosuchdom",
            profile.clone(),
            "Domain nosuchdom is not found".to_string(),
        ),
        (
            "a profile id nothing created",
            domain,
            unknown_id(1),
            profile_not_found(&unknown_id(1)),
        ),
        (
            "another unheld profile id",
            domain,
            unknown_id(2),
            profile_not_found(&unknown_id(2)),
        ),
        (
            "a profile whose delete took its history",
            domain,
            deleted.clone(),
            profile_not_found(&deleted),
        ),
    ];

    for (case, listed_domain, listed_profile, expected) in rows {
        let (status, body) =
            list_history(&emulator, listed_domain, &listed_profile, json!({}), "").await;
        assert_eq!(
            (status, error_code(&body)),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} {body:?}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the absent-resource sentence, got {body:?}"
        );
    }

    // The profile that was NOT deleted still answers from its own history.
    assert_eq!(
        action_types(&records(&emulator, "the profile still held", domain, &profile, json!({})).await),
        vec!["CREATED".to_string()],
        "the profile still held: expected its own creation recorded"
    );
}

/// A profile's history is bounded: past the store's cap the OLDEST record goes as the
/// newest arrives, so a long-lived profile cannot grow the store without bound and the
/// listing keeps serving the most recent actions.
///
/// The actions are recorded through the service's own store rather than through the cap
/// many HTTP calls, which is what keeps this test fast.
#[tokio::test]
async fn a_profiles_history_is_bounded_by_dropping_its_oldest_records() {
    let emulator = Emulator::new();
    let domain = "capdom";
    emulator.create_domain(domain, 90).await;
    let profile = emulator
        .create_profile(domain, json!({ "PhoneNumber": "+15550600" }))
        .await;

    // The oldest records carry an action type no later one does, so a listing that still
    // reported one would be reporting a record the cap should have dropped.
    let overflow = 5;
    {
        let state = service_state(&*emulator.engine).expect("the service's store");
        let mut store = state.lock();
        let held = store
            .domain_mut(REGION, domain)
            .expect("the domain created above");
        for _ in 0..overflow {
            held.record_profile_action(
                &profile,
                ProfileAction::on_profile(ACTION_ADDED_PROFILE_KEY),
                SystemTime::now(),
            );
        }
        for _ in 0..MAX_PROFILE_HISTORY_RECORDS {
            held.record_profile_action(
                &profile,
                ProfileAction::on_profile(ACTION_UPDATED),
                SystemTime::now(),
            );
        }
    }

    let walked = every_record(&emulator, "the bounded history", domain, &profile, MAX_PAGE).await;
    assert_eq!(
        walked.len(),
        MAX_PROFILE_HISTORY_RECORDS,
        "the bounded history: expected at most {MAX_PROFILE_HISTORY_RECORDS} records, got {}",
        walked.len()
    );
    assert!(
        action_types(&walked)
            .iter()
            .all(|action| action == ACTION_UPDATED),
        "the bounded history: expected only the most recent actions, got {:?}",
        action_types(&walked).first()
    );
}
