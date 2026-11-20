// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the FORM a Reserved Instance Marketplace listing id is read in,
//! driven over the wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. A VALUE OUTSIDE THE PUBLISHED LISTING-ID PATTERN IS REFUSED FOR THE CONSTRAINT IT
//!    VIOLATES, before anything is looked up: HTTP 400 `InvalidInput` carrying the parameter
//!    layer's one-clause envelope — the value the request sent, the member that validator
//!    paths a listing id by, and the pattern itself — so a caller that sent something that is
//!    not an id learns that rather than being told the id names no listing.
//! 2. A VALUE INSIDE THAT PATTERN THAT THE OPERATION CANNOT ACT ON is the OTHER answer: the
//!    same `InvalidInput` code with the Marketplace's own ` Invalid value for 'listingId'`,
//!    which says only that the value is unusable — an id that resolves to no listing of the
//!    account, and one naming a listing that is no longer on sale, are the same thing to this
//!    operation.
//! 3. THE TWO ANSWERS ARE ABOUT THE VALUE AND THE STATE, NOT ABOUT THE OPERATION REFUSING
//!    EVERYTHING: the id of a listing that IS on sale is acted on — the listing is withdrawn
//!    and reports it — and the same id then draws rule 2's answer.
//!
//! Each row is a distinct input along what rule 1 generalises over — the way the value departs
//! from the pattern: no separators at all, the wrong number of groups, each group one
//! character short and one character long, a character outside the word class in each
//! position, and the empty value — so no row is the specification. The pattern, the codes and
//! the sentences are written out here, independently of the service.

use std::time::SystemTime;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, RESERVED_INSTANCES_LISTING_STATUS_CANCELLED,
    ReservedInstancesSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// EC2's code for a Marketplace value the service cannot act on, which is also the code the
/// parameter layer's pattern refusal carries for this member.
const INVALID_INPUT: &str = "InvalidInput";

/// The member that validator paths a listing id by, as the refusal it answers names it —
/// NOT the wire name the request sends the member under (`reservedInstancesListingId`).
const VALIDATED_MEMBER: &str = "reservedInstanceListingId";

/// The regular expression the refusal publishes for a listing id: five groups of word
/// characters of lengths 8, 4, 4, 4 and 12, hyphen-separated.
const LISTING_ID_PATTERN: &str = r"\w{8}(-\w{4}){3}-\w{12}";

/// The sentence the Marketplace answers for a value inside the pattern that this operation
/// cannot act on — the service sends it with the space where a reason would go, and a message
/// a client compares is the whole message.
const UNUSABLE_LISTING_ID_MESSAGE: &str = " Invalid value for 'listingId'";

/// A value in the listing-id pattern that names no listing of this account: the store mints
/// its ids from its own counter, so nothing it holds can read like this.
const UNKNOWN_LISTING_ID: &str = "11111111-2222-4333-8444-555555555555";

/// A one-year Reserved Instance term, in seconds — the units `ReservedInstances$Duration` is
/// reported in.
const ONE_YEAR_SECONDS: i64 = 365 * 24 * 60 * 60;

/// The months a price schedule has to cover for a purchase listed the moment its term started.
const ONE_YEAR_MONTHS: i64 = 12;

/// The expected clause for a value outside the pattern, written out here rather than built by
/// the service.
fn pattern_violation_message(value: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{VALIDATED_MEMBER}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {LISTING_ID_PATTERN}"
    )
}

/// One `CancelReservedInstancesListing` call naming `listing_id`.
async fn cancel(router: &MultiServiceRouter, listing_id: &str) -> (u16, String) {
    call(
        router,
        REGION,
        "CancelReservedInstancesListing",
        &[param("ReservedInstancesListingId", listing_id)],
    )
    .await
}

/// Seed one `active` Standard Reserved Instance holding `instance_count` reservations and
/// answer the id the store minted for it.
///
/// SEEDED rather than purchased over the wire: a purchase resolves an offering this emulator
/// publishes, and what this suite is about is the listing id, so the purchase it lists is
/// stored through the state writer that derives every member the service owns.
fn seed_reserved_instances(engine: &engine::InMemoryEngine, instance_count: i32) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_reserved_instances(ReservedInstancesSpec {
            instance_type: "t3.micro".to_string(),
            availability_zone: Some("us-east-1a".to_string()),
            region: REGION.to_string(),
            instance_count,
            duration_seconds: ONE_YEAR_SECONDS,
            offering_class: "standard".to_string(),
            offering_type: "All Upfront".to_string(),
            product_description: "Linux/UNIX".to_string(),
            state: "active".to_string(),
            start: SystemTime::now(),
            fixed_price: 500.0,
            usage_price: 0.0,
        })
        .expect("the store has room for the one purchase this suite seeds")
}

/// List one of the seeded reservations for sale and answer the listing id the store minted.
async fn list_for_sale(
    case: &str,
    router: &MultiServiceRouter,
    reserved_instances_id: &str,
) -> String {
    let params: Vec<Param> = vec![
        param("ReservedInstancesId", reserved_instances_id),
        param("InstanceCount", "1"),
        param("ClientToken", "listing-id-form"),
        param("PriceSchedules.1.Term", &ONE_YEAR_MONTHS.to_string()),
        param("PriceSchedules.1.Price", "250.0"),
    ];
    let (status, body) = call(
        router,
        REGION,
        "CreateReservedInstancesListing",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 listing the reservation, got {status} with body {body}"
    );
    element(&body, "reservedInstancesListingId")
        .unwrap_or_else(|| panic!("case {case:?}: no reservedInstancesListingId in {body}"))
}

/// RULE 1: every value outside the pattern is refused with the clause that names the pattern.
#[tokio::test]
async fn a_listing_id_outside_the_published_pattern_is_refused_for_the_constraint() {
    let (router, _engine) = super::ec2_query::emulator();

    // `(case, the value)`: each row departs from the pattern in a different way.
    let rows: Vec<(&str, String)> = vec![
        ("hyphen-separated words of the wrong lengths", "totally-not-a-listing-id".to_string()),
        ("no separators at all", "11111111222243338444555555555555".to_string()),
        ("one group too few", "11111111-2222-4333-555555555555".to_string()),
        (
            "one group too many",
            "11111111-2222-4333-8444-5555-555555555555".to_string(),
        ),
        ("the first group one character short", "1111111-2222-4333-8444-555555555555".to_string()),
        ("the first group one character long", "111111111-2222-4333-8444-555555555555".to_string()),
        ("a middle group one character short", "11111111-222-4333-8444-555555555555".to_string()),
        ("a middle group one character long", "11111111-22222-4333-8444-555555555555".to_string()),
        ("the last group one character short", "11111111-2222-4333-8444-55555555555".to_string()),
        ("the last group one character long", "11111111-2222-4333-8444-5555555555555".to_string()),
        (
            "a character outside the word class",
            "1111111!-2222-4333-8444-555555555555".to_string(),
        ),
        (
            "a character outside the word class in the last group",
            "11111111-2222-4333-8444-55555555555.".to_string(),
        ),
        ("an EC2 resource id of another kind", "rli-0123456789abcdef0".to_string()),
        ("the empty value", String::new()),
    ];

    for (case, value) in rows {
        let (status, body) = cancel(&router, &value).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS} for {value:?}, got {status} with body \
             {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_INPUT,
            "case {case:?}: expected the code {INVALID_INPUT:?} for {value:?}, got {code:?} in \
             {body}"
        );
        let expected_message = pattern_violation_message(&value);
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }
}

/// RULES 2 and 3: a value INSIDE the pattern draws the Marketplace's own unusable-value
/// sentence — for an id of no listing, and for the id of a listing that is no longer on sale —
/// and the listing that IS on sale is withdrawn by the very call those rows are refused by.
#[tokio::test]
async fn a_listing_id_inside_the_pattern_is_answered_for_the_listing_it_names() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "a listing id inside the pattern";

    // An id in the pattern that the account holds no listing under: the value is readable, so
    // the answer is the one about the listing rather than about the form.
    let (status, body) = cancel(&router, UNKNOWN_LISTING_ID).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} for an id of no listing, got {status} \
         with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, INVALID_INPUT,
        "case {case:?}: expected the code {INVALID_INPUT:?}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, UNUSABLE_LISTING_ID_MESSAGE,
        "case {case:?}: expected the message {UNUSABLE_LISTING_ID_MESSAGE:?}, got {message:?}"
    );

    // RULE 3: the id of a listing that is on sale is acted on, so the two refusals above are
    // judgements about the value and the state rather than a call that refuses everything.
    let reserved_instances_id = seed_reserved_instances(&engine, 2);
    let listing_id = list_for_sale(case, &router, &reserved_instances_id).await;
    let (status, body) = cancel(&router, &listing_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 withdrawing {listing_id}, got {status} with body {body}"
    );
    let status_reported = element(&body, "status").unwrap_or_default();
    assert_eq!(
        status_reported, RESERVED_INSTANCES_LISTING_STATUS_CANCELLED,
        "case {case:?}: expected the withdrawn listing to report \
         {RESERVED_INSTANCES_LISTING_STATUS_CANCELLED:?}, got {status_reported:?} in {body}"
    );

    // And the same id, still inside the pattern, now names a listing this operation cannot
    // act on: the answer is rule 2's, not rule 1's.
    let (status, body) = cancel(&router, &listing_id).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} withdrawing {listing_id} twice, got \
         {status} with body {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, UNUSABLE_LISTING_ID_MESSAGE,
        "case {case:?}: expected the message {UNUSABLE_LISTING_ID_MESSAGE:?}, got {message:?}"
    );
}
