// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetOutpostBillingInformation`, driven over the wire through
//! the same front door as [`crate::site_and_outpost_lifecycle`] (whose harness this
//! suite reuses) and priced by the same rule as [`crate::renewal_pricing`] (whose
//! catalog and price helpers it reuses, so the one subscription-pricing rule is stated
//! once for every suite that checks a figure).
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. The report derives ONE subscription per order placed on the Outpost and one per
//!    renewal contract bought for it, in the order those contracts run: the
//!    Outpost's first order is the `ORIGINAL` subscription, every later order is a
//!    `CAPACITY_INCREASE` one, and a renewal is a `RENEWAL`. Each carries the order it
//!    is the billing record of, and an id of its own that no other subscription of the
//!    store shares.
//! 2. A subscription on hardware is billed from the instant that hardware is
//!    INSTALLED, for the months the term it was bought under runs: an order whose
//!    hardware has arrived is `ACTIVE` from its own `OrderFulfilledDate` to that date
//!    plus the term, one still on its way is `PENDING` from a begin date it has not
//!    reached, one whose term has run out is `INACTIVE`, and a cancelled order —
//!    which delivers nothing — is `CANCELLED` with no term at all.
//! 3. Its price is the service's own subscription-pricing rule applied to the capacity
//!    THAT order's line items subscribe, under the terms the order was bought under:
//!    the whole-term cost split into the payment option's upfront share and equal
//!    monthly instalments for the rest, in the one currency the model declares. An
//!    order bought under no term is a subscription this service holds no term, no end
//!    and no figures for. A renewal's subscription is billed at what that contract was
//!    actually bought at, which is what `CreateRenewal` answered.
//! 4. A renewal extends the subscription already on the Outpost's hardware: its term
//!    begins where the term it renews ends, so it is `PENDING` until then.
//! 5. `ContractEndDate`, `PaymentTerm` and `PaymentOption` state the contract in force
//!    NOW — of the terms being billed at this instant, the one that runs longest — with
//!    the end date rendered as the calendar date ISO 8601 writes. An Outpost with no
//!    term in force states none of the three.
//! 6. The subscriptions are PAGED by the same rule as every other paged answer of this
//!    service: `MaxResults` cuts the page, the `NextToken` resumes after the last
//!    subscription it returned, and a token issued while paging another Outpost's
//!    report is refused (400 `ValidationException`) rather than honoured.
//! 7. Both forms the modelled `OutpostIdentifier` pattern accepts name the same
//!    report, the report is a READ (repeating it answers the same thing and changes
//!    nothing a later caller sees), an identifier that names no Outpost is 404
//!    `NotFoundException` with the identifier echo of the value AS SUPPLIED, and one
//!    that is no Outpost identifier at all is 400 `ValidationException`.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_outposts_outpostsolafservice::state::{
    timestamp, CURRENCY_USD, SUBSCRIPTION_ID_PREFIX, SUBSCRIPTION_PRICE_PER_VCPU_MONTH,
    SUBSCRIPTION_STATUS_ACTIVE, SUBSCRIPTION_STATUS_CANCELLED, SUBSCRIPTION_STATUS_INACTIVE,
    SUBSCRIPTION_STATUS_PENDING, SUBSCRIPTION_TYPE_CAPACITY_INCREASE, SUBSCRIPTION_TYPE_ORIGINAL,
    SUBSCRIPTION_TYPE_RENEWAL,
};

use crate::renewal_pricing::{assert_price, months, price, published_catalog, upfront_share};
use crate::site_and_outpost_lifecycle::{
    is_minted_id, query_value, refusal, segment, text, Outposts, Refusal, ABSENT_OUTPOST_ID,
    ABSENT_SITE_ID, OUTPOST_ARN_PREFIX,
};

/// How close two instants reported as epoch seconds have to be to be the same one:
/// the wire carries them as a JSON number, so a comparison of a computed instant
/// against a reported one is to the millisecond rather than to the nanosecond.
const INSTANT: f64 = 1e-3;

/// The route `GetOutpostBillingInformation` binds, for one identifier form.
fn route(identifier: &str) -> String {
    format!("/outpost/{}/billing-information", segment(identifier))
}

/// The same route with a query string, for the paged rows.
fn paged_route(identifier: &str, query: &str) -> String {
    format!("{}?{query}", route(identifier))
}

/// One order a case places on its Outpost, and what rule 1, 2 and 3 say the
/// subscription it creates reports.
#[derive(Debug, Clone)]
struct OrderRow {
    /// What the row is, named in every assertion it makes.
    case: &'static str,
    /// The `(CatalogItemId, Quantity)` line items the order carries.
    items: Vec<(String, i64)>,
    /// The `PaymentOption` the order is bought under.
    payment_option: &'static str,
    /// The `PaymentTerm` it covers, or `None` for an order that names none —
    /// `CreateOrder` models the member optional.
    payment_term: Option<&'static str>,
    /// Whether the hardware the order was placed for has arrived and been installed,
    /// which is when billing on it starts.
    delivered: bool,
    /// Whether the order was cancelled after being placed.
    cancelled: bool,
    /// The modelled `SubscriptionType` of the subscription it creates.
    expected_type: &'static str,
    /// The modelled `SubscriptionStatus` that subscription holds.
    expected_status: &'static str,
}

/// The capacity, in vCPUs, this row's line items subscribe, from the catalog's own
/// published figures.
fn row_capacity(row: &OrderRow, catalog: &[(String, i64)]) -> i64 {
    row.items
        .iter()
        .map(|(sku, quantity)| {
            let published = catalog
                .iter()
                .find(|(item, _)| item == sku)
                .map_or(0, |(_, capacity)| *capacity);
            published * quantity
        })
        .sum()
}

/// Place one row's order on `outpost_id` over the wire, deliver or cancel it as the
/// row describes, and answer the `OrderId` it was placed under.
async fn place(outposts: &Outposts, outpost_id: &str, row: &OrderRow) -> String {
    let mut body = json!({
        "OutpostIdentifier": outpost_id,
        "PaymentOption": row.payment_option,
        "LineItems": Value::Array(
            row.items
                .iter()
                .map(|(sku, quantity)| json!({ "CatalogItemId": sku, "Quantity": quantity }))
                .collect(),
        ),
    });
    if let Some(term) = row.payment_term {
        body["PaymentTerm"] = json!(term);
    }
    let placed = outposts.ok("POST", "/orders", Some(&body)).await;
    let order_id = text(&placed["Order"], "OrderId");
    if row.cancelled {
        outposts
            .ok(
                "POST",
                &format!("/orders/{}/cancel", segment(&order_id)),
                Some(&json!({})),
            )
            .await;
    }
    if row.delivered {
        deliver(outposts, &order_id);
    }
    order_id
}

/// Bring every step the order `order_id` still owes the clock due NOW, through the
/// store seam the suites that age a stored resource use.
///
/// The hardware an order delivers is installed over spans far longer than a case may
/// wait for, so only the instants those steps fall due at are moved; the next call
/// materialises them, exactly as it would have had the clock reached them on its own.
fn deliver(outposts: &Outposts, order_id: &str) {
    let store = outposts.store();
    let mut store = store.lock();
    let record = store
        .orders
        .get_mut(order_id)
        .unwrap_or_else(|| panic!("the create persisted order {order_id}"));
    let (due, at) = (std::time::Instant::now(), SystemTime::now());
    for transition in record.pending.iter_mut() {
        transition.due = due;
        transition.at = at;
    }
}

/// Backdate the fulfilment of the order `order_id` by `years`, through the same store
/// seam.
///
/// A subscription whose term has RUN OUT is one no case could reach by waiting — the
/// shortest term this service bills is a year — so the instant its hardware was
/// installed at is moved instead, and the report is derived off it exactly as it is
/// off a fulfilment that happened a moment ago.
fn backdate_fulfilment(outposts: &Outposts, order_id: &str, years: u64) {
    let store = outposts.store();
    let mut store = store.lock();
    let record = store
        .orders
        .get_mut(order_id)
        .unwrap_or_else(|| panic!("the create persisted order {order_id}"));
    // 365 days to the year is close enough for a date five years out: what the row
    // asserts is that a term ending in the past is reported as one that has run out.
    let installed = SystemTime::now() - Duration::from_secs(years * 365 * 24 * 60 * 60);
    record.order.order_fulfilled_date = Some(timestamp(installed));
}

/// A timestamp member of a reported subscription, as the epoch seconds the wire
/// carries — `None` when the wire omits it.
fn instant(subscription: &Value, member: &str, case: &str) -> Option<f64> {
    subscription.get(member).map(|value| {
        value
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: {member} {value} is not an instant"))
    })
}

/// The same, required to be present.
fn required_instant(subscription: &Value, member: &str, case: &str) -> f64 {
    instant(subscription, member, case)
        .unwrap_or_else(|| panic!("case {case:?}: expected {member}, got {subscription}"))
}

/// The epoch-seconds instant `at`, as the calendar sees it.
fn at_instant(at: f64) -> ::chrono::DateTime<::chrono::Utc> {
    let seconds = at.trunc();
    ::chrono::DateTime::from_timestamp(
        seconds as i64,
        ((at - seconds) * 1e9).round().clamp(0.0, 1e9 - 1.0) as u32,
    )
    .unwrap_or_else(|| panic!("{at} is an instant"))
}

/// The instant `count` calendar months after the epoch-seconds instant `at` — the
/// calendar arithmetic a term's end is computed by, since a term is a number of months
/// and months differ in length.
fn months_on(at: f64, count: u32) -> ::chrono::DateTime<::chrono::Utc> {
    let at = at_instant(at);
    at.checked_add_months(::chrono::Months::new(count))
        .unwrap_or_else(|| panic!("{at} plus {count} months is an instant"))
}

/// `at` as epoch seconds, so a computed term end can be compared against the one the
/// wire reported.
fn epoch_seconds(at: ::chrono::DateTime<::chrono::Utc>) -> f64 {
    at.timestamp() as f64 + f64::from(at.timestamp_subsec_nanos()) / 1e9
}

/// `at` as the modelled `ContractEndDate` states a date: the calendar date, as ISO
/// 8601 writes one.
fn contract_date(at: ::chrono::DateTime<::chrono::Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

/// Require two reported instants to be the same one.
fn assert_instant(actual: f64, expected: f64, member: &str, case: &str) {
    assert!(
        (actual - expected).abs() <= INSTANT,
        "case {case:?}: expected {member} {expected}, got {actual}"
    );
}

/// Rules 1, 2 and 3 over ONE reported subscription of an order.
///
/// `billed_from` is the instant billing on the order's hardware starts, which the
/// caller read off the order itself; it is `None` for an order that delivers nothing.
fn assert_order_subscription(
    reported: &Value,
    row: &OrderRow,
    order_id: &str,
    capacity: i64,
    billed_from: Option<f64>,
) {
    let case = row.case;
    let id = text(reported, "SubscriptionId");
    assert!(
        is_minted_id(&id, SUBSCRIPTION_ID_PREFIX),
        "case {case:?}: expected a subscription id of this service's own form, got {id:?}"
    );
    assert_eq!(
        reported.get("SubscriptionType").and_then(Value::as_str),
        Some(row.expected_type),
        "case {case:?}: expected SubscriptionType {:?}, got {reported}",
        row.expected_type
    );
    assert_eq!(
        reported.get("SubscriptionStatus").and_then(Value::as_str),
        Some(row.expected_status),
        "case {case:?}: expected SubscriptionStatus {:?}, got {reported}",
        row.expected_status
    );
    assert_eq!(
        reported.get("OrderIds"),
        Some(&json!([order_id])),
        "case {case:?}: the subscription is the billing record of order {order_id}, got {reported}"
    );

    match billed_from {
        // Rule 2: billing starts when the hardware is installed, and the term runs the
        // months it was bought for from there.
        Some(billed_from) => {
            assert_instant(
                required_instant(reported, "BeginDate", case),
                billed_from,
                "BeginDate",
                case,
            );
            match row.payment_term {
                Some(term) => {
                    let expected = months_on(billed_from, months(term) as u32);
                    assert_instant(
                        required_instant(reported, "EndDate", case),
                        epoch_seconds(expected),
                        "EndDate",
                        case,
                    );
                }
                None => assert_eq!(
                    instant(reported, "EndDate", case),
                    None,
                    "case {case:?}: an order bought under no term has no term to end, got {reported}"
                ),
            }
        }
        None => {
            assert_eq!(
                instant(reported, "BeginDate", case),
                None,
                "case {case:?}: a cancelled order delivers nothing, so billing never started on \
                 it, got {reported}"
            );
            assert_eq!(
                instant(reported, "EndDate", case),
                None,
                "case {case:?}: and there is no term of it to end, got {reported}"
            );
        }
    }

    // Rule 3: the price follows from the capacity THIS order's line items subscribe,
    // under the terms it was bought under.
    match row.payment_term {
        Some(term) => {
            let monthly_rate = capacity as f64 * f64::from(SUBSCRIPTION_PRICE_PER_VCPU_MONTH);
            let share = upfront_share(row.payment_option);
            assert_price(
                price(reported, "MonthlyRecurringPrice", case),
                monthly_rate * (1.0 - share),
                "MonthlyRecurringPrice",
                case,
            );
            assert_price(
                price(reported, "UpfrontPrice", case),
                monthly_rate * months(term) * share,
                "UpfrontPrice",
                case,
            );
            assert_eq!(
                reported.get("Currency").and_then(Value::as_str),
                Some(CURRENCY_USD),
                "case {case:?}: expected Currency {CURRENCY_USD:?}, got {reported}"
            );
        }
        None => {
            for member in ["MonthlyRecurringPrice", "UpfrontPrice", "Currency"] {
                assert_eq!(
                    reported.get(member),
                    None,
                    "case {case:?}: an order bought under no term is priced over no term, so it \
                     carries no {member}, got {reported}"
                );
            }
        }
    }
}

/// The whole report for `identifier`, with every subscription on one page.
async fn report(outposts: &Outposts, identifier: &str) -> Value {
    outposts
        .ok("GET", &paged_route(identifier, "MaxResults=1000"), None)
        .await
}

/// The subscriptions one answer carries.
fn subscriptions(report: &Value) -> Vec<Value> {
    report
        .get("Subscriptions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected Subscriptions, got {report}"))
        .clone()
}

/// `GetOrder`, for the order whose own dates a subscription's are checked against.
async fn get_order(outposts: &Outposts, order_id: &str) -> Value {
    outposts
        .ok("GET", &format!("/orders/{}", segment(order_id)), None)
        .await["Order"]
        .clone()
}

/// One site and one Outpost on it, ready to take orders.
async fn an_outpost(outposts: &Outposts, name: &str) -> String {
    let (outpost_id, _) = outposts.site_and_active_outpost(name, "US").await;
    outpost_id
}

/// Rules 1 to 5: every subscription an Outpost's own records create, and the contract
/// in force on it.
///
/// The rows span the class the rules generalise over: the Outpost's first order and
/// later ones, two terms and three payment options, one order bought under no term at
/// all, hardware that has arrived and hardware still on its way, an order cancelled
/// after it was placed, and a renewal contract bought on top of the lot.
#[tokio::test]
async fn the_report_derives_one_subscription_per_order_and_renewal() {
    let outposts = Outposts::new();
    let catalog = published_catalog(&outposts).await;
    let (small, _) = catalog.first().cloned().expect("the catalog is not empty");
    let (large, _) = catalog.last().cloned().expect("the catalog is not empty");
    let outpost_id = an_outpost(&outposts, "billed").await;

    let rows = vec![
        OrderRow {
            case: "the Outpost's first order, installed, three-year term",
            items: vec![(large.clone(), 1)],
            payment_option: "ALL_UPFRONT",
            payment_term: Some("THREE_YEARS"),
            delivered: true,
            cancelled: false,
            expected_type: SUBSCRIPTION_TYPE_ORIGINAL,
            expected_status: SUBSCRIPTION_STATUS_ACTIVE,
        },
        OrderRow {
            case: "a capacity-scaling order of two units, installed, one-year term",
            items: vec![(small.clone(), 2)],
            payment_option: "NO_UPFRONT",
            payment_term: Some("ONE_YEAR"),
            delivered: true,
            cancelled: false,
            expected_type: SUBSCRIPTION_TYPE_CAPACITY_INCREASE,
            expected_status: SUBSCRIPTION_STATUS_ACTIVE,
        },
        OrderRow {
            case: "a scaling order whose hardware is still on its way",
            items: vec![(small.clone(), 1), (large.clone(), 1)],
            payment_option: "PARTIAL_UPFRONT",
            payment_term: Some("FIVE_YEARS"),
            delivered: false,
            cancelled: false,
            expected_type: SUBSCRIPTION_TYPE_CAPACITY_INCREASE,
            expected_status: SUBSCRIPTION_STATUS_PENDING,
        },
        OrderRow {
            case: "a scaling order cancelled after it was placed",
            items: vec![(large.clone(), 3)],
            payment_option: "ALL_UPFRONT",
            payment_term: Some("ONE_YEAR"),
            delivered: false,
            cancelled: true,
            expected_type: SUBSCRIPTION_TYPE_CAPACITY_INCREASE,
            expected_status: SUBSCRIPTION_STATUS_CANCELLED,
        },
        OrderRow {
            case: "a scaling order bought under no term, installed",
            items: vec![(small.clone(), 1)],
            payment_option: "NO_UPFRONT",
            payment_term: None,
            delivered: true,
            cancelled: false,
            expected_type: SUBSCRIPTION_TYPE_CAPACITY_INCREASE,
            expected_status: SUBSCRIPTION_STATUS_ACTIVE,
        },
    ];

    let mut order_ids = Vec::new();
    for row in &rows {
        order_ids.push(place(&outposts, &outpost_id, row).await);
    }
    // The renewal is bought on top of the hardware those orders subscribe, so it is
    // bought after they are all placed — and over the wire, which is the only call
    // that records one.
    let renewal = outposts
        .ok(
            "POST",
            "/renewals",
            Some(&json!({
                "OutpostIdentifier": outpost_id,
                "PaymentOption": "PARTIAL_UPFRONT",
                "PaymentTerm": "FIVE_YEARS",
            })),
        )
        .await;

    let answered = report(&outposts, &outpost_id).await;
    let reported = subscriptions(&answered);
    assert_eq!(
        reported.len(),
        rows.len() + 1,
        "expected one subscription per order placed and one for the renewal bought, got {answered}"
    );

    let mut ids = Vec::new();
    for (index, (row, order_id)) in rows.iter().zip(&order_ids).enumerate() {
        let subscription = &reported[index];
        let order = get_order(&outposts, order_id).await;
        // Billing starts when the hardware is installed, which for an order is its own
        // fulfilment: the subscription's begin date is read off the order rather than
        // off a clock this case kept. An order still on its way is billed from the
        // fulfilment the clock owes it, which is a date after it was submitted and one
        // it has not reached; a cancelled order delivers nothing, so billing never
        // starts on it at all.
        let billed_from = match (row.delivered, row.cancelled) {
            (true, _) => Some(required_instant(&order, "OrderFulfilledDate", row.case)),
            (false, false) => {
                let submitted = required_instant(&order, "OrderSubmissionDate", row.case);
                let begin = required_instant(subscription, "BeginDate", row.case);
                assert!(
                    begin > submitted,
                    "case {:?}: expected a begin date after the order was submitted \
                     ({submitted}), got {begin}",
                    row.case
                );
                Some(begin)
            }
            (false, true) => None,
        };
        assert_order_subscription(
            subscription,
            row,
            order_id,
            row_capacity(row, &catalog),
            billed_from,
        );
        ids.push(text(subscription, "SubscriptionId"));
    }

    // Rule 4: the renewal extends the term it renews, so it begins where the latest
    // term already bought ends and is not being billed yet.
    let contract = reported
        .last()
        .unwrap_or_else(|| panic!("expected the renewal's subscription, got {answered}"));
    let case = "the renewal contract bought on top";
    assert_eq!(
        contract.get("SubscriptionType").and_then(Value::as_str),
        Some(SUBSCRIPTION_TYPE_RENEWAL),
        "case {case:?}: expected SubscriptionType {SUBSCRIPTION_TYPE_RENEWAL:?}, got {contract}"
    );
    assert_eq!(
        contract.get("SubscriptionStatus").and_then(Value::as_str),
        Some(SUBSCRIPTION_STATUS_PENDING),
        "case {case:?}: a renewal is not billed until the term it renews ends, got {contract}"
    );
    assert_eq!(
        contract.get("OrderIds"),
        None,
        "case {case:?}: a renewal is no order, so it is the billing record of none, got {contract}"
    );
    let last_term_end = reported
        .iter()
        .take(rows.len())
        .filter_map(|subscription| instant(subscription, "EndDate", case))
        .fold(f64::MIN, f64::max);
    let renewal_begin = required_instant(contract, "BeginDate", case);
    assert_instant(renewal_begin, last_term_end, "BeginDate", case);
    assert_instant(
        required_instant(contract, "EndDate", case),
        epoch_seconds(months_on(renewal_begin, months("FIVE_YEARS") as u32)),
        "EndDate",
        case,
    );
    // Rule 3's renewal half: the figures are the ones the contract was bought at.
    for member in ["MonthlyRecurringPrice", "UpfrontPrice"] {
        assert_price(
            price(contract, member, case),
            price(&renewal, member, case)
                .unwrap_or_else(|| panic!("case {case:?}: CreateRenewal answered {member}")),
            member,
            case,
        );
    }
    assert_eq!(
        contract.get("Currency").and_then(Value::as_str),
        Some(CURRENCY_USD),
        "case {case:?}: expected Currency {CURRENCY_USD:?}, got {contract}"
    );

    // Rule 1's identity half: no two subscriptions of the store share an id.
    ids.push(text(contract, "SubscriptionId"));
    let mut distinct = ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "every subscription carries an id of its own, got {ids:?}"
    );

    // Rule 5: of the terms being billed now, the one that runs longest — the
    // three-year term of the first order, against the one-year term of the second and
    // the term-less third.
    let current = &reported[0];
    let current_end = required_instant(current, "EndDate", "the contract in force");
    assert_eq!(
        answered.get("ContractEndDate").and_then(Value::as_str),
        Some(contract_date(at_instant(current_end)).as_str()),
        "the report states the date the current contract term ends, got {answered}"
    );
    assert_eq!(
        answered.get("PaymentTerm").and_then(Value::as_str),
        Some("THREE_YEARS"),
        "the report states the term of the contract in force, got {answered}"
    );
    assert_eq!(
        answered.get("PaymentOption").and_then(Value::as_str),
        Some("ALL_UPFRONT"),
        "the report states the option that contract was bought under, got {answered}"
    );
}

/// Rule 2's expiry half and rule 5's: a term that has run out is `INACTIVE`, and is
/// not the contract in force.
#[tokio::test]
async fn a_term_that_has_run_out_is_inactive_and_is_not_the_current_contract() {
    let outposts = Outposts::new();
    let catalog = published_catalog(&outposts).await;
    let (sku, _) = catalog.first().cloned().expect("the catalog is not empty");
    let outpost_id = an_outpost(&outposts, "expired").await;
    let row = OrderRow {
        case: "a one-year term installed five years ago",
        items: vec![(sku, 1)],
        payment_option: "ALL_UPFRONT",
        payment_term: Some("ONE_YEAR"),
        delivered: true,
        cancelled: false,
        expected_type: SUBSCRIPTION_TYPE_ORIGINAL,
        expected_status: SUBSCRIPTION_STATUS_INACTIVE,
    };
    let order_id = place(&outposts, &outpost_id, &row).await;
    // The fulfilment materialises on the next call, and only then is there a date to
    // move back.
    report(&outposts, &outpost_id).await;
    backdate_fulfilment(&outposts, &order_id, 5);

    let answered = report(&outposts, &outpost_id).await;
    let reported = subscriptions(&answered);
    assert_eq!(
        reported.len(),
        1,
        "case {:?}: expected the one subscription that order created, got {answered}",
        row.case
    );
    assert_eq!(
        reported[0]
            .get("SubscriptionStatus")
            .and_then(Value::as_str),
        Some(SUBSCRIPTION_STATUS_INACTIVE),
        "case {:?}: a term whose end has passed has run out, got {answered}",
        row.case
    );
    for member in ["ContractEndDate", "PaymentTerm", "PaymentOption"] {
        assert_eq!(
            answered.get(member),
            None,
            "case {:?}: no term is in force, so the report states no {member}, got {answered}",
            row.case
        );
    }
}

/// Rule 5's other half: an Outpost nothing was ordered for has no subscription and no
/// contract, and one whose only order has not arrived yet has a subscription but still
/// no contract in force.
#[tokio::test]
async fn an_outpost_with_no_term_in_force_states_no_contract() {
    let outposts = Outposts::new();
    let catalog = published_catalog(&outposts).await;
    let (sku, _) = catalog.first().cloned().expect("the catalog is not empty");
    let bare = an_outpost(&outposts, "bare").await;
    let awaiting = an_outpost(&outposts, "awaiting").await;
    let row = OrderRow {
        case: "an order whose hardware has not arrived",
        items: vec![(sku, 1)],
        payment_option: "ALL_UPFRONT",
        payment_term: Some("ONE_YEAR"),
        delivered: false,
        cancelled: false,
        expected_type: SUBSCRIPTION_TYPE_ORIGINAL,
        expected_status: SUBSCRIPTION_STATUS_PENDING,
    };
    place(&outposts, &awaiting, &row).await;

    let cases: Vec<(&str, &str, usize)> = vec![
        ("an Outpost nothing was ordered for", &bare, 0),
        (row.case, &awaiting, 1),
    ];
    for (case, identifier, expected) in &cases {
        let answered = report(&outposts, identifier).await;
        assert_eq!(
            subscriptions(&answered).len(),
            *expected,
            "case {case:?}: expected {expected} subscriptions, got {answered}"
        );
        for member in ["ContractEndDate", "PaymentTerm", "PaymentOption", "NextToken"] {
            assert_eq!(
                answered.get(member),
                None,
                "case {case:?}: expected no {member}, got {answered}"
            );
        }
    }
}

/// Rule 6: the subscriptions page, and a token issued while paging another Outpost's
/// report is refused rather than honoured as a boundary of this one.
#[tokio::test]
async fn the_subscriptions_page_and_a_foreign_token_is_refused() {
    let outposts = Outposts::new();
    let catalog = published_catalog(&outposts).await;
    let (sku, _) = catalog.first().cloned().expect("the catalog is not empty");
    let outpost_id = an_outpost(&outposts, "paged").await;
    let other = an_outpost(&outposts, "other").await;
    let row = OrderRow {
        case: "one of four orders placed to page",
        items: vec![(sku, 1)],
        payment_option: "ALL_UPFRONT",
        payment_term: Some("ONE_YEAR"),
        delivered: true,
        cancelled: false,
        expected_type: SUBSCRIPTION_TYPE_ORIGINAL,
        expected_status: SUBSCRIPTION_STATUS_ACTIVE,
    };
    let mut placed = Vec::new();
    for _ in 0..4 {
        placed.push(place(&outposts, &outpost_id, &row).await);
    }
    place(&outposts, &other, &row).await;

    // Every page size from one subscription at a time to more than the report holds:
    // the pages together are the whole report, in the order the whole report gives it,
    // and the last page carries no token.
    let whole: Vec<String> = subscriptions(&report(&outposts, &outpost_id).await)
        .iter()
        .map(|subscription| text(subscription, "SubscriptionId"))
        .collect();
    assert_eq!(
        whole.len(),
        placed.len(),
        "expected one subscription per order placed, got {whole:?}"
    );
    for size in [1usize, 2, 3, 4, 5] {
        let case = &format!("MaxResults={size}");
        let mut seen: Vec<String> = Vec::new();
        let mut query = format!("MaxResults={size}");
        loop {
            let page = outposts
                .ok("GET", &paged_route(&outpost_id, &query), None)
                .await;
            let reported = subscriptions(&page);
            assert!(
                reported.len() <= size,
                "case {case:?}: a page carries at most {size} subscriptions, got {page}"
            );
            seen.extend(
                reported
                    .iter()
                    .map(|subscription| text(subscription, "SubscriptionId")),
            );
            match page.get("NextToken").and_then(Value::as_str) {
                Some(token) => {
                    query = format!("MaxResults={size}&NextToken={}", query_value(token))
                }
                None => break,
            }
        }
        assert_eq!(
            seen, whole,
            "case {case:?}: the pages together are the whole report, in its order"
        );
    }

    // A token this service issued for ANOTHER Outpost's report names no boundary of
    // this one.
    let foreign = outposts
        .ok("GET", &paged_route(&outpost_id, "MaxResults=1"), None)
        .await;
    let token = text(&foreign, "NextToken");
    assert!(!token.is_empty(), "expected a token to resume from");
    let cases: Vec<Refusal> = vec![
        refusal(
            "a token issued while paging another Outpost's report",
            "GET",
            paged_route(&other, &format!("NextToken={}", query_value(&token))),
            400,
            "ValidationException",
        ),
        refusal(
            "a token this service never issued",
            "GET",
            paged_route(&outpost_id, "NextToken=1%23%23nonsense"),
            400,
            "ValidationException",
        ),
    ];
    outposts.expect_refusals(&cases).await;
}

/// Rule 7: both identifier forms name the same report, and asking for it changes
/// nothing a later caller sees.
#[tokio::test]
async fn both_identifier_forms_report_the_same_and_reading_changes_nothing() {
    let outposts = Outposts::new();
    let catalog = published_catalog(&outposts).await;
    let (sku, _) = catalog.last().cloned().expect("the catalog is not empty");
    let (outpost_id, outpost_arn) = outposts.site_and_active_outpost("read", "US").await;
    let row = OrderRow {
        case: "the order the report is derived off",
        items: vec![(sku, 2)],
        payment_option: "PARTIAL_UPFRONT",
        payment_term: Some("THREE_YEARS"),
        delivered: true,
        cancelled: false,
        expected_type: SUBSCRIPTION_TYPE_ORIGINAL,
        expected_status: SUBSCRIPTION_STATUS_ACTIVE,
    };
    place(&outposts, &outpost_id, &row).await;
    // The fulfilment the clock owed falls due on the first call, so the snapshots
    // below differ only if a LATER read changed something.
    let first = report(&outposts, &outpost_id).await;
    let outpost_before = outposts.get_outpost(&outpost_id).await;
    let orders_before = format!("{:?}", outposts.store().lock().orders);

    for pass in ["by bare id", "by ARN", "by bare id again"] {
        let identifier = match pass {
            "by ARN" => outpost_arn.clone(),
            _ => outpost_id.clone(),
        };
        let answered = report(&outposts, &identifier).await;
        assert_eq!(
            answered, first,
            "case {pass:?}: every read of one Outpost's billing information answers the same \
             report"
        );
    }
    assert_eq!(
        outposts.get_outpost(&outpost_id).await,
        outpost_before,
        "reading the billing information must not change the Outpost itself"
    );
    assert_eq!(
        format!("{:?}", outposts.store().lock().orders),
        orders_before,
        "reading the billing information must leave the orders it was derived off alone"
    );
    // The ARN of an Outpost of ANOTHER endpoint names nothing this one holds.
    let elsewhere = format!(
        "arn:aws:outposts:us-west-2:000000000000:outpost/{}",
        outpost_id
    );
    let reply = outposts.call("GET", &route(&elsewhere), None).await;
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (404, "NotFoundException"),
        "an Outpost ARN of another endpoint names no Outpost here, got {} {}",
        reply.status,
        reply.body
    );
}

/// Rule 7's refusal half: an identifier that names no Outpost is 404 with the
/// identifier echo of the value AS SUPPLIED; one that is no Outpost identifier at all
/// is 400.
#[tokio::test]
async fn an_absent_outpost_is_not_found_and_a_malformed_identifier_is_refused() {
    let outposts = Outposts::new();
    let absent_arn = format!("{OUTPOST_ARN_PREFIX}{ABSENT_OUTPOST_ID}");
    let cases: Vec<Refusal> = vec![
        refusal(
            "a well-formed Outpost id that names nothing",
            "GET",
            route(ABSENT_OUTPOST_ID),
            404,
            "NotFoundException",
        ),
        refusal(
            "the ARN form of an Outpost that names nothing",
            "GET",
            route(&absent_arn),
            404,
            "NotFoundException",
        ),
        refusal(
            "a label that is no identifier at all",
            "GET",
            route("not-an-outpost-id"),
            400,
            "ValidationException",
        ),
        refusal(
            "a site id",
            "GET",
            route(ABSENT_SITE_ID),
            400,
            "ValidationException",
        ),
        refusal(
            "an Outpost id whose suffix is not hex",
            "GET",
            route("op-zzzzzzzzzzzzzzzzz"),
            400,
            "ValidationException",
        ),
        refusal(
            "an Outpost id one hex character short",
            "GET",
            route("op-0000000000000000"),
            400,
            "ValidationException",
        ),
    ];
    outposts.expect_refusals(&cases).await;

    for (case, identifier) in [("by bare id", ABSENT_OUTPOST_ID), ("by ARN", &absent_arn)] {
        let reply = outposts.call("GET", &route(identifier), None).await;
        let expected = format!("outpostIdentifier={identifier}");
        let actual = reply
            .body
            .get("Message")
            .or_else(|| reply.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the identifier echo {expected:?}, got {actual:?}"
        );
    }
}
