// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListCapacityReservations`: what the account's capacity-reservation
//! listing reports, driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots, so every assertion is about what an SDK
//! client observes.
//!
//! The rule under test: a listing reports every capacity reservation the account holds,
//! with the values the writes persisted and the status and allocated amount the stored
//! instants imply, in name order, in pages of at most `MaxResults` entries, each page but
//! the last followed by a `NextToken` this service minted and no other value.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along what
//! the rule generalises over — how many reservations the account holds, where each name
//! sorts, the requested number of data processing units, the point of the lifecycle each
//! reservation is at, the page size asked for, and the form of a continuation token a
//! caller sends back. Nothing here waits on the wall clock: a reservation that must be at
//! a given point of its lifecycle is seeded with the instants that put it there.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, page_token_malformed, CapacityReservationRecord, CANCELLED_CAPACITY_RESERVATION_RETENTION,
    CAPACITY_ALLOCATION, CAPACITY_RESERVATION_STATUS_ACTIVE,
    CAPACITY_RESERVATION_STATUS_CANCELLED, CAPACITY_RESERVATION_STATUS_CANCELLING,
    CAPACITY_RESERVATION_STATUS_PENDING, CAPACITY_RESERVATION_STATUS_UPDATE_PENDING,
    ERROR_CODE_INVALID_INPUT,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router, error_code, message, router};

/// The smallest number of data processing units a request may ask for: the floor of the
/// modelled `TargetDpusInteger` range (4..).
const MODELLED_FLOOR_TARGET_DPUS: i32 = 4;

/// The amount a row that says nothing about the amount asks for: inside the modelled range
/// and above its floor.
const SOME_TARGET_DPUS: i32 = 24;

/// A span long enough that a reservation seeded as still allocating, or still cancelling,
/// stays that way for as long as any test run takes: the transition is due an hour out, so
/// no assertion here depends on how fast the host runs.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The entries a `ListCapacityReservations` answer carries.
fn reservations(listed: &Value) -> Vec<Value> {
    listed["CapacityReservations"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!("a listing must carry CapacityReservations, got {listed:?}")
        })
}

/// The names those entries carry, in the order the listing reported them.
fn names(listed: &Value) -> Vec<String> {
    reservations(listed)
        .iter()
        .map(|entry| {
            entry["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("every entry names its reservation, got {entry:?}"))
                .to_string()
        })
        .collect()
}

/// The `NextToken` a page handed back, if it handed one back.
fn next_token(listed: &Value) -> Option<String> {
    listed["NextToken"].as_str().map(str::to_string)
}

/// One `ListCapacityReservations` call that must be accepted.
async fn list(router: &MultiServiceRouter, request: &Value) -> Value {
    let (status, listed) = call(router, "ListCapacityReservations", request).await;
    assert_eq!(
        status, 200,
        "ListCapacityReservations({request}) must answer 200, got {status} with {listed:?}"
    );
    listed
}

/// The entry the listing carries for `name`, if it carries one.
fn listed_entry(listed: &Value, name: &str) -> Option<Value> {
    reservations(listed)
        .into_iter()
        .find(|entry| entry["Name"].as_str() == Some(name))
}

/// `GetCapacityReservation`'s view of one reservation.
async fn read_reservation(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetCapacityReservation", &json!({ "Name": name })).await;
    (status, body["CapacityReservation"].clone())
}

/// Create one reservation over the wire and assert it was accepted.
async fn create(router: &MultiServiceRouter, name: &str, target_dpus: i32, tags: &Value) {
    let mut request = json!({ "Name": name, "TargetDpus": target_dpus });
    if !tags.is_null() {
        request["Tags"] = tags.clone();
    }
    let (status, body) = call(router, "CreateCapacityReservation", &request).await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateCapacityReservation({request}) must be accepted, got {status} \
         with {body:?}"
    );
}

/// An account nobody has written to holds no capacity reservations, and says so with the
/// empty list the required member takes rather than a refusal or an absent member.
#[tokio::test]
async fn a_fresh_account_lists_no_capacity_reservations() {
    let router = router();

    // Each row is a request a client may send for the same empty account: no members, a
    // page size of one, and the modelled maximum page size.
    for case in [json!({}), json!({ "MaxResults": 1 }), json!({ "MaxResults": 50 })] {
        let listed = list(&router, &case).await;
        assert_eq!(
            reservations(&listed),
            Vec::<Value>::new(),
            "case {case}: an account that holds no reservation reports the empty list, got \
             {listed:?}"
        );
        assert_eq!(
            next_token(&listed),
            None,
            "case {case}: a listing with nothing behind it hands back no token, got {listed:?}"
        );
    }
}

/// One row of the created-reservation table.
struct Row {
    /// What this row varies.
    case: &'static str,
    /// The name the create call carried.
    name: String,
    /// The requested number of data processing units it carried.
    target_dpus: i32,
    /// The `Tags` member it carried (`Value::Null` = absent).
    tags: Value,
}

/// The rows of that table: the requested amount at the modelled floor and well above it,
/// names at both ends of the modelled `CapacityReservationName` length (1 to 128) and over
/// its whole character set (`^[a-zA-Z0-9._-]+$`), names sorting at either end of the
/// listing, and a create with and without tags.
fn created_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the smallest amount the model admits, no tags",
            name: "modelled-floor".to_string(),
            target_dpus: MODELLED_FLOOR_TARGET_DPUS,
            tags: Value::Null,
        },
        Row {
            case: "an amount well above that floor, with tags",
            name: "above.the.floor".to_string(),
            target_dpus: 256,
            tags: json!([
                { "Key": "team", "Value": "platform" },
                { "Key": "cost-centre", "Value": "4711" },
            ]),
        },
        Row {
            case: "a name of one character, which sorts ahead of the longer ones",
            name: "A".to_string(),
            target_dpus: SOME_TARGET_DPUS,
            tags: Value::Null,
        },
        Row {
            case: "a name over the whole modelled character set",
            name: "aA0._-".to_string(),
            target_dpus: SOME_TARGET_DPUS,
            tags: json!([{ "Key": "purpose", "Value": "character-set" }]),
        },
        Row {
            case: "a name of the longest modelled form",
            name: "z".repeat(128),
            target_dpus: SOME_TARGET_DPUS,
            tags: Value::Null,
        },
    ]
}

/// Every reservation a create registered is reported, with the values that create
/// persisted, in name order — and each entry is the one a read of that reservation by name
/// answers with.
#[tokio::test]
async fn every_created_reservation_is_listed_with_what_the_create_persisted() {
    let router = router();
    let rows = created_rows();
    for row in &rows {
        create(&router, &row.name, row.target_dpus, &row.tags).await;
    }

    let listed = list(&router, &json!({ "MaxResults": 50 })).await;

    let mut expected: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
    expected.sort();
    assert_eq!(
        names(&listed),
        expected,
        "expected every reservation the account holds, in name order: {expected:?}, got \
         {listed:?}"
    );

    for row in &rows {
        let entry = listed_entry(&listed, &row.name).unwrap_or_else(|| {
            panic!("case {:?}: {} must be listed, got {listed:?}", row.case, row.name)
        });
        assert_eq!(
            entry["TargetDpus"].as_i64(),
            Some(i64::from(row.target_dpus)),
            "case {:?}: expected the requested amount {} to round-trip, got {entry:?}",
            row.case,
            row.target_dpus
        );
        assert!(
            entry["CreationTime"].as_f64().is_some(),
            "case {:?}: expected the entry to report when the reservation was created, got \
             {entry:?}",
            row.case
        );
        // The allocation of a freshly created reservation is a transition the clock owes,
        // so either point of it is correct here; what it may not be is any other status.
        let status = entry["Status"].as_str().unwrap_or("<no Status>");
        assert!(
            status == CAPACITY_RESERVATION_STATUS_PENDING
                || status == CAPACITY_RESERVATION_STATUS_ACTIVE,
            "case {:?}: expected {CAPACITY_RESERVATION_STATUS_PENDING} or \
             {CAPACITY_RESERVATION_STATUS_ACTIVE} for a created reservation, got {entry:?}",
            row.case
        );

        // The listing reports the same reservation a read of it by name does: the stored
        // record is the one source for both.
        let (status_code, read) = read_reservation(&router, &row.name).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: the listed reservation must be readable by name, got {status_code} \
             with {read:?}",
            row.case
        );
        assert_eq!(
            entry["CreationTime"], read["CreationTime"],
            "case {:?}: expected the listing and the read to report one creation instant, \
             got {entry:?} and {read:?}",
            row.case
        );
        assert_eq!(
            entry["TargetDpus"], read["TargetDpus"],
            "case {:?}: expected the listing and the read to report one requested amount, \
             got {entry:?} and {read:?}",
            row.case
        );
    }
}

/// One seeded reservation: a record written straight into the store with the instants that
/// put it at a given point of its lifecycle, and what a listing must then report for it.
struct Seeded {
    case: &'static str,
    name: &'static str,
    target_dpus: i32,
    /// The status the stored instants imply.
    status: &'static str,
    /// The number of data processing units they imply is allocated.
    allocated_dpus: i32,
    /// Whether the account still holds the reservation at all.
    listed: bool,
}

/// Seed `rows`' reservations into `engine`'s store, each with the instants its row
/// describes, and hand back what each one must then be reported as.
fn seed(engine: &engine::InMemoryEngine) -> Vec<Seeded> {
    let now = SystemTime::now();
    let long_ago = now - WELL_PAST_ANY_RUN;
    let store = state::service_state(engine).expect("this service's state");
    let mut held = store.lock();

    // Allocated long ago: both its allocation and the instant it was due are behind us.
    held.capacity_reservations
        .insert(CapacityReservationRecord::created(
            "allocated".to_string(),
            64,
            Vec::new(),
            long_ago,
        ));

    // Still allocating its FIRST amount: the allocation is due an hour out, so it holds
    // none of it yet.
    let mut allocating =
        CapacityReservationRecord::created("allocating".to_string(), 48, Vec::new(), now);
    allocating.allocation_complete_at = now + WELL_PAST_ANY_RUN;
    held.capacity_reservations.insert(allocating);

    // Allocated once and now allocating a NEW amount: it holds what the first allocation
    // reached until the second one is due.
    let mut updating =
        CapacityReservationRecord::created("updating".to_string(), 16, Vec::new(), long_ago);
    updating.request_allocation(96, now);
    updating.allocation_complete_at = now + WELL_PAST_ANY_RUN;
    held.capacity_reservations.insert(updating);

    // Cancelled just now: the cancellation is due an hour out, so it is still cancelling
    // and still holds its capacity.
    let mut cancelling =
        CapacityReservationRecord::created("cancelling".to_string(), 32, Vec::new(), long_ago);
    cancelling.cancel(now);
    cancelling.cancel_complete_at = Some(now + WELL_PAST_ANY_RUN);
    held.capacity_reservations.insert(cancelling);

    // Cancelled long enough ago that the cancellation is complete, but well inside the
    // documented retention of a cancelled reservation.
    let mut cancelled =
        CapacityReservationRecord::created("cancelled".to_string(), 128, Vec::new(), long_ago);
    cancelled.cancel(long_ago + CAPACITY_ALLOCATION);
    held.capacity_reservations.insert(cancelled);

    // Cancelled longer ago than that retention: the account no longer holds it.
    let retired_at = now - (CANCELLED_CAPACITY_RESERVATION_RETENTION * 2);
    let mut retired =
        CapacityReservationRecord::created("retired".to_string(), 8, Vec::new(), retired_at);
    retired.cancel(retired_at + CAPACITY_ALLOCATION);
    held.capacity_reservations.insert(retired);

    vec![
        Seeded {
            case: "an allocation the clock completed",
            name: "allocated",
            target_dpus: 64,
            status: CAPACITY_RESERVATION_STATUS_ACTIVE,
            allocated_dpus: 64,
            listed: true,
        },
        Seeded {
            case: "a first allocation still in flight",
            name: "allocating",
            target_dpus: 48,
            status: CAPACITY_RESERVATION_STATUS_PENDING,
            allocated_dpus: 0,
            listed: true,
        },
        Seeded {
            case: "a new amount still being allocated",
            name: "updating",
            target_dpus: 96,
            status: CAPACITY_RESERVATION_STATUS_UPDATE_PENDING,
            allocated_dpus: 16,
            listed: true,
        },
        Seeded {
            case: "a cancellation still in flight",
            name: "cancelling",
            target_dpus: 32,
            status: CAPACITY_RESERVATION_STATUS_CANCELLING,
            allocated_dpus: 32,
            listed: true,
        },
        Seeded {
            case: "a cancellation the clock completed, inside the retention",
            name: "cancelled",
            target_dpus: 128,
            status: CAPACITY_RESERVATION_STATUS_CANCELLED,
            allocated_dpus: 0,
            listed: true,
        },
        Seeded {
            case: "a cancelled reservation past its retention",
            name: "retired",
            target_dpus: 8,
            status: CAPACITY_RESERVATION_STATUS_CANCELLED,
            allocated_dpus: 0,
            listed: false,
        },
    ]
}

/// A listing reports each reservation at the point of its lifecycle the stored instants
/// imply — the same point a read of it by name reports — and one whose cancelled retention
/// elapsed is absent from it. Listing is a READ: a second listing reports what the first
/// one did.
#[tokio::test]
async fn a_listing_reports_each_reservation_as_its_stored_instants_imply() {
    let (engine, router) = engine_and_router();
    let rows = seed(&engine);

    let listed = list(&router, &json!({ "MaxResults": 50 })).await;

    let mut expected: Vec<String> = rows
        .iter()
        .filter(|row| row.listed)
        .map(|row| row.name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        names(&listed),
        expected,
        "expected the reservations the account still holds, in name order: {expected:?}, \
         got {listed:?}"
    );

    for row in &rows {
        let entry = listed_entry(&listed, row.name);
        if !row.listed {
            assert!(
                entry.is_none(),
                "case {:?}: {} has left the account, so no listing reports it, got {entry:?}",
                row.case,
                row.name
            );
            continue;
        }
        let entry = entry.unwrap_or_else(|| {
            panic!("case {:?}: {} must be listed, got {listed:?}", row.case, row.name)
        });
        assert_eq!(
            entry["Status"].as_str(),
            Some(row.status),
            "case {:?}: expected the status {}, got {entry:?}",
            row.case,
            row.status
        );
        assert_eq!(
            entry["TargetDpus"].as_i64(),
            Some(i64::from(row.target_dpus)),
            "case {:?}: expected {} requested, got {entry:?}",
            row.case,
            row.target_dpus
        );
        assert_eq!(
            entry["AllocatedDpus"].as_i64(),
            Some(i64::from(row.allocated_dpus)),
            "case {:?}: expected {} allocated, got {entry:?}",
            row.case,
            row.allocated_dpus
        );

        let (status_code, read) = read_reservation(&router, row.name).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: the listed reservation must be readable by name, got {status_code} \
             with {read:?}",
            row.case
        );
        assert_eq!(
            entry, read,
            "case {:?}: expected the listing and the read to report one reservation, got \
             {entry:?} and {read:?}",
            row.case
        );
    }

    // A listing REPORTS: nothing it answered moved any reservation on.
    let again = list(&router, &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        again, listed,
        "two listings of the same account must report the same reservations, got {again:?} \
         after {listed:?}"
    );
}

/// A paged walk reports every reservation exactly once, in pages of at most the size asked
/// for, and hands back a token exactly while reservations remain behind the page.
#[tokio::test]
async fn a_paged_listing_walks_every_reservation_exactly_once() {
    let (engine, router) = engine_and_router();

    // Seven reservations, seeded with an allocation already behind them so no entry can
    // change between pages, and named so the sorted order is not the order they were
    // written in.
    let long_ago = SystemTime::now() - WELL_PAST_ANY_RUN;
    let held: Vec<String> = [3_usize, 1, 6, 0, 4, 2, 5]
        .iter()
        .map(|index| format!("paged-{index}"))
        .collect();
    {
        let store = state::service_state(&*engine).expect("this service's state");
        let mut store = store.lock();
        for name in &held {
            store
                .capacity_reservations
                .insert(CapacityReservationRecord::created(
                    name.clone(),
                    SOME_TARGET_DPUS,
                    Vec::new(),
                    long_ago,
                ));
        }
    }
    let mut expected = held.clone();
    expected.sort();
    let total = expected.len();

    // Each row is a page size: one at a time, sizes that divide the total and sizes that
    // leave a remainder, the total itself, one past it, and the modelled maximum. The last
    // row asks for no size at all, which is the page a request naming none gets.
    for case in [Some(1), Some(2), Some(3), Some(7), Some(8), Some(50), None] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut request = json!({});
            if let Some(size) = case {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let listed = list(&router, &request).await;
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case:?}: a token walk must terminate, walked {walked:?}"
            );

            let page = names(&listed);
            let page_size = case.unwrap_or(50).min(50) as usize;
            let remaining = total - walked.len();
            assert_eq!(
                page.len(),
                page_size.min(remaining),
                "case {case:?}: expected a page of {} after {walked:?}, got {page:?}",
                page_size.min(remaining)
            );
            walked.extend(page);

            token = next_token(&listed);
            let more = walked.len() < total;
            assert_eq!(
                token.is_some(),
                more,
                "case {case:?}: expected a NextToken to be present={more} after {walked:?}, \
                 got {listed:?}"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: expected the walk to report every reservation once, in name \
             order: {expected:?}, got {walked:?}"
        );
        assert_eq!(
            pages,
            total.div_ceil(case.unwrap_or(50).min(50) as usize),
            "case {case:?}: expected the walk to take {} pages, took {pages}",
            total.div_ceil(case.unwrap_or(50).min(50) as usize)
        );
    }
}

/// `MaxResults` is "the maximum number of results to return" within the modelled
/// `MaxCapacityReservationsCount` range (1..50): each published bound is a page size, and
/// one step past either bound is refused rather than answered with a page.
#[tokio::test]
async fn the_page_size_is_bounded_by_the_modelled_range() {
    let (engine, router) = engine_and_router();

    // Two reservations, so a page of one leaves something behind it and a page of fifty
    // reports both.
    let long_ago = SystemTime::now() - WELL_PAST_ANY_RUN;
    {
        let store = state::service_state(&*engine).expect("this service's state");
        let mut store = store.lock();
        for name in ["bounded-a", "bounded-b"] {
            store
                .capacity_reservations
                .insert(CapacityReservationRecord::created(
                    name.to_string(),
                    SOME_TARGET_DPUS,
                    Vec::new(),
                    long_ago,
                ));
        }
    }

    // (the page size asked for, how many entries it may report)
    for (size, reported) in [(1, 1), (2, 2), (50, 2)] {
        let listed = list(&router, &json!({ "MaxResults": size })).await;
        assert_eq!(
            reservations(&listed).len(),
            reported,
            "case {size}: expected {reported} entries at the published bound, got {listed:?}"
        );
    }

    // (case, the page size asked for, the clause the refusal must state)
    for (case, size, clause) in [
        (
            "one below the modelled floor",
            0,
            "Member must have value greater than or equal to 1",
        ),
        (
            "one past the modelled ceiling",
            51,
            "Member must have value less than or equal to 50",
        ),
    ] {
        let (status, refused) = call(
            &router,
            "ListCapacityReservations",
            &json!({ "MaxResults": size }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for MaxResults {size}, got {status} with \
             {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        let expected =
            format!("1 validation error detected: Value at 'maxResults' failed to satisfy constraint: {clause}");
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {refused:?}"
        );
    }
}

/// A `NextToken` is "a token generated by the Athena service": a value this service did not
/// mint for THIS listing names no page, and is refused rather than answered with one.
#[tokio::test]
async fn a_next_token_this_service_did_not_mint_names_no_page() {
    let account = router();
    for index in 0..3 {
        create(&account, &format!("token-case-{index}"), SOME_TARGET_DPUS, &Value::Null).await;
    }

    // A token another store minted for the same listing: another account's registry, so
    // another secret.
    let other_account = router();
    for index in 0..3 {
        create(
            &other_account,
            &format!("token-case-{index}"),
            SOME_TARGET_DPUS,
            &Value::Null,
        )
        .await;
    }
    let other_store_token = next_token(&list(&other_account, &json!({ "MaxResults": 1 })).await)
        .expect("a page of one of three reservations hands back a token");

    // A token THIS store minted for a DIFFERENT listing: the account's workgroups, which
    // resume at a position in another sequence entirely.
    let work_group_token = {
        for index in 0..2 {
            crate::harness::create_work_group(&account, &format!("token-case-{index}")).await;
        }
        let (status, listed) = call(&account, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
        assert_eq!(
            status, 200,
            "the workgroup listing must be readable, got {status} with {listed:?}"
        );
        next_token(&listed).expect("a page of one of the account's workgroups hands back a token")
    };

    // (case, the token sent)
    for (case, token) in [
        ("a value that is not a token at all", "made-up-token".to_string()),
        ("an empty-looking hex value", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        ("a token another store minted for this listing", other_store_token),
        ("a token this store minted for another listing", work_group_token),
    ] {
        let (status, refused) = call(
            &account,
            "ListCapacityReservations",
            &json!({ "MaxResults": 2, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            page_token_malformed(&token),
            "case {case:?}: expected the sentence {:?}, got {refused:?}",
            page_token_malformed(&token)
        );
        assert_eq!(
            refused["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected AthenaErrorCode {ERROR_CODE_INVALID_INPUT:?}, got \
             {refused:?}"
        );
    }

    // The refusals left the listing itself alone.
    let listed = list(&account, &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        names(&listed).len(),
        3,
        "expected the three reservations the creates registered, got {listed:?}"
    );
}
