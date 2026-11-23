// SPDX-License-Identifier: CC0-1.0

//! Paging `ListCustomLineItemVersions` ACROSS a write that changes the history, over the
//! wire: `CreateBillingGroup` -> `CreateCustomLineItem` -> `UpdateCustomLineItem` -> a
//! first page -> a write that drops or rewrites a generation -> the same `NextToken`
//! handed back.
//!
//! RULE: a paginated read cuts its page over a STABLE identity of the element, so the
//! `NextToken` a page hands back names the same cut point on the next call however the
//! collection changed in between: resuming reports exactly the elements that still exist
//! and were not already reported, in the same order, each once — it never re-reports one
//! the first page already carried and never skips one the first page left out. The element
//! of this listing is a GENERATION of the item, and the history is NOT append-only: a
//! write that leaves a generation applying in no billing period takes it out of the
//! history altogether, so the element's position in the history is not an identity a
//! cursor can be cut over, while the ordinal the store mints for a generation names that
//! generation for as long as it is a version of the item.
//!
//! The rows span what the rule generalises over: which write lands between the pages (none,
//! a bounded delete, an update), which generation it changes (the frozen one the first page
//! already reported, the one the token was minted for, both at once), what it does to it
//! (drops it, opens a generation in its place, rewrites the whole history into one
//! generation, moves the periods it applies in without dropping it, changes nothing at
//! all), the shape of the history the writes build (open-ended, bounded by the create's own
//! exclusive end), how a month is spelled, and the page size (each published bound of
//! `MaxCustomLineItemResults`, and a size between them).

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// An AWS-managed pricing plan every billing group here computes with: the
/// `PricingPlanFullArn` pattern enumerates it, so no plan has to be created first
/// (`smithy_query(query="shape",shape="PricingPlanFullArn")`).
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// `MaxCustomLineItemResults`' published bounds — the `@range` of this operation's
/// `MaxResults` (`smithy_query(query="shape",shape="MaxCustomLineItemResults")`).
const MAX_RESULTS_BOUNDS: (i64, i64) = (1, 100);

/// The `Description` each fixture here gives the generation the CREATE leaves, so a
/// generation is identified across calls by a value it carries rather than by where it
/// happens to sit in an answer.
const CREATED: &str = "generation of the create";

/// The `Description` the update that builds the second generation leaves.
const UPDATED: &str = "generation of the first update";

/// A live emulator holding only this service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// A router over ONE shared engine, with the emulated account designated the
    /// organization's PAYER account — the world in which this service's control plane is
    /// reachable at all.
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = Some(::engine::emulator_account_id().to_string());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send a JSON body to one of this service's routes and return the status and the
    /// parsed body.
    async fn send(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://billingconductor.us-east-1.amazonaws.com{path}"
            ))
            .header("host", "billingconductor.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/billingconductor/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create a billing group and return its ARN.
    async fn billing_group(&self, case: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": "payer",
                    "AccountGrouping": { "LinkedAccountIds": [] },
                    "ComputationPreference": { "PricingPlanArn": MANAGED_PRICING_PLAN },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the billing group create to answer HTTP 200, got {status} \
             with {body}"
        );
        member_str(case, &body, "Arn")
    }

    /// Create the custom line item this case pages the history of, applying over the
    /// billing periods `range` names, and return its ARN.
    async fn custom_line_item(&self, case: &str, group_arn: &str, range: Value) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": "paged",
                    "Description": CREATED,
                    "BillingGroupArn": group_arn,
                    "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
                    "BillingPeriodRange": range,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the custom line item create to answer HTTP 200, got {status} \
             with {body}"
        );
        member_str(case, &body, "Arn")
    }

    /// `UpdateCustomLineItem` on `arn` with the members `request` carries, which this case
    /// requires to succeed.
    async fn update_ok(&self, case: &str, arn: &str, request: Value) {
        let mut body = json!({ "Arn": arn });
        for (member, value) in object(&request) {
            body[member] = value;
        }
        let (status, response) = self.send("POST", "/update-custom-line-item", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer HTTP 200, got {status} with {response}"
        );
    }

    /// `DeleteCustomLineItem` on `arn` bounded to `range`, which this case requires to
    /// succeed.
    async fn delete_ok(&self, case: &str, arn: &str, range: Value) {
        let (status, response) = self
            .send(
                "POST",
                "/delete-custom-line-item",
                &json!({ "Arn": arn, "BillingPeriodRange": range }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer HTTP 200, got {status} with {response}"
        );
    }

    /// One `ListCustomLineItemVersions` page: the versions it reports, in order, and the
    /// `NextToken` it hands back. The read must succeed — a token a previous page of this
    /// very listing minted is one the listing serves.
    async fn page(
        &self,
        case: &str,
        arn: &str,
        max_results: Option<i64>,
        next_token: Option<&str>,
    ) -> (Vec<String>, Option<String>) {
        let mut body = json!({ "Arn": arn });
        if let Some(max_results) = max_results {
            body["MaxResults"] = json!(max_results);
        }
        if let Some(token) = next_token {
            body["NextToken"] = json!(token);
        }
        let (status, response) = self
            .send("POST", "/list-custom-line-item-versions", &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the version read {body} to answer HTTP 200, got {status} \
             with {response}"
        );
        let reported = response
            .get("CustomLineItemVersions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let token = response
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        (descriptions(case, &reported), token)
    }
}

/// The members of a JSON object, or none when it is not one.
fn object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// A string member a response is required to carry.
fn member_str(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {body}"))
        .to_string()
}

/// The `Description` of every reported version, in the order they were reported — how a
/// generation is named in this suite.
fn descriptions(case: &str, versions: &[Value]) -> Vec<String> {
    versions
        .iter()
        .map(|version| member_str(case, version, "Description"))
        .collect()
}

/// A `BillingPeriodRange` as a write carries it (`@required InclusiveStartBillingPeriod`,
/// optional `ExclusiveEndBillingPeriod`).
fn period_range(start: &str, end: Option<&str>) -> Value {
    let mut range = json!({ "InclusiveStartBillingPeriod": start });
    if let Some(end) = end {
        range["ExclusiveEndBillingPeriod"] = json!(end);
    }
    range
}

/// A `BillingPeriod` as a comparable month count, so `2019-7` and `2019-07` are the same
/// period — the normalisation the modelled pattern (`^\d{4}-(0?[1-9]|1[012])$`) makes
/// necessary.
fn period_key(value: &str) -> Option<i64> {
    let (year, month) = value.split_once('-')?;
    Some(year.parse::<i64>().ok()? * 12 + month.parse::<i64>().ok()?)
}

/// The billing period `months` away from the CURRENT one: `period(0)` is the current
/// period and `period(-1)` the previous one — the two a write of this service may be
/// applied from — so every fixture here is placed relative to the clock rather than at a
/// fixed month.
fn period(months: i64) -> String {
    let current =
        service_com_amazonaws_billingconductor_awsbillingconductor::state::current_billing_period();
    let key = period_key(&current).expect("the current billing period is spelled YYYY-MM");
    let shifted = key + months;
    format!(
        "{:04}-{:02}",
        (shifted - 1).div_euclid(12),
        (shifted - 1).rem_euclid(12) + 1
    )
}

/// The same billing period with its month spelled without a leading zero — the other
/// spelling the modelled `BillingPeriod` pattern allows. A month from October on has only
/// the one spelling, so for those this is the value it was given.
fn one_digit_month(billing_period: &str) -> String {
    let (year, month) = billing_period
        .split_once('-')
        .expect("a billing period is spelled YYYY-MM");
    format!("{year}-{}", month.trim_start_matches('0'))
}

/// The write that lands BETWEEN the first page and the page resumed from its `NextToken`.
#[derive(Clone)]
enum Between {
    /// `UpdateCustomLineItem` with these members beside the item's `Arn`.
    Update(Value),
    /// `DeleteCustomLineItem` bounded to this `BillingPeriodRange`.
    Delete(Value),
    /// Nothing at all: the control, where the history a token was minted over is the
    /// history it is handed back to.
    Nothing,
}

/// One row of the resume table: the case, the `BillingPeriodRange` the create names, the
/// updates that build the history in order, the write that lands between the pages, and
/// the `Description` of every generation the pages resumed from the first page's
/// `NextToken` must report, in order.
type ResumeRow = (
    &'static str,
    Value,
    Vec<Value>,
    Between,
    Vec<&'static str>,
);

/// Apply the write a row puts between its two pages.
async fn apply(emulator: &Emulator, case: &str, item: &str, between: &Between) {
    match between {
        Between::Update(request) => emulator.update_ok(case, item, request.clone()).await,
        Between::Delete(range) => emulator.delete_ok(case, item, range.clone()).await,
        Between::Nothing => {}
    }
}

/// Walk every remaining page from `token`, at page size `max_results`, and report the
/// generations they carry in order. The walk is bounded: a listing that kept handing back
/// a cursor into a sequence it had already served would never end.
async fn walk(
    emulator: &Emulator,
    case: &str,
    item: &str,
    max_results: Option<i64>,
    token: Option<String>,
) -> Vec<String> {
    let mut walked: Vec<String> = Vec::new();
    let mut next = token;
    let mut pages = 0;
    while let Some(token) = next {
        let (reported, handed_back) = emulator
            .page(case, item, max_results, Some(&token))
            .await;
        walked.extend(reported);
        next = handed_back;
        pages += 1;
        assert!(
            pages <= 8,
            "case {case:?}: expected the resumed walk to end, got {pages} pages reporting \
             {walked:?}"
        );
    }
    walked
}

/// THE RULE: the `NextToken` a page of the version history hands back names the same cut
/// point on the next call however a write changed the history in between — the resumed
/// pages report exactly the generations that still exist and were not already reported,
/// in order, each once.
#[tokio::test]
async fn a_next_token_names_the_same_cut_point_after_a_write_to_the_history() {
    // Every row builds a two-generation history — the create's generation, frozen by an
    // update applied from the current billing period, and that update's — and takes its
    // first page at `MaxResults` 1, the page size that truncates such a history. A write
    // may be applied only from the current or the previous billing period, so the writes
    // start in one of those two while an exclusive end reaches as far forward as the row
    // needs.
    let second_generation = json!({
        "Description": UPDATED,
        "BillingPeriodRange": period_range(&period(0), None),
    });
    let rows: Vec<ResumeRow> = vec![
        (
            "a delete drops the frozen generation the first page reported",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Delete(period_range(&period(-1), Some(&period(0)))),
            vec![UPDATED],
        ),
        (
            "a delete drops the generation the token was minted for",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Delete(period_range(&period(0), None)),
            vec![],
        ),
        (
            "a delete moves the live generation's billing periods later",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Delete(period_range(&period(0), Some(&period(1)))),
            vec![UPDATED],
        ),
        (
            "an update opens a generation in place of the one the token was minted for",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Update(json!({
                "Description": "generation of the write between the pages",
                "BillingPeriodRange": period_range(&period(0), None),
            })),
            vec!["generation of the write between the pages"],
        ),
        (
            "an update rewrites the whole history into one generation",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Update(json!({
                "Description": "generation over every billing period",
                "BillingPeriodRange": period_range(&period(-1), None),
            })),
            vec!["generation over every billing period"],
        ),
        (
            "a write that modified nothing leaves the history as the token found it",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Update(second_generation.clone()),
            vec![UPDATED],
        ),
        (
            "no write at all between the pages",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Nothing,
            vec![UPDATED],
        ),
        (
            "the create named an exclusive end and the delete drops the frozen generation",
            period_range(&period(-1), Some(&period(6))),
            vec![second_generation.clone()],
            Between::Delete(period_range(&period(-1), Some(&period(0)))),
            vec![UPDATED],
        ),
        (
            "the delete's months are spelled without their leading zeros",
            period_range(&period(-1), None),
            vec![second_generation.clone()],
            Between::Delete(period_range(
                &one_digit_month(&period(-1)),
                Some(&one_digit_month(&period(0))),
            )),
            vec![UPDATED],
        ),
    ];

    for (case, create, updates, between, expected) in rows {
        let emulator = Emulator::new();
        let group = emulator.billing_group(case).await;
        let item = emulator.custom_line_item(case, &group, create).await;
        for update in &updates {
            emulator.update_ok(case, &item, update.clone()).await;
        }

        // The first page: the oldest generation, and a cursor into the rest of the
        // history.
        let (first, token) = emulator.page(case, &item, Some(1), None).await;
        assert_eq!(
            first,
            vec![CREATED.to_string()],
            "case {case:?}: expected the first page to report the oldest generation, got {first:?}"
        );
        let Some(token) = token else {
            panic!("case {case:?}: expected the first page to hand back a NextToken");
        };

        apply(&emulator, case, &item, &between).await;

        let walked = walk(&emulator, case, &item, Some(1), Some(token)).await;
        let expected: Vec<String> = expected.iter().map(|value| value.to_string()).collect();
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the pages resumed from the token to report \
             {expected:?}, got {walked:?}"
        );

        // The same thing computed from what the history holds AFTER the write rather than
        // written out: the resumed pages owe exactly the generations a full read reports
        // and the first page had not already reported.
        let (live, unpaged_token) = emulator.page(case, &item, None, None).await;
        assert_eq!(
            unpaged_token, None,
            "case {case:?}: expected a read of the whole history to hand back no NextToken, got \
             {unpaged_token:?}"
        );
        let owed: Vec<String> = live
            .iter()
            .filter(|description| !first.contains(description))
            .cloned()
            .collect();
        assert_eq!(
            walked, owed,
            "case {case:?}: expected the resumed pages to report every generation the first page \
             left out ({owed:?}) once, got {walked:?} out of the history {live:?}"
        );
    }
}

/// THE RULE at every page size the model admits: whatever page the caller asked for, the
/// pages resumed from a token minted before a write that dropped a generation report
/// exactly the generations that still exist and were not already reported.
#[tokio::test]
async fn the_resumed_pages_owe_the_live_generations_at_every_published_page_size() {
    // Each published bound of `MaxResults`, a size between them, and the absent size the
    // server chooses for itself.
    for max_results in [
        Some(MAX_RESULTS_BOUNDS.0),
        Some(2),
        Some(MAX_RESULTS_BOUNDS.1),
        None,
    ] {
        let case = format!("MaxResults {max_results:?} across a dropped generation");
        let case = case.as_str();
        let emulator = Emulator::new();
        let group = emulator.billing_group(case).await;
        let item = emulator
            .custom_line_item(case, &group, period_range(&period(-1), None))
            .await;
        emulator
            .update_ok(
                case,
                &item,
                json!({
                    "Description": UPDATED,
                    "BillingPeriodRange": period_range(&period(0), None),
                }),
            )
            .await;

        let (first, token) = emulator.page(case, &item, max_results, None).await;
        assert!(
            !first.is_empty(),
            "case {case:?}: expected the first page to report at least one generation, got \
             {first:?}"
        );

        // The write between the pages: the frozen generation the first page reported loses
        // its only billing period, so it is no version of the item any more and every
        // generation after it would shift down a position.
        emulator
            .delete_ok(case, &item, period_range(&period(-1), Some(&period(0))))
            .await;

        let walked = walk(&emulator, case, &item, max_results, token).await;
        let (live, _) = emulator.page(case, &item, None, None).await;
        let owed: Vec<String> = live
            .iter()
            .filter(|description| !first.contains(description))
            .cloned()
            .collect();
        assert_eq!(
            walked, owed,
            "case {case:?}: expected the resumed pages to report {owed:?}, got {walked:?} out of \
             the history {live:?} after the first page reported {first:?}"
        );
        // Whichever page size was asked for, the generation the item IS reaches the
        // caller: either the first page carried it or the resumed pages owe it.
        assert!(
            first.contains(&UPDATED.to_string()) || walked.contains(&UPDATED.to_string()),
            "case {case:?}: expected the live generation to be reported, got {first:?} then \
             {walked:?}"
        );
    }
}
