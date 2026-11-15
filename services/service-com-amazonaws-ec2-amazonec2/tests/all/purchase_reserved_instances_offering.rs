// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `PurchaseReservedInstancesOffering`, with the
//! `DescribeReservedInstancesOfferings` that publishes the offering it buys and the
//! `DescribeReservedInstances` that reads the purchase back — driven over the wire the
//! service actually speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=PurchaseReservedInstancesOffering`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2` signing name in the
//! request's SigV4 credential scope, so every call here carries one.
//!
//! The rules under test:
//!
//! 1. A purchase BUYS THE OFFERING ITS ID NAMES: every member of the Reserved Instance it
//!    stores comes from that offering's terms — the instance type, the Availability Zone and
//!    so the scope, the product description, the offering class and type, the term and both
//!    prices — with only the number of reservations coming from the request, and the whole of
//!    it comes back out of `DescribeReservedInstances` under the id the purchase answered.
//! 2. A PURCHASE IS NOT IN EFFECT AT ONCE: it is `payment-pending` until its upfront payment
//!    settles, and one the request scheduled for a future `PurchaseTime` is `queued` until
//!    that date, which is also when its term starts.
//! 3. A VALUE OUTSIDE THE OFFERING-ID FORM is refused `InvalidParameterValue` before anything
//!    is resolved, a well-formed id this service publishes no offering under is refused
//!    `InvalidReservedInstancesOfferingId`, and `DryRun` is answered between them: after the
//!    request's own shape has been read and before the offering is resolved. Nothing is bought
//!    on any of those paths.
//! 4. WHAT THE REQUEST ASKS FOR IS READ: a count below one reservation is refused, and so is
//!    an order whose total (`instanceCount * price`) is above the `LimitPrice` the caller set.
//!
//! Every table row is a distinct input along what the rules generalise over — the scope, the
//! instance type, the product, the offering class and payment option, the term, the count, the
//! purchase time, the limit price, and the several ways a value can fail to be an offering id
//! — so no one recorded purchase is the specification.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE,
    DRY_RUN_OPERATION_STATUS, INVALID_PARAMETER_VALUE, INVALID_RESERVED_INSTANCES_OFFERING_ID,
    RESERVED_INSTANCE_SCOPE_REGION, RESERVED_INSTANCE_SCOPE_ZONE, RESERVED_INSTANCE_STATE_ACTIVE,
    RESERVED_INSTANCE_STATE_PAYMENT_PENDING, RESERVED_INSTANCE_STATE_QUEUED,
    ReservedInstancesOfferingTerms, echoed_resource_id, encode_reserved_instances_offering_id,
    iso8601_utc, reserved_instances_id_is_well_formed,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{as_timestamp, call, error_code, error_message, filters, indexed};
use super::wire::{Param, document, param, set_items};

/// The Region every call here is signed for: an offering is published for the Region the
/// request reached, and the zone a purchase of it names is that Region's own.
const REGION: &str = "us-east-1";

/// The HTTP status every refusal here carries.
const REFUSAL_STATUS: u16 = 400;

/// A one-year term in seconds, and below it a three-year one: the two values the `duration`
/// filter of `DescribeReservedInstancesOfferings` documents (`31536000` | `94608000`).
const ONE_YEAR_SECONDS: i64 = 31_536_000;

/// The other of those two terms.
const THREE_YEARS_SECONDS: i64 = 94_608_000;

/// How long a poll below may run before it reports that a purchase never left its transient
/// state. Generous on purpose: all it bounds is a hang, so a loaded machine polls more often
/// instead of failing.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between two reads, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How far two amounts this suite compares may differ: a price reaches the wire as a `Float`
/// member, so a value built from one carries that rounding.
const AMOUNT_TOLERANCE: f64 = 0.01;

/// One `PurchaseReservedInstancesOffering` call, as its HTTP status and XML body.
async fn purchase(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    call(router, REGION, "PurchaseReservedInstancesOffering", params).await
}

/// The `reservedInstancesId` a served purchase answered, having first pinned the status.
fn purchased_id(case: &str, status: u16, body: &str) -> String {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    super::ec2_query::member(body, "reservedInstancesId")
        .unwrap_or_else(|| panic!("case {case:?}: no <reservedInstancesId> in {body}"))
}

/// The members `DescribeReservedInstances` reports for `reserved_instances_id`, by element
/// name — read off the parsed document, so a member of a nested set is never taken for one of
/// the purchase's own.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    reserved_instances_id: &str,
) -> BTreeMap<String, String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeReservedInstances",
        &indexed("ReservedInstancesId", &[reserved_instances_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the report of {reserved_instances_id} must succeed, got {status} with \
         body {body}"
    );
    let parsed = document(case, &body);
    let item = set_items(parsed.root_element(), "reservedInstancesSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the report must carry {reserved_instances_id}: {body}")
        });
    item.children()
        .filter(|node| node.is_element())
        .map(|node| {
            (
                node.tag_name().name().to_string(),
                node.text().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The `state` the report carries for `reserved_instances_id`.
async fn reported_state(router: &MultiServiceRouter, case: &str, reserved_instances_id: &str) -> String {
    described(router, case, reserved_instances_id)
        .await
        .get("state")
        .cloned()
        .unwrap_or_default()
}

/// Look again at `reserved_instances_id` until it reports `wanted`, exactly as a client's own
/// waiter does, and answer the state it then reports.
///
/// A wait on the CONDITION rather than on a span: the move is the clock's, so this keeps
/// looking until the report shows it or [`POLL_DEADLINE`] passes.
async fn polled_state(
    router: &MultiServiceRouter,
    case: &str,
    reserved_instances_id: &str,
    wanted: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    let mut state = reported_state(router, case, reserved_instances_id).await;
    while state != wanted && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        state = reported_state(router, case, reserved_instances_id).await;
    }
    state
}

/// How many Reserved Instances the store holds — what a "nothing was bought" assertion reads.
fn stored_count(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().reserved_instances.len()
}

/// The instant the store owes `reserved_instances_id` its `active` state, as seconds since the
/// Unix epoch; `None` when the purchase is already in effect.
fn activation_of(engine: &engine::InMemoryEngine, reserved_instances_id: &str) -> Option<f64> {
    engine
        .state::<Ec2State>()
        .lock()
        .reserved_instances_activations
        .get(reserved_instances_id)
        .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|since| since.as_secs_f64())
}

/// One row of the purchase table: the case, the offering to buy and how many reservations of
/// it to ask for.
struct PurchaseCase {
    case: &'static str,
    terms: ReservedInstancesOfferingTerms,
    instance_count: i32,
}

/// One offering a row buys, as the fields an offering is defined by — the row form of
/// [`ReservedInstancesOfferingTerms`], so a row can write only what it varies
/// (`OfferingRow { .. ZONAL_STANDARD }`).
#[derive(Debug, Clone, Copy)]
struct OfferingRow {
    instance_type: &'static str,
    availability_zone: Option<&'static str>,
    product_description: &'static str,
    offering_class: &'static str,
    offering_type: &'static str,
    duration_seconds: i64,
    fixed_price: f32,
    usage_price: f32,
}

/// The offering every row that varies nothing buys: one reservation of it costs its purchase
/// price, in a zone this Region has. The enum members are the modelled catalogs' own —
/// `standard` of `OFFERING_CLASS_TYPE_VALUES`, `All Upfront` of `OFFERING_TYPE_VALUES_VALUES`,
/// `Linux/UNIX` of `R_I_PRODUCT_DESCRIPTION_VALUES` and an `InstanceType` of
/// `INSTANCE_TYPE_VALUES`.
const ZONAL_STANDARD: OfferingRow = OfferingRow {
    instance_type: "m5.large",
    availability_zone: Some("us-east-1a"),
    product_description: "Linux/UNIX",
    offering_class: "standard",
    offering_type: "All Upfront",
    duration_seconds: ONE_YEAR_SECONDS,
    fixed_price: 500.0,
    usage_price: 0.0,
};

/// The terms of one offering to buy, as a row writes them.
fn terms(row: OfferingRow) -> ReservedInstancesOfferingTerms {
    ReservedInstancesOfferingTerms {
        instance_type: row.instance_type.to_string(),
        availability_zone: row.availability_zone.map(str::to_string),
        product_description: row.product_description.to_string(),
        offering_class: row.offering_class.to_string(),
        offering_type: row.offering_type.to_string(),
        duration_seconds: row.duration_seconds,
        fixed_price: row.fixed_price,
        usage_price: row.usage_price,
    }
}

/// A member of the report as a number, or a failure naming the case.
fn number(case: &str, reported: &BTreeMap<String, String>, name: &str) -> f64 {
    let value = reported
        .get(name)
        .unwrap_or_else(|| panic!("case {case:?}: the purchase must report {name}: {reported:?}"));
    value
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("case {case:?}: {name} must be a number, got {value:?}"))
}

/// A member of the report as text, or the empty string when it carries none.
fn text(reported: &BTreeMap<String, String>, name: &str) -> String {
    reported.get(name).cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule 1: a purchase buys the offering its id names
// ---------------------------------------------------------------------------

/// The rule: the Reserved Instance a purchase stores carries the offering's own terms and the
/// count the request asked for, and a later `DescribeReservedInstances` reports exactly that.
///
/// The rows differ along what the rule generalises over: the scope (a zone of the Region, and
/// the Region itself), the instance type, the product description, the offering class, the
/// payment option, the term, both prices and how many reservations are bought.
#[tokio::test]
async fn a_purchase_stores_the_offering_it_names_and_reports_it_back() {
    let cases = [
        PurchaseCase {
            case: "one reservation of a zonal All Upfront offering",
            terms: terms(ZONAL_STANDARD),
            instance_count: 1,
        },
        PurchaseCase {
            case: "several reservations of a Regional Convertible offering",
            terms: terms(OfferingRow {
                instance_type: "m5.xlarge",
                availability_zone: None,
                offering_class: "convertible",
                offering_type: "Partial Upfront",
                duration_seconds: THREE_YEARS_SECONDS,
                fixed_price: 1_234.5,
                usage_price: 0.04,
                ..ZONAL_STANDARD
            }),
            instance_count: 4,
        },
        PurchaseCase {
            case: "a Windows offering in another zone of the Region",
            terms: terms(OfferingRow {
                instance_type: "t3.micro",
                availability_zone: Some("us-east-1b"),
                product_description: "Windows",
                offering_type: "No Upfront",
                fixed_price: 0.0,
                usage_price: 0.026,
                ..ZONAL_STANDARD
            }),
            instance_count: 2,
        },
        PurchaseCase {
            case: "a previous-generation type bought for a whole Region",
            terms: terms(OfferingRow {
                instance_type: "m1.small",
                availability_zone: None,
                product_description: "SUSE Linux",
                offering_class: "convertible",
                duration_seconds: THREE_YEARS_SECONDS,
                fixed_price: 987.65,
                ..ZONAL_STANDARD
            }),
            instance_count: 10,
        },
    ];

    for PurchaseCase {
        case,
        terms,
        instance_count,
    } in cases
    {
        let (router, engine) = super::ec2_query::emulator();
        let offering_id = encode_reserved_instances_offering_id(&terms);
        let bought_at = SystemTime::now();

        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", &offering_id),
                param("InstanceCount", &instance_count.to_string()),
            ],
        )
        .await;
        let reserved_instances_id = purchased_id(case, status, &body);
        assert!(
            reserved_instances_id_is_well_formed(&reserved_instances_id),
            "case {case:?}: a Reserved Instances id is a UUID, got {reserved_instances_id:?}"
        );
        assert_eq!(
            stored_count(&engine),
            1,
            "case {case:?}: exactly the purchase just made must be held, got {} \
             Reserved Instances",
            stored_count(&engine)
        );

        let reported = described(&router, case, &reserved_instances_id).await;
        assert_eq!(
            text(&reported, "reservedInstancesId"),
            reserved_instances_id,
            "case {case:?}: the report must be of the purchase the call answered, got \
             {reported:?}"
        );
        assert_eq!(
            text(&reported, "instanceType"),
            terms.instance_type,
            "case {case:?}: the instance type is the offering's, got {reported:?}"
        );
        assert_eq!(
            reported.get("availabilityZone").cloned(),
            terms.availability_zone.clone(),
            "case {case:?}: the zone is the offering's, got {reported:?}"
        );
        let expected_scope = if terms.availability_zone.is_some() {
            RESERVED_INSTANCE_SCOPE_ZONE
        } else {
            RESERVED_INSTANCE_SCOPE_REGION
        };
        assert_eq!(
            text(&reported, "scope"),
            expected_scope,
            "case {case:?}: a purchase that names a zone is zonal and one that names none is \
             Regional, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "productDescription"),
            terms.product_description,
            "case {case:?}: the product description is the offering's, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "offeringClass"),
            terms.offering_class,
            "case {case:?}: the offering class is the offering's, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "offeringType"),
            terms.offering_type,
            "case {case:?}: the payment option is the offering's, got {reported:?}"
        );
        assert_eq!(
            number(case, &reported, "duration") as i64,
            terms.duration_seconds,
            "case {case:?}: the term is the offering's, got {reported:?}"
        );
        assert!(
            (number(case, &reported, "fixedPrice") - f64::from(terms.fixed_price)).abs()
                <= AMOUNT_TOLERANCE,
            "case {case:?}: expected the offering's purchase price {}, got {reported:?}",
            terms.fixed_price
        );
        assert!(
            (number(case, &reported, "usagePrice") - f64::from(terms.usage_price)).abs()
                <= AMOUNT_TOLERANCE,
            "case {case:?}: expected the offering's usage price {}, got {reported:?}",
            terms.usage_price
        );
        assert_eq!(
            number(case, &reported, "instanceCount") as i32,
            instance_count,
            "case {case:?}: the count is the request's, got {reported:?}"
        );

        // The term runs from the purchase for the offering's duration, so the two instants the
        // purchase reports are the offering's term around the moment it was bought.
        let start = as_timestamp(&text(&reported, "start")).unwrap_or_else(|| {
            panic!("case {case:?}: the purchase must report a start instant: {reported:?}")
        });
        let end = as_timestamp(&text(&reported, "end")).unwrap_or_else(|| {
            panic!("case {case:?}: the purchase must report an end instant: {reported:?}")
        });
        let bought_at_secs = bought_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("the test clock is past the Unix epoch")
            .as_secs_f64();
        assert!(
            start >= bought_at_secs - 1.0,
            "case {case:?}: the term starts no earlier than the purchase ({bought_at_secs}), \
             got {start}"
        );
        assert!(
            (end - start - terms.duration_seconds as f64).abs() <= 1.0,
            "case {case:?}: the term must last the offering's {} seconds, got {} ",
            terms.duration_seconds,
            end - start
        );

        // RULE 2, on the purchase this row just made: the capacity is paid for before it is in
        // effect, and the move to `active` is the clock's — read until it holds rather than
        // waited out.
        let settled = polled_state(
            &router,
            case,
            &reserved_instances_id,
            RESERVED_INSTANCE_STATE_ACTIVE,
        )
        .await;
        assert_eq!(
            settled, RESERVED_INSTANCE_STATE_ACTIVE,
            "case {case:?}: the clock must put the purchase into effect, it reports {settled:?}"
        );
        assert_eq!(
            activation_of(&engine, &reserved_instances_id),
            None,
            "case {case:?}: a purchase in effect owes the clock nothing further"
        );
    }
}

/// The rule: a purchase of an offering the catalog PUBLISHES is served, and what it stores is
/// the offering `DescribeReservedInstancesOfferings` reported under that id.
///
/// The round trip the rule above asserts over derived ids, made over an id this service really
/// published: the search narrows the catalog to one offering, the purchase buys that id, and
/// the report of the purchase carries the offering's own terms.
#[tokio::test]
async fn an_offering_the_catalog_publishes_can_be_bought_by_the_id_it_was_reported_under() {
    let cases: &[(&str, Vec<Param>)] = &[
        (
            "a zonal offering of the published catalog",
            vec![
                param("InstanceType", "m5.large"),
                param("ProductDescription", "Linux/UNIX"),
                param("OfferingClass", "standard"),
                param("OfferingType", "All Upfront"),
                param("AvailabilityZone", "us-east-1a"),
            ],
        ),
        (
            "a Regional offering of the published catalog",
            {
                let mut params = vec![
                    param("InstanceType", "t3.micro"),
                    param("ProductDescription", "Windows"),
                    param("OfferingClass", "convertible"),
                    param("OfferingType", "No Upfront"),
                ];
                params.extend(filters(&[("scope", &[RESERVED_INSTANCE_SCOPE_REGION])]));
                params
            },
        ),
    ];

    for (case, search) in cases {
        let (router, engine) = super::ec2_query::emulator();
        let mut params = search.clone();
        params.push(param("MaxResults", "1"));
        let (status, body) = call(
            &router,
            REGION,
            "DescribeReservedInstancesOfferings",
            &params,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the search for an offering must succeed, got {status} with body \
             {body}"
        );
        let parsed = document(case, &body);
        let offering = set_items(parsed.root_element(), "reservedInstancesOfferingsSet")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: the search must report an offering: {body}"));
        let offered: BTreeMap<String, String> = offering
            .children()
            .filter(|node| node.is_element())
            .map(|node| {
                (
                    node.tag_name().name().to_string(),
                    node.text().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let offering_id = text(&offered, "reservedInstancesOfferingId");

        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", &offering_id),
                param("InstanceCount", "3"),
            ],
        )
        .await;
        let reserved_instances_id = purchased_id(case, status, &body);
        let reported = described(&router, case, &reserved_instances_id).await;

        for member in [
            "instanceType",
            "productDescription",
            "offeringClass",
            "offeringType",
            "scope",
            "availabilityZone",
            "duration",
        ] {
            assert_eq!(
                reported.get(member).cloned(),
                offered.get(member).cloned(),
                "case {case:?}: the purchase's {member} is the offering's, got {reported:?} for \
                 the offering {offered:?}"
            );
        }
        assert_eq!(
            number(case, &reported, "instanceCount") as i32,
            3,
            "case {case:?}: the count is the request's, got {reported:?}"
        );
        assert_eq!(
            stored_count(&engine),
            1,
            "case {case:?}: one purchase must be held, got {}",
            stored_count(&engine)
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a purchase is not in effect at once
// ---------------------------------------------------------------------------

/// The rule: a purchase waits — on its upfront payment when it is made now, and on the
/// `PurchaseTime` it was scheduled for when that is still ahead, which is also when its term
/// starts.
///
/// The rows differ along the purchase time the request names: none at all, one that has
/// passed ("if you do not specify a purchase time, the default is the current time"), and two
/// in the future, which is what queues a purchase.
#[tokio::test]
async fn a_purchase_waits_on_its_payment_or_on_the_purchase_time_it_was_queued_for() {
    // (case, how far from now the request's PurchaseTime is, the state it must be held in)
    let cases: &[(&str, Option<i64>, &str)] = &[
        (
            "no purchase time at all",
            None,
            RESERVED_INSTANCE_STATE_PAYMENT_PENDING,
        ),
        (
            "a purchase time that has passed",
            Some(-3_600),
            RESERVED_INSTANCE_STATE_PAYMENT_PENDING,
        ),
        (
            "a purchase time an hour out",
            Some(3_600),
            RESERVED_INSTANCE_STATE_QUEUED,
        ),
        (
            "a purchase time a week out",
            Some(7 * 24 * 3_600),
            RESERVED_INSTANCE_STATE_QUEUED,
        ),
    ];

    for (case, purchase_time, expected_state) in cases {
        let (router, engine) = super::ec2_query::emulator();
        let offering = terms(ZONAL_STANDARD);
        let mut params = vec![
            param(
                "ReservedInstancesOfferingId",
                &encode_reserved_instances_offering_id(&offering),
            ),
            param("InstanceCount", "1"),
        ];
        let scheduled_for = purchase_time.map(|seconds| {
            let shift = Duration::from_secs(seconds.unsigned_abs());
            let now = SystemTime::now();
            if seconds < 0 { now - shift } else { now + shift }
        });
        if let Some(at) = scheduled_for {
            params.push(param("PurchaseTime", &iso8601_utc(at)));
        }

        let (status, body) = purchase(&router, &params).await;
        let reserved_instances_id = purchased_id(case, status, &body);
        let reported = described(&router, case, &reserved_instances_id).await;
        assert_eq!(
            text(&reported, "state"),
            *expected_state,
            "case {case:?}: expected the purchase to be held {expected_state:?}, got {reported:?}"
        );

        // The instant the clock owes it `active`: the purchase time of a queued purchase, which
        // is the date the REQUEST named, and a settlement instant still ahead for one made now.
        let owed = activation_of(&engine, &reserved_instances_id).unwrap_or_else(|| {
            panic!("case {case:?}: a purchase not yet in effect must owe the clock its state")
        });
        if *expected_state == RESERVED_INSTANCE_STATE_QUEUED {
            let queued_for = scheduled_for
                .expect("a queued purchase is one the request named a future time for")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("the test clock is past the Unix epoch")
                .as_secs_f64();
            assert!(
                (owed - queued_for).abs() <= 1.0,
                "case {case:?}: a queued purchase becomes active at the purchase time \
                 {queued_for}, got {owed}"
            );
            // And its term starts then rather than at the call.
            let start = as_timestamp(&text(&reported, "start")).unwrap_or_else(|| {
                panic!("case {case:?}: the purchase must report a start instant: {reported:?}")
            });
            assert!(
                (start - queued_for).abs() <= 1.0,
                "case {case:?}: a queued purchase's term starts at the purchase time \
                 {queued_for}, got {start}"
            );
            // Nothing a reader does brings that date forward: the purchase is still queued
            // after the report above read it.
            assert_eq!(
                reported_state(&router, case, &reserved_instances_id).await,
                RESERVED_INSTANCE_STATE_QUEUED,
                "case {case:?}: reading a queued purchase must not put it into effect"
            );
            continue;
        }
        let settled = polled_state(
            &router,
            case,
            &reserved_instances_id,
            RESERVED_INSTANCE_STATE_ACTIVE,
        )
        .await;
        assert_eq!(
            settled, RESERVED_INSTANCE_STATE_ACTIVE,
            "case {case:?}: the clock must settle the payment, the purchase reports {settled:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the refusals, and the order they are answered in
// ---------------------------------------------------------------------------

/// The rule: a value outside the `ReservedInstancesOfferingId` form names no offering in any
/// account, so it is refused `InvalidParameterValue` — naming the member and echoing the value
/// — before anything is resolved, and nothing is bought.
///
/// The rows are the ways a value can fail to be an offering id: a plain word, a value of
/// another id family, a truncated UUID, one with a non-hexadecimal digit, this service's
/// offering prefix with nothing or with non-hexadecimal text after it, and the prefix without
/// its separator.
#[tokio::test]
async fn a_value_outside_the_offering_id_form_is_refused_before_anything_is_bought() {
    let offered = encode_reserved_instances_offering_id(&terms(ZONAL_STANDARD));

    for named in [
        "not-a-real-offering-id",
        "rio",
        "rio-",
        "rio-not-hex",
        "ri-0123456789abcdef0",
        "1a2b3c4d-1234-5678-9abc",
        "1a2b3c4d-1234-5678-9abc-1234567890zz",
        "RIO-0123456789ABCDEF",
    ] {
        let case = format!("an offering id of {named:?}");
        let (router, engine) = super::ec2_query::emulator();
        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", named),
                param("InstanceCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the code {INVALID_PARAMETER_VALUE}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Invalid value 'ReservedInstancesOfferingId' for {named}."),
            "case {case:?}: the refusal must name the member and the value it could not read, \
             got {body}"
        );
        assert_eq!(
            stored_count(&engine),
            0,
            "case {case:?}: a refused purchase buys nothing, the store holds {}",
            stored_count(&engine)
        );

        // The same call with an id this service DOES publish an offering under is served, so
        // the refusals above are about the id's form and nothing else.
        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", &offered),
                param("InstanceCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an offering this service publishes must be bought, got {status} \
             with body {body}"
        );
    }
}

/// The rule: a WELL-FORMED offering id this service publishes no offering under is refused
/// `InvalidReservedInstancesOfferingId` — the other half of the pair — and buys nothing.
///
/// The rows are the ways a well-formed id can name nothing: the UUID form AWS issues its own
/// offering ids in, this service's derived form with its payload altered, and a derived id
/// whose offering lies in a zone of ANOTHER Region, which this Region publishes no offering
/// for.
#[tokio::test]
async fn a_well_formed_offering_id_that_names_no_offering_is_refused() {
    let published = encode_reserved_instances_offering_id(&terms(ZONAL_STANDARD));
    let another_region = encode_reserved_instances_offering_id(&terms(OfferingRow {
        availability_zone: Some("eu-west-1a"),
        ..ZONAL_STANDARD
    }));
    // The derived form with its payload altered: still `rio-` and still hexadecimal, so it is
    // well formed, but it carries no offering this service ever reported.
    let altered = format!("{}00", &published[..published.len() - 2]);

    let cases: &[(&str, &str)] = &[
        (
            "a UUID of the form AWS issues an offering id in",
            "1a2b3c4d-1234-5678-9abc-1234567890ab",
        ),
        ("a derived id whose payload was altered", &altered),
        (
            "an offering of a zone another Region has",
            &another_region,
        ),
    ];

    for (case, named) in cases {
        let (router, engine) = super::ec2_query::emulator();
        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", named),
                param("InstanceCount", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_RESERVED_INSTANCES_OFFERING_ID,
            "case {case:?}: expected the code {INVALID_RESERVED_INSTANCES_OFFERING_ID}, got \
             {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "The Reserved Instances offering ID '{id}' does not exist",
                id = echoed_resource_id(named)
            ),
            "case {case:?}: the refusal must name the id it could not resolve, got {body}"
        );
        assert_eq!(
            stored_count(&engine),
            0,
            "case {case:?}: a refused purchase buys nothing, the store holds {}",
            stored_count(&engine)
        );
    }
}

/// The rule: `DryRun` is answered after the request's own shape has been read and before the
/// offering is resolved — "checks whether you have the required permissions for the action,
/// without actually making the request … if you have the required permissions, the error
/// response is DryRunOperation" — and buys nothing.
///
/// The rows differ in what the probe would have gone on to do: buy a published offering, name a
/// well-formed id that resolves to nothing, or carry a value that is no offering id at all —
/// which is refused for its FORM even under the flag, since that check runs ahead of it.
#[tokio::test]
async fn a_dry_run_is_answered_before_the_offering_is_resolved_and_buys_nothing() {
    let published = encode_reserved_instances_offering_id(&terms(ZONAL_STANDARD));

    // (case, the offering id the probe names, the code and status it must be answered with)
    let cases: &[(&str, &str, &str, u16)] = &[
        (
            "a probe of an offering this service publishes",
            &published,
            DRY_RUN_OPERATION,
            DRY_RUN_OPERATION_STATUS,
        ),
        (
            "a probe naming a well-formed id that resolves to nothing",
            "1a2b3c4d-1234-5678-9abc-1234567890ab",
            DRY_RUN_OPERATION,
            DRY_RUN_OPERATION_STATUS,
        ),
        (
            "a probe carrying no offering id at all",
            "not-a-real-offering-id",
            INVALID_PARAMETER_VALUE,
            REFUSAL_STATUS,
        ),
    ];

    for (case, named, code, expected_status) in cases {
        let (router, engine) = super::ec2_query::emulator();
        let (status, body) = purchase(
            &router,
            &[
                param("ReservedInstancesOfferingId", named),
                param("InstanceCount", "1"),
                param("DryRun", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            *code,
            "case {case:?}: expected the code {code}, got {body}"
        );
        if *code == DRY_RUN_OPERATION {
            assert_eq!(
                error_message(&body),
                DRY_RUN_OPERATION_MESSAGE,
                "case {case:?}: the verdict carries EC2's own sentence, got {body}"
            );
        }
        assert_eq!(
            stored_count(&engine),
            0,
            "case {case:?}: a probe buys nothing, the store holds {}",
            stored_count(&engine)
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: what the request asks for
// ---------------------------------------------------------------------------

/// The rule: a count below one reservation is not a purchase, and an order whose total
/// (`instanceCount * price`) is above the `LimitPrice` the request set is the order the caller
/// asked not to be made — both refused `InvalidParameterValue`, buying nothing.
///
/// The rows differ along the two members: a count of zero and a negative one, a limit above
/// the total, one exactly at it, one below it and a negative one.
#[tokio::test]
async fn a_count_or_a_limit_price_the_order_cannot_meet_is_refused() {
    // The offering every row buys: one reservation costs its purchase price.
    let offering = terms(ZONAL_STANDARD);
    let offering_id = encode_reserved_instances_offering_id(&offering);

    // (case, the count to ask for, the limit price to set, whether the purchase is served)
    let cases: &[(&str, i32, Option<f64>, bool)] = &[
        ("one reservation and no limit price", 1, None, true),
        ("a limit price above the total order", 2, Some(1_500.0), true),
        ("a limit price exactly at the total order", 2, Some(1_000.0), true),
        ("a limit price below the total order", 2, Some(999.0), false),
        ("a limit price below the price of one", 1, Some(0.5), false),
        ("a negative limit price", 1, Some(-1.0), false),
        ("a count of zero", 0, None, false),
        ("a negative count", -3, None, false),
    ];

    for (case, instance_count, limit_price, served) in cases {
        let (router, engine) = super::ec2_query::emulator();
        let mut params = vec![
            param("ReservedInstancesOfferingId", &offering_id),
            param("InstanceCount", &instance_count.to_string()),
        ];
        if let Some(amount) = limit_price {
            params.push(param("LimitPrice.Amount", &amount.to_string()));
            params.push(param("LimitPrice.CurrencyCode", "USD"));
        }

        let (status, body) = purchase(&router, &params).await;
        if *served {
            let reserved_instances_id = purchased_id(case, status, &body);
            assert_eq!(
                number(
                    case,
                    &described(&router, case, &reserved_instances_id).await,
                    "instanceCount"
                ) as i32,
                *instance_count,
                "case {case:?}: an order within its limit must buy what it asked for"
            );
            continue;
        }
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the code {INVALID_PARAMETER_VALUE}, got {body}"
        );
        assert_eq!(
            stored_count(&engine),
            0,
            "case {case:?}: a refused purchase buys nothing, the store holds {}",
            stored_count(&engine)
        );
    }
}
