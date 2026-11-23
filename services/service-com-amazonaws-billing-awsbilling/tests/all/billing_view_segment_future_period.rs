// SPDX-License-Identifier: CC0-1.0

//! The rule the modelled `BillingViewSegmentTimeRange$beginDateInclusive`
//! states — "The inclusive start of the time range. This value can't be in the
//! future."
//! (`smithy_query(query="doc",shape="BillingViewSegmentTimeRange",member="beginDateInclusive")`)
//! — over `ListBillingViewSegments`.
//!
//! A request whose `timeRange` opens after the service's clock asks about
//! instants no billing data can have been recorded over, so it is refused with
//! the operation's modelled `ValidationException` (400) naming the member that
//! failed, instead of being answered with the view's stored timeline clipped to
//! future bounds. The rows below differ along everything the rule generalises
//! over: how far past the clock the bound lies (a minute to decades, and a
//! fixed calendar instant), whether the request carries the sibling bound at
//! all, and whether it names the view by ARN or is answered for the caller's
//! own PRIMARY view.
//!
//! The rule is about ONE member, so the neighbours it does not reach are
//! asserted here too: a begin bound at or before the clock is answered, and so
//! is a period with NO begin bound of its own, whose end — the current calendar
//! month's, the default this read uses — always lies in the future. The sibling
//! `endDateExclusive` carries only its own modelled rule ("This value must be
//! after `beginDateInclusive`"), so a future end is a period a caller can ask
//! about.
//!
//! The crate's other dated reads already hold to the same rule where their own
//! members state it, and their suites assert it: `credits.rs` for
//! `GetCredits`' past `startDate` and non-future `endDate`, and
//! `enterprise_support_charge_summary.rs` /
//! `business_support_account_charges.rs` for a `billingMonth` the clock has not
//! reached.
//!
//! The harness (`call`, `error_code`, `now_secs`, `standalone_emulator_with_engine`,
//! `PRIMARY_ARN`) is the one `billing_views.rs` owns, and the civil-calendar
//! `at` is `credit_allocation_history.rs`'s. The store is the service's DEFAULT
//! one: the account's PRIMARY view carries the one segment a standalone account
//! has, unbounded on both sides, which is why an accepted period is reported
//! back exactly as the request bounded it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;

use crate::billing_views::{
    call, error_code, now_secs, standalone_emulator_with_engine, PRIMARY_ARN,
};
use crate::credit_allocation_history::at;

/// The domain the default PRIMARY view's one segment carries
/// (`state::BILLING_DOMAIN_BILLABLE`).
const BILLABLE: &str = "BILLABLE";

/// A `ListBillingViewSegments` request body: the period bounds it carries, and
/// whether it names the PRIMARY view's ARN or leaves the view to the caller's
/// own.
fn request(arn: bool, begin: Option<i64>, end: Option<i64>) -> Value {
    let mut body = json!({});
    if arn {
        body["arn"] = json!(PRIMARY_ARN);
    }
    if begin.is_some() || end.is_some() {
        let mut range = json!({});
        if let Some(begin) = begin {
            range["beginDateInclusive"] = json!(begin);
        }
        if let Some(end) = end {
            range["endDateExclusive"] = json!(end);
        }
        body["timeRange"] = range;
    }
    body
}

/// The whole stored timeline, key by key, so a read can be shown not to write.
fn timeline(engine: &Arc<engine::InMemoryEngine>) -> Vec<String> {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let state = state.lock();
    state
        .billing_view_segments
        .iter()
        .map(|(key, segment)| format!("{key}={segment:?}"))
        .collect()
}

/// The names a `ValidationException` body's `fieldList` carries.
fn named_fields(body: &Value) -> Vec<String> {
    body["fieldList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `(domain, beginDateInclusive, endDateExclusive)` of each reported
/// segment, in the order the answer reports them.
fn reported(body: &Value) -> Vec<(Option<String>, Option<f64>, Option<f64>)> {
    body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            (
                item["domain"].as_str().map(str::to_string),
                item["timeRange"]["beginDateInclusive"].as_f64(),
                item["timeRange"]["endDateExclusive"].as_f64(),
            )
        })
        .collect()
}

/// A row of the refusal table: what it is called, the request it sends, and
/// whether the refusal must be the one the not-in-the-future rule words (the
/// rows that also break another rule are only asserted to be refused as the
/// model declares).
type FutureRow = (&'static str, Value, bool);

#[tokio::test]
async fn a_future_begin_date_inclusive_is_refused() {
    let (router, engine) = standalone_emulator_with_engine();
    let before = timeline(&engine);
    let now = now_secs();
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const YEAR: i64 = 365 * DAY;

    let rows: Vec<FutureRow> = vec![
        (
            "a minute after the clock, with an end an hour later",
            request(true, Some(now + MINUTE), Some(now + HOUR)),
            true,
        ),
        (
            "a minute after the clock, with no end bound of its own",
            request(true, Some(now + MINUTE), None),
            true,
        ),
        (
            "an hour after the clock, with no arn",
            request(false, Some(now + HOUR), Some(now + 2 * HOUR)),
            true,
        ),
        (
            "a day after the clock",
            request(true, Some(now + DAY), Some(now + 2 * DAY)),
            true,
        ),
        (
            "a year after the clock, with no arn",
            request(false, Some(now + YEAR), Some(now + 2 * YEAR)),
            true,
        ),
        (
            "two decades after the clock",
            request(true, Some(now + 20 * YEAR), Some(now + 21 * YEAR)),
            true,
        ),
        (
            "a fixed calendar instant no clock has reached",
            request(true, Some(at(2100, 1, 1, 0)), Some(at(2100, 2, 1, 0))),
            true,
        ),
        (
            // Two rules fail at once: the begin bound is in the future AND the
            // end bound does not come after it. Which sentence AWS answers with
            // is its own, so only the modelled refusal is asserted.
            "a future begin whose end lies before it",
            request(true, Some(now + HOUR), Some(now - HOUR)),
            false,
        ),
    ];

    for (case, body, future_wording) in rows {
        let (status, got) = call(&router, "ListBillingViewSegments", &body).await;
        assert_eq!(
            (status, error_code(&got).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {got:?}"
        );
        let named = named_fields(&got);
        assert!(
            named.iter().any(|name| name == "timeRange"),
            "case {case:?}: expected fieldList to name timeRange, got {named:?}"
        );
        let message = got["message"].as_str().unwrap_or_default().to_string();
        assert!(
            !message.is_empty(),
            "case {case:?}: the refusal carries a message, got {got:?}"
        );
        if future_wording {
            assert!(
                message.contains("beginDateInclusive") && message.contains("future"),
                "case {case:?}: expected the refusal to say beginDateInclusive can't be \
                 in the future, got {message:?}"
            );
        }
        assert!(
            got["items"].is_null(),
            "case {case:?}: a refused read reports no segment, got {got:?}"
        );
    }

    assert_eq!(
        timeline(&engine),
        before,
        "case \"a refused read writes nothing\": the stored timeline must be untouched"
    );
}

#[tokio::test]
async fn a_begin_date_inclusive_at_or_before_the_clock_is_answered() {
    let (router, engine) = standalone_emulator_with_engine();
    let before = timeline(&engine);
    let now = now_secs();
    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;
    const YEAR: i64 = 365 * DAY;

    // (case, beginDateInclusive, endDateExclusive). The clock only ever moves
    // forward between this reading and the handler's, so a bound at `now` is in
    // the past by the time the rule is applied to it.
    let rows: Vec<(&str, i64, i64)> = vec![
        ("the clock's own whole second", now, now + HOUR),
        ("a second before the clock", now - 1, now + HOUR),
        ("an hour before the clock", now - HOUR, now),
        ("a day before the clock", now - DAY, now - HOUR),
        ("a year before the clock", now - YEAR, now - 1),
        (
            "a fixed calendar month in the past",
            at(2024, 3, 1, 0),
            at(2024, 4, 1, 0),
        ),
    ];

    for (case, begin, end) in rows {
        let body = request(true, Some(begin), Some(end));
        let (status, got) = call(&router, "ListBillingViewSegments", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {got:?}"
        );
        // The account's one stored segment is bounded on neither side, so the
        // period the answer reports it over is the requested one exactly.
        assert_eq!(
            reported(&got),
            vec![(
                Some(BILLABLE.to_string()),
                Some(begin as f64),
                Some(end as f64)
            )],
            "case {case:?}: expected the account's segment clipped to the requested \
             period, got {got:?}"
        );
        assert_eq!(
            got["nextToken"].as_str(),
            None,
            "case {case:?}: one segment is one page, got {got:?}"
        );
    }

    assert_eq!(
        timeline(&engine),
        before,
        "case \"an answered read writes nothing\": the stored timeline must be untouched"
    );
}

#[tokio::test]
async fn a_period_with_no_begin_bound_is_answered_though_it_ends_in_the_future() {
    let (router, _engine) = standalone_emulator_with_engine();
    let now = now_secs();
    const DAY: i64 = 86_400;

    // The begin bound this read defaults to is the current calendar month's
    // start, which the clock has passed; the end bound carries no
    // not-in-the-future rule of its own, so a period still open at its end —
    // the default one among them — is answerable.
    let rows: Vec<(&str, Value)> = vec![
        ("no timeRange at all", request(true, None, None)),
        ("no timeRange at all, with no arn", request(false, None, None)),
        (
            "only an endDateExclusive, a day after the clock",
            request(true, None, Some(now + DAY)),
        ),
        (
            "only an endDateExclusive, a year after the clock",
            request(false, None, Some(now + 365 * DAY)),
        ),
    ];

    for (case, body) in rows {
        let (status, got) = call(&router, "ListBillingViewSegments", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {got:?}"
        );
        let got_items = reported(&got);
        assert_eq!(
            got_items.len(),
            1,
            "case {case:?}: expected the account's one segment, got {got:?}"
        );
        let (domain, begin, _end) = got_items[0].clone();
        assert_eq!(
            domain.as_deref(),
            Some(BILLABLE),
            "case {case:?}: expected the {BILLABLE} domain, got {got:?}"
        );
        // Whatever the request left to the read, the period it reports opens at
        // an instant the clock has reached.
        assert!(
            begin.is_some_and(|begin| begin <= now_secs() as f64),
            "case {case:?}: expected a reported begin at or before the clock, got \
             {begin:?}"
        );
    }
}
