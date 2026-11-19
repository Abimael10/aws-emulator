// SPDX-License-Identifier: CC0-1.0

//! The resource-association surface of a PERCENTAGE custom line item, end to end over
//! the wire: `CreateCustomLineItem` ->
//! `BatchAssociateResourcesToCustomLineItem` -> `ListResourcesAssociatedToCustomLineItem`.
//!
//! RULE: `BatchAssociateResourcesToCustomLineItem` PERSISTS one association per
//! requested resource against the percentage custom line item the `TargetArn` names,
//! and reports PER RESOURCE — a resource it associated in
//! `SuccessfullyAssociatedResources`, one it could not in
//! `FailedAssociatedResources` with the `AssociateResourceErrorReason` that
//! classifies the failure — while a fault of the request as a whole (an unknown
//! target, a target that is not a percentage item, a billing period range covering no
//! period) is the call's own modelled error. What it persisted is what
//! `ListResourcesAssociatedToCustomLineItem` reports back, with `Relationship`
//! derived from the class of resource the association names and `EndBillingPeriod`
//! from the range the associating call set.
//!
//! The rows differ along the class of the associated resource (billing group /
//! custom line item), the form the caller names it in (full ARN / bare id), the
//! number of resources in one request, the way a resource fails, and the billing
//! period a read asks for — never along a renaming.
//!
//! The last two suites here are about WHICH billing periods such a call may put an
//! association in effect over. RULE: an association is in effect only in billing periods
//! the custom line item HOLDING it and the resource it NAMES both apply in, so the stored
//! span is the request's range narrowed to the periods the two share — a range naming no
//! period the target applies in is the call's own modelled `ValidationException`, and a
//! resource applying in none of the periods left is that element's
//! `INVALID_BILLING_PERIOD_RANGE`. No read of a billing period can then report an
//! association to an item it reports no version of, which is the same invariant a bounded
//! delete maintains from the other end
//! (`tests/all/bounded_delete_narrows_resource_associations.rs`) and the create applies to
//! its own `AssociatedValues`
//! (`tests/all/create_custom_line_item_association_periods.rs`), whose helpers those rows
//! read the store through.

use crate::bounded_delete_narrows_resource_associations::{
    associated_arns, association_size, item_in, range,
};
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// How many resources ONE `BatchAssociateResourcesToCustomLineItem` request may name:
/// the `ResourceArns` `@length` maximum
/// (`smithy_query(query="shape",shape="CustomLineItemBatchAssociationsList")`: len
/// 1..30). It bounds the request, not the membership the item accumulates.
pub(crate) const MAX_RESOURCES_PER_REQUEST: usize = 30;

/// An AWS-managed pricing plan every billing group here computes with: the
/// `PricingPlanFullArn` pattern enumerates it, so no plan has to be created first
/// (`smithy_query(query="shape",shape="PricingPlanFullArn")`).
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// A live emulator holding only this service.
///
/// `pub(crate)` so the sibling suites about the same surface drive it rather than
/// standing up a second harness of their own (see
/// `tests/all/custom_line_item_associations_accumulate.rs`).
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// A router over ONE shared engine, with the emulated account designated the
    /// organization's PAYER account.
    ///
    /// Billing Conductor is a payer-account-only API (see the `crate::service` module
    /// docs): its control plane is authorized for the payer account alone, and the store
    /// records no payer account by default — so this harness designates one, which is
    /// the world in which the associations these tests are about are reachable at all.
    /// The denial every other caller gets is asserted in
    /// `tests/all/payer_account_gate.rs`, over the default store.
    pub(crate) fn new() -> Self {
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
    /// parsed body. The method matters: the batch association routes are `PUT`
    /// (`smithy_query(query="operation",operation=
    /// "BatchAssociateResourcesToCustomLineItem")`).
    pub(crate) async fn send(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
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
    pub(crate) async fn billing_group(&self, name: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-billing-group",
                &json!({
                    "Name": name,
                    "AccountGrouping": { "LinkedAccountIds": [] },
                    "ComputationPreference": { "PricingPlanArn": MANAGED_PRICING_PLAN },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case billing group {name:?}: expected HTTP 200, got {status} with {body}"
        );
        arn_of(&body, &format!("billing group {name:?}"))
    }

    /// Create a custom line item with the given charge details and return its ARN.
    pub(crate) async fn custom_line_item(
        &self,
        group_arn: &str,
        name: &str,
        charge: Value,
    ) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": name,
                    "Description": "a custom line item",
                    "BillingGroupArn": group_arn,
                    "ChargeDetails": charge,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case custom line item {name:?}: expected HTTP 200, got {status} with {body}"
        );
        arn_of(&body, &format!("custom line item {name:?}"))
    }

    /// A percentage custom line item: the only kind resources can be associated to.
    pub(crate) async fn percentage_item(&self, group_arn: &str, name: &str) -> String {
        self.custom_line_item(
            group_arn,
            name,
            json!({ "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } }),
        )
        .await
    }

    /// A percentage custom line item applying from the billing period `inclusive_start`
    /// on, rather than from the one the create lands in.
    ///
    /// An association is in effect only in billing periods the item HOLDING it and the
    /// resource it names both apply in, so a case whose association reaches back to the
    /// previous billing period needs an item that applies there to hold it — which is
    /// what a create naming that period as its `InclusiveStartBillingPeriod` makes
    /// (`tests/all/create_custom_line_item_billing_period_window.rs` pins the window such
    /// a create may name).
    pub(crate) async fn percentage_item_from(
        &self,
        group_arn: &str,
        name: &str,
        inclusive_start: &str,
    ) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": name,
                    "Description": "a custom line item",
                    "BillingGroupArn": group_arn,
                    "ChargeDetails": {
                        "Type": "FEE",
                        "Percentage": { "PercentageValue": 10.0 },
                    },
                    "BillingPeriodRange": {
                        "InclusiveStartBillingPeriod": inclusive_start,
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case custom line item {name:?} from {inclusive_start}: expected HTTP 200, got \
             {status} with {body}"
        );
        arn_of(&body, &format!("custom line item {name:?}"))
    }

    /// A flat custom line item: a fixed charge value, with no associated resources.
    async fn flat_item(&self, group_arn: &str, name: &str) -> String {
        self.custom_line_item(
            group_arn,
            name,
            json!({ "Type": "CREDIT", "Flat": { "ChargeValue": 7.5 } }),
        )
        .await
    }

    /// `BatchAssociateResourcesToCustomLineItem` with the resources named exactly as
    /// the caller spells them, and an optional `BillingPeriodRange`.
    pub(crate) async fn associate(
        &self,
        target: &str,
        resources: &[String],
        range: Option<Value>,
    ) -> (u16, Value) {
        let mut body = json!({ "TargetArn": target, "ResourceArns": resources });
        if let Some(range) = range {
            body["BillingPeriodRange"] = range;
        }
        self.send(
            "PUT",
            "/batch-associate-resources-to-custom-line-item",
            &body,
        )
        .await
    }

    /// `ListResourcesAssociatedToCustomLineItem`, with whatever extra members the
    /// caller wants on the request.
    async fn associated(&self, target: &str, extra: Value) -> (u16, Value) {
        let mut body = json!({ "Arn": target });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        self.send(
            "POST",
            "/list-resources-associated-to-custom-line-item",
            &body,
        )
        .await
    }

    /// The `(Arn, Relationship)` pairs a read reports, in the order it reports them.
    pub(crate) async fn association_pairs(
        &self,
        target: &str,
        extra: Value,
    ) -> Vec<(String, String)> {
        let (status, body) = self.associated(target, extra).await;
        assert_eq!(
            status, 200,
            "case read of {target}: expected HTTP 200, got {status} with {body}"
        );
        elements(&body)
    }
}

/// The `Arn` a create answered with.
fn arn_of(body: &Value, label: &str) -> String {
    body.get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {label}: expected an Arn member, got {body}"))
        .to_string()
}

/// The `(Arn, Relationship)` pairs of an `AssociatedResources` list.
fn elements(body: &Value) -> Vec<(String, String)> {
    body.get("AssociatedResources")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .map(|element| {
            (
                element
                    .get("Arn")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                element
                    .get("Relationship")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The ARNs of one of the batch response's two lists, in response order.
pub(crate) fn reported(body: &Value, member: &str) -> Vec<String> {
    body.get(member)
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|element| element.get("Arn").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The `(Arn, Error.Reason)` pairs of `FailedAssociatedResources`.
pub(crate) fn failures(body: &Value) -> Vec<(String, String)> {
    body.get("FailedAssociatedResources")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .map(|element| {
            (
                element
                    .get("Arn")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                element
                    .get("Error")
                    .and_then(|error| error.get("Reason"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The bare resource id of a full ARN — the OTHER form every association member
/// admits (`^(arn:...:(customlineitem|billinggroup)/)?[a-zA-Z0-9]{10,12}$`).
fn bare_id(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

/// The `BillingPeriod` `months` after the CURRENT billing period (negative for
/// earlier), in the zero-padded spelling the modelled `BillingPeriod` pattern
/// `^\d{4}-(0?[1-9]|1[012])$` allows.
///
/// An association made with no billing period range of its own is in effect from the
/// period the associating call landed in, and a read that names no billing period is
/// scoped to the current one, so both are decided by the wall clock rather than by a
/// fixed month.
pub(crate) fn period(months: i64) -> String {
    let current =
        service_com_amazonaws_billingconductor_awsbillingconductor::state::current_billing_period();
    let (year, month) = current
        .split_once('-')
        .expect("the current billing period is spelled YYYY-MM");
    let total = year.parse::<i64>().expect("a four-digit year") * 12
        + month.parse::<i64>().expect("a month")
        - 1
        + months;
    format!("{:04}-{:02}", total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// The same billing period with its month spelled without a leading zero — the other
/// spelling the modelled `BillingPeriod` pattern allows. A month from October on has
/// only the one spelling, so for those this is the value it was given.
///
/// `pub(crate)` so a sibling suite that varies the SPELLING of a period it writes or
/// reads names it the one way (see
/// `tests/all/association_size_is_billing_period_scoped.rs`).
pub(crate) fn one_digit_month(billing_period: &str) -> String {
    let (year, month) = billing_period
        .split_once('-')
        .expect("a billing period is spelled YYYY-MM");
    format!("{year}-{}", month.trim_start_matches('0'))
}

/// Every class of resource an association can name, in both the forms the member
/// admits: what is persisted and read back is the resource, not the spelling.
#[tokio::test]
async fn a_batch_association_persists_one_association_per_resource_however_it_is_named() {
    // (case, name the item's OWN billing group as a resource, how many custom line
    // items of that group to use, name them by bare id rather than by full ARN)
    let cases: [(&str, bool, usize, bool); 6] = [
        ("the own billing group by ARN", true, 0, false),
        ("the own billing group by bare id", true, 0, true),
        ("one custom line item by ARN", false, 1, false),
        ("one custom line item by bare id", false, 1, true),
        ("the group and two items by ARN", true, 2, false),
        ("the group and two items by bare id", true, 2, true),
    ];

    for (case, own_group, items, by_id) in cases {
        let emulator = Emulator::new();
        let home = emulator.billing_group("home").await;
        let target = emulator.percentage_item(&home, "target").await;

        // The resources to associate: the item's own billing group is a PARENT of it,
        // custom line items of that same group are CHILDren of it.
        let mut expected: Vec<(String, String)> = Vec::new();
        let mut requested: Vec<String> = Vec::new();
        if own_group {
            requested.push(if by_id { bare_id(&home) } else { home.clone() });
            expected.push((home.clone(), "PARENT".to_string()));
        }
        for index in 0..items {
            let arn = emulator.percentage_item(&home, &format!("item{index}")).await;
            requested.push(if by_id { bare_id(&arn) } else { arn.clone() });
            expected.push((arn, "CHILD".to_string()));
        }

        let (status, body) = emulator.associate(&target, &requested, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            reported(&body, "SuccessfullyAssociatedResources"),
            requested,
            "case {case:?}: expected every resource reported associated as the caller named \
             it, got {body}"
        );
        assert!(
            failures(&body).is_empty(),
            "case {case:?}: expected no failed resource, got {:?}",
            failures(&body)
        );

        // The read reports the RESOURCE, by its full ARN, whichever form the
        // associating call used, with the relationship its class implies.
        let mut reported_back = emulator.association_pairs(&target, json!({})).await;
        reported_back.sort();
        expected.sort();
        assert_eq!(
            reported_back, expected,
            "case {case:?}: expected the persisted associations read back, got \
             {reported_back:?}"
        );
    }
}

/// Associating the same resource again changes nothing: the operation is
/// `@idempotent`, so neither a retry nor a repeat inside one request can hold one
/// resource twice.
#[tokio::test]
async fn re_associating_a_resource_is_idempotent() {
    let emulator = Emulator::new();
    let home = emulator.billing_group("home").await;
    let target = emulator.percentage_item(&home, "target").await;
    // The two resources in scope of the target: its own billing group, and another
    // custom line item of that same group.
    let group = home.clone();
    let item = emulator.percentage_item(&home, "item").await;

    // Rows: one request naming a resource twice, the same request sent twice, and the
    // same resources named in the other spelling and the other order.
    let repeats: [(&str, Vec<String>); 4] = [
        ("one request, one resource twice", vec![group.clone(), group.clone()]),
        ("both resources", vec![group.clone(), item.clone()]),
        ("the same request again", vec![group.clone(), item.clone()]),
        (
            "by bare id, reversed",
            vec![bare_id(&item), bare_id(&group)],
        ),
    ];
    let expected = vec![
        (group.clone(), "PARENT".to_string()),
        (item.clone(), "CHILD".to_string()),
    ];

    for (case, resources) in repeats {
        let (status, body) = emulator.associate(&target, &resources, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert!(
            failures(&body).is_empty(),
            "case {case:?}: an association that already holds is not a failure, got {:?}",
            failures(&body)
        );
        let mut reported_back = emulator.association_pairs(&target, json!({})).await;
        reported_back.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        // After the first row only the group is associated; after every later row
        // both are.
        let wanted = if case == "one request, one resource twice" {
            vec![(group.clone(), "PARENT".to_string())]
        } else {
            wanted
        };
        assert_eq!(
            reported_back, wanted,
            "case {case:?}: expected exactly one association per resource, got \
             {reported_back:?}"
        );
    }
}

/// A resource that cannot be associated fails as ITS OWN element, classified by the
/// modelled `AssociateResourceErrorReason`, while the other resources of the same
/// request are still associated.
#[tokio::test]
async fn a_resource_that_cannot_be_associated_fails_as_its_own_element() {
    let emulator = Emulator::new();
    let home = emulator.billing_group("home").await;
    let target = emulator.percentage_item(&home, "target").await;
    // A resource the target CAN hold, so every row shows one failure beside one
    // association: the billing group the target itself applies to.
    let good = home.clone();

    // The ways ONE resource fails: an id no resource has, a full custom line item ARN
    // and a full billing group ARN of resources that do not exist (so the class of
    // the ARN does not change the classification), and the target named as its own
    // resource in either form.
    // `AssociateResourceErrorReason` values from
    // `smithy_query(query="shape",shape="AssociateResourceErrorReason")`.
    let cases: [(&str, String, &str); 5] = [
        ("an unknown bare id", "ZZZZZZZZZZ".to_string(), "INVALID_ARN"),
        (
            "an unknown custom line item ARN",
            "arn:aws:billingconductor::000000000000:customlineitem/ZZZZZZZZZZ".to_string(),
            "INVALID_ARN",
        ),
        (
            "an unknown billing group ARN",
            "arn:aws:billingconductor::000000000000:billinggroup/ZZZZZZZZZZZZ".to_string(),
            "INVALID_ARN",
        ),
        ("the target itself", target.clone(), "ILLEGAL_CUSTOMLINEITEM"),
        (
            "the target itself by bare id",
            bare_id(&target),
            "ILLEGAL_CUSTOMLINEITEM",
        ),
    ];

    for (case, resource, reason) in cases {
        let (status, body) = emulator
            .associate(&target, &[resource.clone(), good.clone()], None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-resource failure is still a 200 batch answer, got {status} \
             with {body}"
        );
        assert_eq!(
            failures(&body),
            vec![(resource.clone(), reason.to_string())],
            "case {case:?}: expected {resource:?} reported failed with {reason}, got {body}"
        );
        assert_eq!(
            reported(&body, "SuccessfullyAssociatedResources"),
            vec![good.clone()],
            "case {case:?}: the other resource of the same request is still associated, got \
             {body}"
        );
        // Nothing of the failed resource was persisted.
        let reported_back = emulator.association_pairs(&target, json!({})).await;
        assert_eq!(
            reported_back,
            vec![(good.clone(), "PARENT".to_string())],
            "case {case:?}: expected only the resource that succeeded to be associated, got \
             {reported_back:?}"
        );
    }
}

/// EVERY in-scope resource one request names is associated, however many of them the
/// request carries: the size of the request is bounded by `ResourceArns` alone, and
/// no resource of a request within that bound is refused as a limit.
#[tokio::test]
async fn every_in_scope_resource_of_one_request_is_associated() {
    // The number of resources in ONE request. `ResourceArns` is
    // @length(min: 1, max: 30)
    // (`smithy_query(query="shape",shape="CustomLineItemBatchAssociationsList")`), so
    // the rows run from the published minimum, through the `AssociatedValues` maximum
    // a create can express (five) and one past it, to the published maximum.
    for size in [1usize, 2, 5, 6, MAX_RESOURCES_PER_REQUEST] {
        let case = format!("{size} resources in one request");
        let emulator = Emulator::new();
        let home = emulator.billing_group("home").await;
        let target = emulator.percentage_item(&home, "target").await;

        // Every resource in scope of the billing group the target applies to: that
        // group, and custom line items of that same group.
        let mut resources: Vec<String> = vec![home.clone()];
        while resources.len() < size {
            resources.push(
                emulator
                    .percentage_item(&home, &format!("item{}", resources.len()))
                    .await,
            );
        }

        let (status, body) = emulator.associate(&target, &resources, None).await;
        assert_eq!(
            status, 200,
            "case {case}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            reported(&body, "SuccessfullyAssociatedResources"),
            resources,
            "case {case}: expected every resource associated, got {body}"
        );
        assert_eq!(
            failures(&body),
            Vec::new(),
            "case {case}: expected no failed resource, got {body}"
        );

        // What was persisted is what a read reports back.
        let mut reported_back: Vec<String> = emulator
            .association_pairs(&target, json!({ "MaxResults": 100 }))
            .await
            .into_iter()
            .map(|(arn, _)| arn)
            .collect();
        reported_back.sort();
        let mut expected = resources.clone();
        expected.sort();
        assert_eq!(
            reported_back, expected,
            "case {case}: expected the read to report every association, got \
             {reported_back:?}"
        );
    }
}

/// A fault of the REQUEST is the call's own modelled error, not a per-resource one:
/// an unknown target is `ResourceNotFoundException` (404), a target that is not a
/// percentage custom line item and a billing period range that covers no period are
/// `ValidationException` (400).
#[tokio::test]
async fn a_request_level_fault_is_the_calls_own_modelled_error() {
    let emulator = Emulator::new();
    let home = emulator.billing_group("home").await;
    let target = emulator.percentage_item(&home, "target").await;
    let flat = emulator.flat_item(&home, "flat").await;
    let group = emulator.billing_group("group").await;

    // An unknown target, in both the forms `CustomLineItemArn` admits.
    for unknown in [
        "ZZZZZZZZZZ".to_string(),
        "arn:aws:billingconductor::000000000000:customlineitem/ZZZZZZZZZZ".to_string(),
    ] {
        let (status, body) = emulator
            .associate(&unknown, std::slice::from_ref(&group), None)
            .await;
        assert_eq!(
            status, 404,
            "case target {unknown:?}: expected HTTP 404, got {status} with {body}"
        );
        assert_eq!(
            body.get("__type").and_then(Value::as_str),
            Some("ResourceNotFoundException"),
            "case target {unknown:?}: expected the modelled not-found error, got {body}"
        );
    }

    // A flat custom line item has no associated resources to be a percentage of.
    for flat_target in [flat.clone(), bare_id(&flat)] {
        let (status, body) = emulator
            .associate(&flat_target, std::slice::from_ref(&group), None)
            .await;
        assert_eq!(
            status, 400,
            "case flat target {flat_target:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            body.get("__type").and_then(Value::as_str),
            Some("ValidationException"),
            "case flat target {flat_target:?}: expected the modelled validation error, got \
             {body}"
        );
        assert_eq!(
            body.get("Reason").and_then(Value::as_str),
            Some("ILLEGAL_CUSTOMLINEITEM"),
            "case flat target {flat_target:?}: expected the modelled reason, got {body}"
        );
    }

    // The start is inclusive and the end exclusive, so a range whose end is at or
    // before its start covers no billing period — in either spelling of a month.
    let empty_ranges = [
        ("the same period", "2026-3", "2026-3"),
        ("the same period, padded", "2026-03", "2026-3"),
        ("an earlier end", "2026-3", "2026-2"),
        ("an earlier year", "2026-1", "2025-12"),
    ];
    for (case, start, end) in empty_ranges {
        let (status, body) = emulator
            .associate(
                &target,
                std::slice::from_ref(&group),
                Some(json!({
                    "InclusiveStartBillingPeriod": start,
                    "ExclusiveEndBillingPeriod": end,
                })),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?} ({start}..{end}): expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            body.get("Reason").and_then(Value::as_str),
            Some("ILLEGAL_BILLING_PERIOD_RANGE"),
            "case {case:?} ({start}..{end}): expected the modelled reason, got {body}"
        );
    }

    // Nothing was associated by any refused call.
    let reported_back = emulator.association_pairs(&target, json!({})).await;
    assert!(
        reported_back.is_empty(),
        "case after the refusals: a refused call persists no association, got \
         {reported_back:?}"
    );

    // A range that does cover a period is accepted, and so is one with no end at all.
    // Their starts are billing periods whose bill is not closed — the previous period is
    // the earliest one a write may name.
    for (case, range) in [
        (
            "a one-period range",
            json!({
                "InclusiveStartBillingPeriod": period(0),
                "ExclusiveEndBillingPeriod": period(1),
            }),
        ),
        (
            "an open range from the previous billing period",
            json!({ "InclusiveStartBillingPeriod": period(-1) }),
        ),
    ] {
        let (status, body) = emulator
            .associate(&target, std::slice::from_ref(&group), Some(range))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
    }
}

/// A read reports the associations in effect for the billing period it asks for, and
/// only the relationship its filter names: the start is inclusive, the end exclusive,
/// and an association with no end of its own is in effect in every later period.
///
/// The periods are named relative to the CURRENT billing period ([`period`]): a read
/// that names none is scoped to that period, and an association made with no range of
/// its own is in effect from the period the associating call landed in —
/// `tests/all/resource_association_billing_periods.rs` is the suite about that scoping.
#[tokio::test]
async fn a_read_reports_the_associations_the_request_asks_for() {
    let emulator = Emulator::new();
    let home = emulator.billing_group("home").await;
    let target = emulator.percentage_item(&home, "target").await;
    // The bounded association names the billing group the target applies to, the open
    // one a custom line item of that same group.
    let bounded = home.clone();
    let open = emulator.percentage_item(&home, "open").await;

    // One association bounded to the three billing periods from the current one on, one
    // with no range at all.
    let (status, body) = emulator
        .associate(
            &target,
            std::slice::from_ref(&bounded),
            Some(json!({
                "InclusiveStartBillingPeriod": period(0),
                "ExclusiveEndBillingPeriod": period(3),
            })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case the bounded association: expected HTTP 200, got {status} with {body}"
    );
    assert_eq!(
        emulator
            .associate(&target, std::slice::from_ref(&open), None)
            .await
            .0,
        200,
        "case the open association: expected HTTP 200"
    );

    // `EndBillingPeriod` is the exclusive end the associating call set, and absent
    // where it set none.
    let (_, body) = emulator.associated(&target, json!({})).await;
    let ends: Vec<(String, Option<String>)> = body
        .get("AssociatedResources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|element| {
            (
                element
                    .get("Arn")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                element
                    .get("EndBillingPeriod")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect();
    assert!(
        ends.contains(&(bounded.clone(), Some(period(3))))
            && ends.contains(&(open.clone(), None)),
        "case EndBillingPeriod: expected the bounded association to report its exclusive end \
         and the open one none, got {ends:?}"
    );

    // Each published bound of the bounded range, and one period past each — plus a month
    // long before either association was made.
    // (case, the billing period the read asks for, is the bounded association in effect
    // there, is the one with no range of its own)
    let probes: [(&str, String, bool, bool); 6] = [
        (
            "the period before both associations",
            period(-1),
            false,
            false,
        ),
        (
            "a period long before either association",
            "2020-01".to_string(),
            false,
            false,
        ),
        (
            "the bounded range's inclusive start",
            period(0),
            true,
            true,
        ),
        (
            "the same period with a one-digit month",
            one_digit_month(&period(0)),
            true,
            true,
        ),
        (
            "the last period of the bounded range",
            period(2),
            true,
            true,
        ),
        (
            "the bounded range's exclusive end",
            period(3),
            false,
            true,
        ),
    ];
    for (case, billing_period, bounded_in_effect, open_in_effect) in probes {
        let reported_back = emulator
            .association_pairs(&target, json!({ "BillingPeriod": billing_period }))
            .await;
        let arns: Vec<String> = reported_back
            .iter()
            .map(|(arn, _)| arn.clone())
            .collect();
        assert_eq!(
            arns.contains(&open),
            open_in_effect,
            "case {case:?} ({billing_period}): expected the association with no range of its \
             own to be {}, got {arns:?}",
            if open_in_effect { "reported" } else { "left out" }
        );
        assert_eq!(
            arns.contains(&bounded),
            bounded_in_effect,
            "case {case:?} ({billing_period}): expected the bounded association to be {}, got \
             {arns:?}",
            if bounded_in_effect { "reported" } else { "left out" }
        );
    }

    // `Filters.Relationship` keeps only the class it names: the billing group is a
    // PARENT, the other custom line item a CHILD.
    for (relationship, expected) in [("PARENT", &bounded), ("CHILD", &open)] {
        let reported_back = emulator
            .association_pairs(&target, json!({ "Filters": { "Relationship": relationship } }))
            .await;
        assert_eq!(
            reported_back,
            vec![(expected.clone(), relationship.to_string())],
            "case filter {relationship:?}: expected only the associations of that \
             relationship, got {reported_back:?}"
        );
    }

    // `MaxResults` bounds a page and the `NextToken` resumes at the element left out,
    // so a walk of one-element pages sees every association exactly once.
    let mut walked: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..4 {
        let request = match &token {
            Some(token) => json!({ "MaxResults": 1, "NextToken": token }),
            None => json!({ "MaxResults": 1 }),
        };
        let (status, body) = emulator.associated(&target, request).await;
        assert_eq!(
            status, 200,
            "case the paginated walk: expected HTTP 200, got {status} with {body}"
        );
        let page = elements(&body);
        assert!(
            page.len() <= 1,
            "case the paginated walk: expected at most MaxResults=1 elements, got {page:?}"
        );
        walked.extend(page);
        token = body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    walked.sort();
    let mut expected = vec![
        (bounded, "PARENT".to_string()),
        (open, "CHILD".to_string()),
    ];
    expected.sort();
    assert_eq!(
        walked, expected,
        "case the paginated walk: expected every association exactly once, got {walked:?}"
    );
}

/// A custom line item is created against a billing group that exists, with exactly
/// one of `Flat` or `Percentage`, and a read of its associations is a 404 before it
/// exists — the create is what makes the target of an association reachable.
#[tokio::test]
async fn creating_a_custom_line_item_refuses_what_it_cannot_persist() {
    let emulator = Emulator::new();
    let home = emulator.billing_group("home").await;

    // (case, charge details, the reason the model enumerates)
    // `ValidationExceptionReason` values from
    // `smithy_query(query="shape",shape="ValidationExceptionReason")`.
    let refused: [(&str, Value, Value, &str); 6] = [
        (
            "neither Flat nor Percentage",
            json!(home),
            json!({ "Type": "FEE" }),
            "ILLEGAL_CHARGE_DETAILS",
        ),
        (
            "both Flat and Percentage",
            json!(home),
            json!({
                "Type": "FEE",
                "Flat": { "ChargeValue": 1.0 },
                "Percentage": { "PercentageValue": 1.0 },
            }),
            "ILLEGAL_CHARGE_DETAILS",
        ),
        (
            "a billing group that does not exist",
            json!("ZZZZZZZZZZZZ"),
            json!({ "Type": "FEE", "Flat": { "ChargeValue": 1.0 } }),
            "MISSING_BILLINGGROUP",
        ),
        (
            "an associated value that does not exist",
            json!(home),
            json!({
                "Type": "FEE",
                "Percentage": { "PercentageValue": 1.0, "AssociatedValues": ["ZZZZZZZZZZ"] },
            }),
            "INVALID_ARN",
        ),
        (
            "the same associated value twice",
            json!(home),
            json!({
                "Type": "FEE",
                "Percentage": {
                    "PercentageValue": 1.0,
                    "AssociatedValues": [home.clone(), home.clone()],
                },
            }),
            "ILLEGAL_RESOURCE_ARNS",
        ),
        (
            // One resource in BOTH forms the member admits is still one resource, so
            // it is the same refusal: the check is on the resource, not the spelling.
            "the same resource in both forms",
            json!(home),
            json!({
                "Type": "FEE",
                "Percentage": {
                    "PercentageValue": 1.0,
                    "AssociatedValues": [home.clone(), bare_id(&home)],
                },
            }),
            "ILLEGAL_RESOURCE_ARNS",
        ),
    ];

    for (case, group, charge, reason) in refused {
        let (status, body) = emulator
            .send(
                "POST",
                "/create-custom-line-item",
                &json!({
                    "Name": "item",
                    "Description": "a custom line item",
                    "BillingGroupArn": group,
                    "ChargeDetails": charge,
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            body.get("Reason").and_then(Value::as_str),
            Some(reason),
            "case {case:?}: expected the modelled reason {reason}, got {body}"
        );
    }

    // A percentage item created WITH `AssociatedValues` already holds those
    // associations: the create is the other way a resource gets associated.
    let seeded = emulator
        .custom_line_item(
            &home,
            "seeded",
            json!({
                "Type": "FEE",
                "Percentage": { "PercentageValue": 2.5, "AssociatedValues": [home.clone()] },
            }),
        )
        .await;
    assert_eq!(
        emulator.association_pairs(&seeded, json!({})).await,
        vec![(home.clone(), "PARENT".to_string())],
        "case AssociatedValues at create: expected the association to be persisted by the \
         create"
    );

    // A `ClientToken` replay creates no second item.
    let body = json!({
        "Name": "tokened",
        "Description": "a custom line item",
        "BillingGroupArn": home,
        "ChargeDetails": { "Type": "FEE", "Flat": { "ChargeValue": 1.0 } },
        "ClientToken": "a-token",
    });
    let (first_status, first) = emulator.send("POST", "/create-custom-line-item", &body).await;
    let (second_status, second) = emulator.send("POST", "/create-custom-line-item", &body).await;
    assert_eq!(
        (first_status, second_status),
        (200, 200),
        "case a ClientToken replay: expected both calls to succeed, got {first} and {second}"
    );
    assert_eq!(
        first.get("Arn"),
        second.get("Arn"),
        "case a ClientToken replay: expected the ARN the first call minted, got {first} and \
         {second}"
    );
}

/// A PERCENTAGE custom line item of `group` applying over the billing periods
/// `billing_period_range` names — `None` for the create that names none, which applies
/// from the billing period it lands in on.
async fn item_over(
    emulator: &Emulator,
    case: &str,
    group: &str,
    name: &str,
    billing_period_range: Option<&Value>,
) -> String {
    let mut body = json!({
        "Name": name,
        "Description": "an item whose associations are bounded",
        "BillingGroupArn": group,
        "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
    });
    if let Some(billing_period_range) = billing_period_range {
        body["BillingPeriodRange"] = billing_period_range.clone();
    }
    let (status, answered) = emulator
        .send("POST", "/create-custom-line-item", &body)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create of {name:?} to answer HTTP 200, got {status} with \
         {answered}"
    );
    arn_of(&answered, &format!("custom line item {name:?}"))
}

/// A `DeleteCustomLineItem` bounded to `billing_period_range`, asserted to be applied:
/// what carves a HOLE in the billing periods an item applies in, so that the periods it
/// applies in are several separate stretches.
async fn delete_periods(emulator: &Emulator, case: &str, arn: &str, billing_period_range: &Value) {
    let (status, body) = emulator
        .send(
            "POST",
            "/delete-custom-line-item",
            &json!({ "Arn": arn, "BillingPeriodRange": billing_period_range }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the bounded delete to answer HTTP 200, got {status} with {body}"
    );
}

/// The member a refusal names as at fault, or `None` when it names none.
fn refused_member(body: &Value) -> Option<String> {
    body.get("Fields")?
        .as_array()?
        .first()?
        .get("Name")?
        .as_str()
        .map(str::to_string)
}

/// The resource an association NAMES: the target's own billing group, which carries the
/// charges of the accounts it bills in every period a write may land in, or another custom
/// line item of that group, which applies only in the billing periods its own generations
/// hold.
enum Named {
    BillingGroup,
    Item {
        /// The `BillingPeriodRange` the named item is created with.
        range: Option<Value>,
        /// A bounded delete applied to it BEFORE the association, carving a hole in the
        /// periods it applies in; `None` where there is none.
        hole: Option<Value>,
    },
}

/// One row: how the TARGET and the NAMED resource were set up, the range the associating
/// call carries, and whether the association is in effect in each probed billing period.
struct PeriodRow {
    case: &'static str,
    /// The `BillingPeriodRange` the target is created with; `None` for the create that
    /// names none.
    target_range: Option<Value>,
    /// A bounded delete applied to the TARGET before the association.
    target_hole: Option<Value>,
    named: Named,
    /// The `BillingPeriodRange` the associating call names; `None` for the call that
    /// names none.
    requested: Option<Value>,
    /// (the billing period a read asks about, is the association in effect there)
    probes: Vec<(String, bool)>,
}

/// THE RULE: the association a batch associate stores is in effect in exactly the billing
/// periods the request's range, the TARGET item's own periods and the NAMED resource's own
/// periods all share — so a period reporting no version of either end reports no
/// association between them, and every period all three cover does.
#[tokio::test]
async fn an_association_is_in_effect_only_where_the_item_and_the_resource_both_apply() {
    let rows: Vec<PeriodRow> = vec![
        PeriodRow {
            case: "an item and a named item applying over the same periods",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: None,
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-2), false),
                (period(-1), true),
                (period(0), true),
                (period(6), true),
            ],
        },
        PeriodRow {
            case: "a named item that stops applying before the range ends",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), Some(&period(0)))),
                hole: None,
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(3), false),
            ],
        },
        PeriodRow {
            case: "a target that stops applying before the range ends",
            target_range: Some(range(&period(-1), Some(&period(0)))),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: None,
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(1), false),
            ],
        },
        PeriodRow {
            case: "a named item that only starts applying later than the range does",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: None,
                hole: None,
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), false),
                (period(0), true),
                (period(2), true),
            ],
        },
        PeriodRow {
            case: "a range bounded inside the periods both of them apply in",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: None,
            },
            requested: Some(range(&period(-1), Some(&period(0)))),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(1), false),
            ],
        },
        PeriodRow {
            case: "a call naming no range at all",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: None,
            },
            requested: None,
            probes: vec![
                (period(-1), false),
                (period(0), true),
                (period(2), true),
            ],
        },
        PeriodRow {
            case: "a named item with a hole carved out of the periods it applies in",
            target_range: Some(range(&period(-1), None)),
            target_hole: None,
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: Some(range(&period(0), Some(&period(1)))),
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(1), true),
                (period(4), true),
            ],
        },
        PeriodRow {
            case: "a target with a hole carved out of the periods it applies in",
            target_range: Some(range(&period(-1), None)),
            target_hole: Some(range(&period(0), Some(&period(1)))),
            named: Named::Item {
                range: Some(range(&period(-1), None)),
                hole: None,
            },
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(1), true),
                (period(4), true),
            ],
        },
        PeriodRow {
            // A billing group is not bounded by periods of its own, so the TARGET's are
            // the only bound on an association naming it.
            case: "the billing group named by a target bounded to the previous period",
            target_range: Some(range(&period(-1), Some(&period(0)))),
            target_hole: None,
            named: Named::BillingGroup,
            requested: Some(range(&period(-1), None)),
            probes: vec![
                (period(-1), true),
                (period(0), false),
                (period(1), false),
            ],
        },
        PeriodRow {
            case: "the billing group named by a target created with no range at all",
            target_range: None,
            target_hole: None,
            named: Named::BillingGroup,
            requested: None,
            probes: vec![
                (period(-1), false),
                (period(0), true),
                (period(3), true),
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let target = item_over(&emulator, case, &group, "target", row.target_range.as_ref()).await;
        if let Some(hole) = &row.target_hole {
            delete_periods(&emulator, case, &target, hole).await;
        }
        let resource = match &row.named {
            Named::BillingGroup => group.clone(),
            Named::Item { range, hole } => {
                let named = item_over(&emulator, case, &group, "named", range.as_ref()).await;
                if let Some(hole) = hole {
                    delete_periods(&emulator, case, &named, hole).await;
                }
                named
            }
        };

        let (status, body) = emulator
            .associate(
                &target,
                std::slice::from_ref(&resource),
                row.requested.clone(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the association to answer HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            reported(&body, "SuccessfullyAssociatedResources"),
            vec![resource.clone()],
            "case {case:?}: expected the resource reported associated, got {body}"
        );

        for (billing_period, expected) in &row.probes {
            let listed = associated_arns(&emulator, case, &target, billing_period)
                .await
                .unwrap_or_default();
            let holds = listed.contains(&resource);
            assert_eq!(
                holds, *expected,
                "case {case:?}: expected the association to be in effect in {billing_period}: \
                 {expected}, got {holds} from {listed:?}"
            );

            // THE RULE as the invariant two reads of one period have to agree on: a period
            // reporting no version of the item holding the association — or none of the
            // item it names — reports no association between them.
            let target_reported = item_in(&emulator, case, &target, billing_period)
                .await
                .is_some();
            let named_reported = match &row.named {
                Named::BillingGroup => true,
                Named::Item { .. } => item_in(&emulator, case, &resource, billing_period)
                    .await
                    .is_some(),
            };
            assert!(
                !holds || (target_reported && named_reported),
                "case {case:?}: {billing_period} reports a version of the target: \
                 {target_reported} and of the named resource: {named_reported}, so it may \
                 report no association between them, got {listed:?}"
            );

            // `AssociationSize` counts the associations in effect in the period it is read
            // for, and a period reporting no version of the target reports no size at all,
            // so the two period-scoped reads of the one association agree.
            let size = association_size(&emulator, case, &target, billing_period).await;
            assert_eq!(
                size,
                target_reported.then_some(i64::from(holds)),
                "case {case:?}: expected the AssociationSize of {billing_period} to count the \
                 one association only where it is in effect, got {size:?}"
            );
        }
    }
}

/// THE RULE on a RE-association: a second call for a resource the item already holds adds
/// the periods it names bounded exactly as the first one was, so the ONE record ends up in
/// effect over the stretches the request's range and both resources share — several
/// stretches when a bounded delete carved a hole in the periods one of them applies in.
#[tokio::test]
async fn a_re_association_adds_only_the_periods_the_item_and_the_resource_share() {
    let case = "a named item with a hole, re-associated over every period of the item";
    let emulator = Emulator::new();
    let group = emulator.billing_group("payer").await;
    let target = item_over(
        &emulator,
        case,
        &group,
        "target",
        Some(&range(&period(-1), None)),
    )
    .await;
    let named = item_over(
        &emulator,
        case,
        &group,
        "named",
        Some(&range(&period(-1), None)),
    )
    .await;
    // The named item applies in the previous billing period and from the next one on, but
    // not in the current one.
    delete_periods(
        &emulator,
        case,
        &named,
        &range(&period(0), Some(&period(1))),
    )
    .await;

    // The first call puts only the later stretch in effect.
    let (status, body) = emulator
        .associate(
            &target,
            std::slice::from_ref(&named),
            Some(range(&period(1), None)),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first association to answer HTTP 200, got {status} with \
         {body}"
    );

    // The second names every period the target applies in: the periods it adds are the
    // ones the named item applies in and the association does not already cover.
    let (status, body) = emulator
        .associate(
            &target,
            std::slice::from_ref(&named),
            Some(range(&period(-1), None)),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the re-association to answer HTTP 200, got {status} with {body}"
    );
    assert_eq!(
        reported(&body, "SuccessfullyAssociatedResources"),
        vec![named.clone()],
        "case {case:?}: expected the resource reported associated, got {body}"
    );

    for (billing_period, expected) in [
        (period(-1), true),
        (period(0), false),
        (period(1), true),
        (period(5), true),
    ] {
        let listed = associated_arns(&emulator, case, &target, &billing_period)
            .await
            .unwrap_or_default();
        assert_eq!(
            listed.contains(&named),
            expected,
            "case {case:?}: expected the association to be in effect in {billing_period}: \
             {expected}, got {listed:?}"
        );
        // ONE record however many stretches it is in effect over, so the resource is
        // counted once in every period it is in effect in.
        let size = association_size(&emulator, case, &target, &billing_period).await;
        assert_eq!(
            size,
            Some(i64::from(expected)),
            "case {case:?}: expected the AssociationSize of {billing_period} to count the one \
             record once, got {size:?}"
        );
    }
}

/// One row of the refused-range table: the case, the TARGET's own `BillingPeriodRange`,
/// a bounded delete applied to it before the association, and the range the associating
/// call names (`None` for the call that names none).
type RefusedRangeRow = (
    &'static str,
    Option<Value>,
    Option<Value>,
    Option<Value>,
);

/// One row of the refused-element table: the case, the NAMED item's own
/// `BillingPeriodRange`, a bounded delete applied to it before the association, and the
/// range the associating call names.
type RefusedElementRow = (&'static str, Option<Value>, Option<Value>, Value);

/// THE RULE where the TARGET leaves nothing: a range naming no billing period the target
/// applies in is a fault of the request as a whole — the modelled 400 `ValidationException`
/// carrying the `ValidationExceptionReason` the model enumerates for an illegal billing
/// period range and the member at fault — and NOTHING is written, rather than an
/// association in effect in a period the item has no version in.
#[tokio::test]
async fn a_range_naming_no_period_the_target_applies_in_is_the_calls_own_refusal() {
    // (case, the target's own range, a bounded delete applied to it first, the range the
    // associating call names)
    let rows: Vec<RefusedRangeRow> = vec![
        (
            "a target applying in the previous billing period only",
            Some(range(&period(-1), Some(&period(0)))),
            None,
            Some(range(&period(0), None)),
        ),
        (
            "a target applying in the previous period only, the call naming no range",
            Some(range(&period(-1), Some(&period(0)))),
            None,
            None,
        ),
        (
            "a target that only starts applying in the current billing period",
            None,
            None,
            Some(range(&period(-1), Some(&period(0)))),
        ),
        (
            "a target cut back to the previous period by a bounded delete",
            Some(range(&period(-1), None)),
            Some(range(&period(0), None)),
            Some(range(&period(0), None)),
        ),
        (
            "a range falling inside the hole a bounded delete carved",
            Some(range(&period(-1), None)),
            Some(range(&period(0), Some(&period(1)))),
            Some(range(&period(0), Some(&period(1)))),
        ),
    ];

    for (case, target_range, target_hole, requested) in rows {
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let target = item_over(&emulator, case, &group, "target", target_range.as_ref()).await;
        if let Some(hole) = &target_hole {
            delete_periods(&emulator, case, &target, hole).await;
        }

        // The billing group the target applies to is always in scope and has no periods of
        // its own, so the only thing that can refuse this call is the range.
        let (status, body) = emulator
            .associate(&target, std::slice::from_ref(&group), requested)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            body.get("__type").and_then(Value::as_str),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
        assert_eq!(
            body.get("Reason").and_then(Value::as_str),
            Some("ILLEGAL_BILLING_PERIOD_RANGE"),
            "case {case:?}: expected the modelled reason for an illegal billing period range, \
             got {body}"
        );
        assert_eq!(
            refused_member(&body).as_deref(),
            Some("BillingPeriodRange"),
            "case {case:?}: expected the member at fault named in Fields, got {body}"
        );
        let message = body
            .get("Message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains(&target),
            "case {case:?}: expected the refusal to name the item whose periods the range \
             names none of, got {message:?}"
        );

        // Nothing was written: no billing period reports the association the refused call
        // would have made.
        for billing_period in [period(-2), period(-1), period(0), period(1), period(3)] {
            let listed = associated_arns(&emulator, case, &target, &billing_period)
                .await
                .unwrap_or_default();
            assert!(
                !listed.contains(&group),
                "case {case:?}: expected the refused call to write nothing, yet a read of \
                 {billing_period} reports {listed:?}"
            );
        }
    }
}

/// THE RULE where ONE named resource leaves nothing: a resource applying in none of the
/// billing periods the call would associate it in is that element's own failure, carrying
/// the `AssociateResourceErrorReason` the model enumerates for a billing period range this
/// element cannot be associated over — while the other resources of the same request are
/// associated.
#[tokio::test]
async fn a_resource_applying_in_none_of_the_requested_periods_fails_as_its_own_element() {
    // (case, the named item's range, a bounded delete applied to it first, the range the
    // associating call names)
    let rows: Vec<RefusedElementRow> = vec![
        (
            "a named item applying in the previous billing period only",
            Some(range(&period(-1), Some(&period(0)))),
            None,
            range(&period(0), None),
        ),
        (
            "a named item that only starts applying in the current billing period",
            None,
            None,
            range(&period(-1), Some(&period(0))),
        ),
        (
            "a named item cut back to the previous period by a bounded delete",
            Some(range(&period(-1), None)),
            Some(range(&period(0), None)),
            range(&period(0), None),
        ),
        (
            "a named item with a hole the range falls inside",
            Some(range(&period(-1), None)),
            Some(range(&period(0), Some(&period(1)))),
            range(&period(0), Some(&period(1))),
        ),
    ];

    for (case, named_range, named_hole, requested) in rows {
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        // The target applies from the previous billing period on, the widest window a
        // write may name, so every range below names periods IT applies in: what is left
        // to refuse is the named item alone.
        let target = item_over(
            &emulator,
            case,
            &group,
            "target",
            Some(&range(&period(-1), None)),
        )
        .await;
        let named = item_over(&emulator, case, &group, "named", named_range.as_ref()).await;
        if let Some(hole) = &named_hole {
            delete_periods(&emulator, case, &named, hole).await;
        }

        // ONE request naming both the item that cannot hold the association and the
        // billing group that can: the batch reports an outcome per resource.
        let resources = vec![named.clone(), group.clone()];
        let (status, body) = emulator
            .associate(&target, &resources, Some(requested.clone()))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the call to answer HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            failures(&body),
            vec![(named.clone(), "INVALID_BILLING_PERIOD_RANGE".to_string())],
            "case {case:?}: expected the named item reported failed with the modelled reason, \
             got {body}"
        );
        assert_eq!(
            reported(&body, "SuccessfullyAssociatedResources"),
            vec![group.clone()],
            "case {case:?}: expected the other resource of the same request associated, got \
             {body}"
        );

        // Nothing was written for the refused element, in any period, while the element
        // that succeeded is in effect in the period the range begins in.
        let start = requested
            .get("InclusiveStartBillingPeriod")
            .and_then(Value::as_str)
            .expect("the range names its inclusive start")
            .to_string();
        for billing_period in [period(-1), period(0), period(1), period(3)] {
            let listed = associated_arns(&emulator, case, &target, &billing_period)
                .await
                .unwrap_or_default();
            assert!(
                !listed.contains(&named),
                "case {case:?}: expected the failed element to write nothing, yet a read of \
                 {billing_period} reports {listed:?}"
            );
        }
        let listed = associated_arns(&emulator, case, &target, &start)
            .await
            .unwrap_or_default();
        assert!(
            listed.contains(&group),
            "case {case:?}: expected the associated resource to be readable back for {start}, \
             got {listed:?}"
        );
    }
}
