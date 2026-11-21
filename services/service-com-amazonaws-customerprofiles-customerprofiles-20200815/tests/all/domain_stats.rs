// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetDomain`'s `Stats`.
//!
//! The rule under test: every member of `DomainStats` is counted off the data the domain
//! itself holds, so each one moves with the writes that put data in the domain and takes it
//! out again. `ProfileCount` is the profiles the domain holds; each profile is held as one
//! object of the standard profile model, so `ObjectCount` ("The total number of objects in
//! domain") counts every profile once beside every object ingested under one of the
//! domain's object types; `TotalSize` ("The total size, in bytes, of all objects in the
//! domain") grows with the bytes those objects carry; and `MeteringProfileCount` is the
//! metered view of the profile count.
//!
//! The assertions are a SWEEP over a sequence of writes rather than one recorded shape, and
//! `TotalSize` is asserted as a size — zero when the domain is empty, strictly larger when
//! more data is stored and smaller again when data goes — never as one run's number.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::domain_profile_lifecycle::{Emulator, REGION};
use crate::profile_objects::{contact, ingest, put_contact_type};

/// The four members `DomainStats` declares, as a response reports them.
#[derive(Debug, PartialEq, Eq)]
struct Stats {
    profile_count: i64,
    metering_profile_count: i64,
    object_count: i64,
    total_size: i64,
}

/// `GetDomain`'s `Stats`, asserting the read succeeded and that every member is reported.
async fn stats(emulator: &Emulator, case: &str, domain: &str) -> Stats {
    let (status, body) = emulator.send("GET", &format!("/domains/{domain}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetDomain 200, got {status} {body:?}"
    );
    let member = |name: &str| {
        body.pointer(&format!("/Stats/{name}"))
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: no Stats.{name} in {body:?}"))
    };
    Stats {
        profile_count: member("ProfileCount"),
        metering_profile_count: member("MeteringProfileCount"),
        object_count: member("ObjectCount"),
        total_size: member("TotalSize"),
    }
}

/// The rule itself: the counts follow the domain's stored data through a sequence of
/// writes, and the size follows the bytes those writes stored.
#[tokio::test]
async fn the_domain_stats_are_counted_off_the_data_the_domain_holds() {
    let emulator = Emulator::new();
    emulator.create_domain("statsdom", 90).await;
    put_contact_type(&emulator, "statsdom", "Contact_Record", true).await;

    // A fresh domain holds nothing, so every statistic is at its default.
    let empty = stats(&emulator, "a fresh domain", "statsdom").await;
    assert_eq!(
        empty,
        Stats {
            profile_count: 0,
            metering_profile_count: 0,
            object_count: 0,
            total_size: 0,
        },
        "case \"a fresh domain\": expected every statistic at zero"
    );

    // Each profile is one object, and the bytes it carries are part of the domain's size.
    let first = emulator
        .create_profile(
            "statsdom",
            json!({ "AccountNumber": "A-1", "PhoneNumber": "+15550100" }),
        )
        .await;
    let one = stats(&emulator, "one profile", "statsdom").await;
    assert_eq!(
        (one.profile_count, one.metering_profile_count, one.object_count),
        (1, 1, 1),
        "case \"one profile\": expected one profile and the one object it is, got {one:?}"
    );
    assert!(
        one.total_size > 0,
        "case \"one profile\": expected the stored profile to have a size, got {one:?}"
    );

    emulator
        .create_profile(
            "statsdom",
            json!({
                "AccountNumber": "A-2",
                "FirstName": "Ada",
                "LastName": "Lovelace",
                "Attributes": { "tier": "gold" },
            }),
        )
        .await;
    let two = stats(&emulator, "two profiles", "statsdom").await;
    assert_eq!(
        (two.profile_count, two.metering_profile_count, two.object_count),
        (2, 2, 2),
        "case \"two profiles\": expected two profiles and two objects, got {two:?}"
    );
    assert!(
        two.total_size > one.total_size,
        "case \"two profiles\": expected the size to grow with the second profile, got \
         {two:?} after {one:?}"
    );

    // An ingested object is an object of the domain beside the profile it is tied to, and
    // it is tied to the first profile by the phone number both carry, so no profile is
    // created.
    for contact_id in ["c-1", "c-2"] {
        ingest(
            &emulator,
            "statsdom",
            "Contact_Record",
            &contact(contact_id, json!({ "phone": "+15550100" }), json!({})),
        )
        .await;
    }
    let ingested = stats(&emulator, "two ingested objects", "statsdom").await;
    assert_eq!(
        (
            ingested.profile_count,
            ingested.metering_profile_count,
            ingested.object_count
        ),
        (2, 2, 4),
        "case \"two ingested objects\": expected the two profiles and four objects, got \
         {ingested:?}"
    );
    assert!(
        ingested.total_size > two.total_size,
        "case \"two ingested objects\": expected the size to grow with the objects, got \
         {ingested:?} after {two:?}"
    );

    // A read changes nothing a later read sees.
    assert_eq!(
        stats(&emulator, "read twice", "statsdom").await,
        ingested,
        "case \"read twice\": expected two reads of an unchanged domain to agree"
    );

    // Deleting a profile takes the profile and the objects tied to it out of every count.
    let (status, body) = emulator.delete_profile("statsdom", &first).await;
    assert_eq!(status, 200, "DeleteProfile: expected 200, got {status} {body:?}");
    let after_delete = stats(&emulator, "after a delete", "statsdom").await;
    assert_eq!(
        (
            after_delete.profile_count,
            after_delete.metering_profile_count,
            after_delete.object_count
        ),
        (1, 1, 1),
        "case \"after a delete\": expected the deleted profile and its objects to be gone, \
         got {after_delete:?}"
    );
    assert!(
        after_delete.total_size < ingested.total_size && after_delete.total_size > 0,
        "case \"after a delete\": expected the size to fall to what is still held, got \
         {after_delete:?} after {ingested:?}"
    );

    // One domain's data is not another's: a second domain holding one profile reports its
    // own counts.
    emulator.create_domain("otherstats", 90).await;
    emulator
        .create_profile("otherstats", json!({ "AccountNumber": "B-1" }))
        .await;
    let other = stats(&emulator, "another domain", "otherstats").await;
    assert_eq!(
        (other.profile_count, other.object_count),
        (1, 1),
        "case \"another domain\": expected only its own profile, got {other:?}"
    );
}

/// `MeteringProfileCount` is the profile count metered per hundred objects a profile
/// carries BEYOND the first hundred: "If you have more than 100 objects associated with a
/// single profile, that profile counts as two profiles. If you have more than 200 objects,
/// that profile counts as three, and so on." — so exactly a hundred objects is still one
/// profile, and the hundred-and-first is what makes it two.
///
/// The state is seeded directly rather than through a hundred ingest calls: what the rule
/// is about is how the count is DERIVED from the objects a profile holds, and driving that
/// over the wire would only make the test slow.
#[tokio::test]
async fn metering_counts_a_profile_once_more_per_hundred_objects_past_the_first_hundred() {
    use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
        service_state, StoredProfileObject, OBJECTS_PER_METERED_PROFILE,
    };
    use std::time::SystemTime;

    let emulator = Emulator::new();
    emulator.create_domain("meterdom", 90).await;
    put_contact_type(&emulator, "meterdom", "Contact_Record", true).await;
    let profile_id = emulator
        .create_profile("meterdom", json!({ "AccountNumber": "M-1" }))
        .await;

    // (case, the objects the profile carries, the profiles it is metered as). The rows sit
    // on each side of both published thresholds.
    let rows = [
        ("none", 0usize, 1i64),
        ("one short of the hundred", OBJECTS_PER_METERED_PROFILE - 1, 1),
        ("exactly a hundred, which is not MORE than a hundred", OBJECTS_PER_METERED_PROFILE, 1),
        ("one past the hundred", OBJECTS_PER_METERED_PROFILE + 1, 2),
        ("exactly two hundred", 2 * OBJECTS_PER_METERED_PROFILE, 2),
        ("one past two hundred", 2 * OBJECTS_PER_METERED_PROFILE + 1, 3),
    ];
    for (case, held, metered) in rows {
        {
            let state = service_state(&*emulator.engine).expect("this service's store");
            let mut store = state.lock();
            let domain = store.domain_mut(REGION, "meterdom").expect("the domain");
            domain.profile_objects.clear();
            let now = SystemTime::now();
            for index in 0..held {
                domain.put_profile_object(StoredProfileObject {
                    object_type_name: "Contact_Record".to_string(),
                    unique_key: format!("seeded-{index}"),
                    object: json!({ "contactId": format!("c-{index}") }).to_string(),
                    profile_id: profile_id.clone(),
                    index_values: Default::default(),
                    created_at: now,
                    last_updated_at: now,
                });
            }
        }
        let reported = stats(&emulator, case, "meterdom").await;
        assert_eq!(
            reported.metering_profile_count, metered,
            "case {case:?} ({held} objects): expected MeteringProfileCount {metered}, got \
             {reported:?}"
        );
        assert_eq!(
            reported.profile_count, 1,
            "case {case:?}: expected the one profile it holds, got {reported:?}"
        );
        assert_eq!(
            reported.object_count,
            1 + i64::try_from(held).expect("a count"),
            "case {case:?}: expected the profile and its objects, got {reported:?}"
        );
    }
}
