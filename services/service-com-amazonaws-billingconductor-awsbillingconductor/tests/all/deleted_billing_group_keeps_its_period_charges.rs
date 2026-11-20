// SPDX-License-Identifier: CC0-1.0

//! What a billing-group delete leaves the billing periods BEFORE it holding of the
//! group's custom line items, end to end over the service's real `restJson1` wire
//! through [`MultiServiceRouter::dispatch`]: `CreateBillingGroup` ->
//! `CreateCustomLineItem` -> `BatchAssociateResourcesToCustomLineItem` ->
//! `DeleteBillingGroup` -> `GetBillingGroupCostReport` / `ListBillingGroupCostReports` /
//! `ListCustomLineItems` / `ListResourcesAssociatedToCustomLineItem`.
//!
//! RULE: a delete that names no billing-period range ends every billing-period-scoped
//! record it takes with it FROM the billing period the call is made in and never earlier,
//! so a read scoped to a period before the delete reports and prices the group's custom
//! line items and their resource associations exactly as it did before the delete, while
//! no read from the delete's own period on reports any of them — and a record that was in
//! effect in no earlier period is left in effect in none at all, so nothing of it is
//! reported anywhere. It is the bound the group's own memberships end at
//! (`tests/all/deleted_billing_group_period_reads.rs` holds the group and its membership
//! to it), asked of the CHARGES the group carried.
//!
//! The rows differ along what the rule generalises over: the billing period the item was
//! put in effect from (the previous one, the current one, none named at all), how its
//! range is bounded (open-ended, ending at the current period, ending past it), the charge
//! it carries (a fee, a credit, a percentage of another item, a percentage of the group
//! itself), how many periods of the group's life precede the delete, and which of the four
//! period-scoped reads asks.
//!
//! The wire can only create a group NOW, so a row creates it through the real wire and
//! then backdates the group's own instants — the one thing these reads cannot ask the
//! service for. An ITEM needs no such help: a create may put one in effect from the
//! previous billing period on.

use crate::association_delete_gate_spans_billing_periods::error_code;
use crate::bounded_delete_narrows_resource_associations::range;
use crate::percentage_custom_line_item_resource_associations::period;
use crate::pricing_rule_association_periods::period_start_millis;
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// An AWS-managed pricing plan every billing group here computes with: the
/// `PricingPlanFullArn` pattern enumerates it, so no plan has to be created first
/// (`smithy_query(query="shape",shape="PricingPlanFullArn")`).
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// How close two reported amounts count as equal: the wire carries a cost report's amounts
/// as decimal strings, and the amounts here are sums of them.
const TOLERANCE: f64 = 1e-9;

/// A live emulator holding only this service, with the engine kept so a row can backdate
/// the creation the wire could only make now.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    /// A router over ONE shared engine, with the emulated account designated the
    /// organization's PAYER account — the world in which this payer-account-only control
    /// plane is reachable at all (`tests/all/payer_account_gate.rs` owns the denial every
    /// other caller gets).
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = Some(::engine::emulator_account_id().to_string());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Send a JSON body to one of this service's routes and return the status and the
    /// parsed body. The method matters: the batch association routes are `PUT`
    /// (`smithy_query(query="operation",operation=
    /// "BatchAssociateResourcesToCustomLineItem")`).
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

    /// POST a JSON body to one of this service's routes.
    async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.send("POST", path, body).await
    }

    /// Create a billing group and backdate its stored instants to `created`, as if the
    /// creating call had been made in that billing period, so the periods before the
    /// delete are periods the group really was billing in.
    async fn group_created_in(&self, case: &str, name: &str, created: &str) -> String {
        let (status, body) = self
            .post(
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
            "case {case:?}: setup CreateBillingGroup expected HTTP 200, got {status} with {body}"
        );
        let arn = member_str(&body, "Arn", case);
        let at = period_start_millis(created);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .billing_groups
            .get_mut(&arn)
            .expect("setup group_created_in: the group was created above");
        record.creation_time_millis = at;
        record.last_modified_time_millis = at;
        arn
    }

    /// `CreateCustomLineItem` carrying `charge`, applying over the billing periods
    /// `applied` names (every period from the current one on when it names none).
    async fn item(
        &self,
        case: &str,
        group: &str,
        name: &str,
        charge: &Value,
        applied: Option<&Value>,
    ) -> String {
        let mut body = json!({
            "Name": name,
            "Description": "the item the delete is read through",
            "BillingGroupArn": group,
            "ChargeDetails": charge,
        });
        if let Some(applied) = applied {
            body["BillingPeriodRange"] = applied.clone();
        }
        let (status, answered) = self.post("/create-custom-line-item", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateCustomLineItem {name:?} expected HTTP 200, got {status} \
             with {answered}"
        );
        member_str(&answered, "Arn", case)
    }

    /// `BatchAssociateResourcesToCustomLineItem`: put `resource` in effect under the
    /// percentage item `target` from the billing period `from` on.
    async fn associate(&self, case: &str, target: &str, resource: &str, from: &str) {
        let (status, body) = self
            .send(
                "PUT",
                "/batch-associate-resources-to-custom-line-item",
                &json!({
                    "TargetArn": target,
                    "ResourceArns": [resource],
                    "BillingPeriodRange": range(from, None),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup BatchAssociateResourcesToCustomLineItem expected HTTP 200, got \
             {status} with {body}"
        );
        assert_eq!(
            body.get("FailedAssociatedResources"),
            Some(&json!([])),
            "case {case:?}: setup expected no failed resource, got {body}"
        );
    }

    /// `DeleteBillingGroup`, asserted to be served.
    async fn delete_group(&self, case: &str, group: &str) {
        let (status, body) = self
            .post("/delete-billing-group", &json!({ "Arn": group }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteBillingGroup expected HTTP 200, got {status} with {body}"
        );
    }

    /// `GetBillingGroupCostReport` over the single billing period `months` from the
    /// current one, as the half-open range that covers exactly it.
    async fn cost_report(&self, group: &str, months: i64) -> (u16, Value) {
        self.post(
            "/get-billing-group-cost-report",
            &json!({
                "Arn": group,
                "BillingPeriodRange": {
                    "InclusiveStartBillingPeriod": period(months),
                    "ExclusiveEndBillingPeriod": period(months + 1),
                },
            }),
        )
        .await
    }

    /// The `ProformaCost` `GetBillingGroupCostReport` answers for that ONE billing period.
    ///
    /// The report is also held to the rest of its money side: this emulator records no
    /// metered usage, so the margin over the period is the whole pro forma amount.
    async fn proforma_in(&self, case: &str, group: &str, months: i64) -> f64 {
        let (status, body) = self.cost_report(group, months).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetBillingGroupCostReport for {} expected HTTP 200, got {status} \
             with {body}",
            period(months)
        );
        let rows = body
            .get("BillingGroupCostReportResults")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            rows.len(),
            1,
            "case {case:?}: one group over one billing period is one row, got {rows:?}"
        );
        let proforma = amount(case, &rows[0], "ProformaCost");
        let margin = amount(case, &rows[0], "Margin");
        assert!(
            (margin - proforma).abs() < TOLERANCE,
            "case {case:?}: no metered usage is recorded, so the margin is the pro forma \
             amount {proforma}, got {margin} in {:?}",
            rows[0]
        );
        proforma
    }

    /// The `ProformaCost` `ListBillingGroupCostReports` reports for ONE group in the
    /// billing period `months` from the current one, or `None` when that period reports no
    /// such group.
    async fn listed_proforma(&self, case: &str, group: &str, months: i64) -> Option<f64> {
        let (status, body) = self
            .post(
                "/list-billing-group-cost-reports",
                &json!({
                    "BillingPeriod": period(months),
                    "Filters": { "BillingGroupArns": [group] },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListBillingGroupCostReports expected HTTP 200, got {status} with \
             {body}"
        );
        body.get("BillingGroupCostReports")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|element| element.get("Arn").and_then(Value::as_str) == Some(group))
            .map(|element| amount(case, &element, "ProformaCost"))
    }

    /// The ARNs `ListCustomLineItems` reports for one billing group in the billing period
    /// `months` from the current one.
    async fn items_in(&self, case: &str, group: &str, months: i64) -> Vec<String> {
        let (status, body) = self
            .post(
                "/list-custom-line-items",
                &json!({
                    "BillingPeriod": period(months),
                    "Filters": { "BillingGroups": [group] },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListCustomLineItems expected HTTP 200, got {status} with {body}"
        );
        body.get("CustomLineItems")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|element| element.get("Arn").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// The resource ARNs `ListResourcesAssociatedToCustomLineItem` reports for one
    /// percentage item in the billing period `months` from the current one.
    async fn associated_in(&self, case: &str, item: &str, months: i64) -> Vec<String> {
        let (status, body) = self
            .post(
                "/list-resources-associated-to-custom-line-item",
                &json!({ "Arn": item, "BillingPeriod": period(months) }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListResourcesAssociatedToCustomLineItem expected HTTP 200, got \
             {status} with {body}"
        );
        body.get("AssociatedResources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|element| element.get("Arn").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }
}

/// A string member a response is required to carry.
fn member_str(body: &Value, member: &str, case: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {body}"))
        .to_string()
}

/// One reported amount, read as the decimal its string carries.
fn amount(case: &str, element: &Value, member: &str) -> f64 {
    let text = element
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} member, got {element}"));
    text.parse::<f64>().unwrap_or_else(|_| {
        panic!("case {case:?}: expected {member} to be a decimal amount, got {text:?}")
    })
}

/// A FLAT charge of `value` that ADDS to the bill (`CustomLineItemType` has exactly `FEE`
/// and `CREDIT`; `smithy_query(query="shape",shape="CustomLineItemType")`).
fn fee_flat(value: f64) -> Value {
    json!({ "Type": "FEE", "Flat": { "ChargeValue": value } })
}

/// A FLAT charge of `value` that comes OFF the bill.
fn credit_flat(value: f64) -> Value {
    json!({ "Type": "CREDIT", "Flat": { "ChargeValue": value } })
}

/// A PERCENTAGE charge: `value` per cent "multiplied against the combined value of its
/// associated resources" (`smithy_query(query="shape",
/// shape="CustomLineItemPercentageChargeDetails")`).
fn fee_percentage(value: f64) -> Value {
    json!({ "Type": "FEE", "Percentage": { "PercentageValue": value } })
}

/// One row of the table: a billing group whose life began `created` months before the
/// current billing period, carrying one custom line item.
struct Row {
    case: &'static str,
    /// The name the group and its item carry, differing along the characters the modelled
    /// `BillingGroupName` / `CustomLineItemName` pattern `^[a-zA-Z0-9_\+=\.\-@]+$` allows
    /// (`smithy_query(query="shape",shape="CustomLineItemName")`).
    name: &'static str,
    /// How many months before the current billing period the group was created in.
    created: i64,
    /// The item's `ChargeDetails`.
    charge: fn() -> Value,
    /// The item's `BillingPeriodRange`, as months relative to the current billing period:
    /// `None` names no range at all, which puts the item in effect from the current period
    /// on.
    applied: Option<(i64, Option<i64>)>,
    /// The pro forma amount the period BEFORE the delete carries, and whether that period
    /// reports the item at all — a percentage item with nothing associated is reported
    /// there and prices at nothing.
    previous_proforma: f64,
    previous_reports_item: bool,
}

/// THE RULE: a billing-group delete ends the group's custom line items from the billing
/// period the delete is made in and never earlier, so every period-scoped read of an
/// earlier period answers exactly as it did before the delete, and no read from the
/// delete's own period on reports the item.
#[tokio::test]
async fn a_group_delete_keeps_the_charges_of_the_periods_before_it() {
    let rows: &[Row] = &[
        Row {
            case: "a flat fee applying from the previous billing period on, open ended",
            name: "Fee_From_Prev",
            created: -1,
            charge: || fee_flat(100.0),
            applied: Some((-1, None)),
            previous_proforma: 100.0,
            previous_reports_item: true,
        },
        Row {
            case: "a flat credit applying from the previous billing period on",
            name: "Credit.From-Prev",
            created: -2,
            charge: || credit_flat(25.5),
            applied: Some((-1, None)),
            previous_proforma: -25.5,
            previous_reports_item: true,
        },
        Row {
            case: "a flat fee bounded to the previous billing period alone",
            name: "Fee=Prev@Only",
            created: -3,
            charge: || fee_flat(7.0),
            applied: Some((-1, Some(0))),
            previous_proforma: 7.0,
            previous_reports_item: true,
        },
        Row {
            case: "a flat fee running from the previous billing period past the current one",
            name: "Fee+Spanning",
            created: -1,
            charge: || fee_flat(12.25),
            applied: Some((-1, Some(2))),
            previous_proforma: 12.25,
            previous_reports_item: true,
        },
        Row {
            case: "a percentage fee applying from the previous billing period on, nothing \
                   associated to it",
            name: "Pct_From_Prev",
            created: -2,
            charge: || fee_percentage(10.0),
            applied: Some((-1, None)),
            previous_proforma: 0.0,
            previous_reports_item: true,
        },
        Row {
            case: "a flat fee applying from the current billing period on",
            name: "Fee_From_Now",
            created: -1,
            charge: || fee_flat(40.0),
            applied: Some((0, None)),
            previous_proforma: 0.0,
            previous_reports_item: false,
        },
        Row {
            case: "a flat fee the create named no billing period range for",
            name: "Fee_No_Range",
            created: -2,
            charge: || credit_flat(55.0),
            applied: None,
            previous_proforma: 0.0,
            previous_reports_item: false,
        },
    ];

    let emulator = Emulator::new();
    for row in rows {
        let case = row.case;
        let group = emulator
            .group_created_in(case, row.name, &period(row.created))
            .await;
        let applied = row.applied.map(|(start, end)| {
            range(
                &period(start),
                end.map(period).as_deref(),
            )
        });
        let item = emulator
            .item(case, &group, row.name, &(row.charge)(), applied.as_ref())
            .await;

        // What the period before the delete answered while the group was still stored.
        let before = emulator.proforma_in(case, &group, -1).await;
        assert!(
            (before - row.previous_proforma).abs() < TOLERANCE,
            "case {case:?}: before the delete the previous billing period carried \
             {expected}, got {before}",
            expected = row.previous_proforma
        );
        let listed_before = emulator.items_in(case, &group, -1).await;
        assert_eq!(
            listed_before.contains(&item),
            row.previous_reports_item,
            "case {case:?}: before the delete, ListCustomLineItems for the previous billing \
             period reporting the item was expected to be {}, got {listed_before:?}",
            row.previous_reports_item
        );

        emulator.delete_group(case, &group).await;

        // The periods before the delete answer exactly as they did: the item was in
        // effect there and the group is still reported there, so the charge it carried
        // is still priced there.
        let after = emulator.proforma_in(case, &group, -1).await;
        assert!(
            (after - row.previous_proforma).abs() < TOLERANCE,
            "case {case:?}: the billing period before the delete must keep the charge it \
             carried, expected {expected}, got {after}",
            expected = row.previous_proforma
        );
        let listed_report = emulator.listed_proforma(case, &group, -1).await;
        assert!(
            listed_report.is_some_and(|cost| (cost - row.previous_proforma).abs() < TOLERANCE),
            "case {case:?}: ListBillingGroupCostReports for the previous billing period must \
             report the group with {expected}, got {listed_report:?}",
            expected = row.previous_proforma
        );
        let listed_after = emulator.items_in(case, &group, -1).await;
        assert_eq!(
            listed_after, listed_before,
            "case {case:?}: the previous billing period must report the items it did before \
             the delete, {listed_before:?}, got {listed_after:?}"
        );

        // A period of the group's life BEFORE the item was put in effect carries no
        // charge of it — the delete puts none there either.
        if row.created < -1 {
            let first = emulator.proforma_in(case, &group, row.created).await;
            assert!(
                first.abs() < TOLERANCE,
                "case {case:?}: the group's first billing period held no item, so it carries \
                 no charge, got {first}"
            );
        }

        // From the delete's own period on, nothing reports the group or its item.
        let current = emulator.items_in(case, &group, 0).await;
        assert!(
            !current.contains(&item),
            "case {case:?}: the period of the delete reports no item of the deleted group, \
             got {current:?}"
        );
        let (status, body) = emulator.cost_report(&group, 0).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the period of the delete held no such billing group, so its report \
             is refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {body}"
        );
    }
}

/// THE SAME RULE on the resource associations a percentage item holds: an association to
/// the deleted group, like one to another item of it, is in effect in the periods before
/// the delete and in none from it on — so the pro forma amount an earlier period carries
/// for the whole graph is the one it carried before the delete.
#[tokio::test]
async fn a_group_delete_keeps_the_association_graph_of_the_periods_before_it() {
    let case = "a percentage graph of a deleted group";
    let emulator = Emulator::new();
    let group = emulator
        .group_created_in(case, "Graph_Group", &period(-2))
        .await;
    let applied = range(&period(-1), None);
    let flat = emulator
        .item(case, &group, "Graph_Flat", &fee_flat(100.0), Some(&applied))
        .await;
    let share_of_item = emulator
        .item(
            case,
            &group,
            "Graph_Share_Of_Item",
            &fee_percentage(10.0),
            Some(&applied),
        )
        .await;
    let share_of_group = emulator
        .item(
            case,
            &group,
            "Graph_Share_Of_Group",
            &fee_percentage(50.0),
            Some(&applied),
        )
        .await;
    emulator
        .associate(case, &share_of_item, &flat, &period(-1))
        .await;
    emulator
        .associate(case, &share_of_group, &group, &period(-1))
        .await;

    // The flat fee itself, plus the tenth of it the one percentage item charges; the item
    // that is a share of the GROUP charges nothing, because the metered usage of the
    // accounts a billing group bills is usage this emulator records none of.
    let expected = 110.0;
    let before = emulator.proforma_in(case, &group, -1).await;
    assert!(
        (before - expected).abs() < TOLERANCE,
        "case {case:?}: before the delete the previous billing period carried {expected}, got \
         {before}"
    );

    emulator.delete_group(case, &group).await;

    let after = emulator.proforma_in(case, &group, -1).await;
    assert!(
        (after - expected).abs() < TOLERANCE,
        "case {case:?}: the billing period before the delete must keep the charge of the whole \
         graph, expected {expected}, got {after}"
    );
    let items = emulator.items_in(case, &group, -1).await;
    for (label, arn) in [
        ("the flat fee", &flat),
        ("the share of another item", &share_of_item),
        ("the share of the group", &share_of_group),
    ] {
        assert!(
            items.contains(arn),
            "case {case:?}: the previous billing period must still report {label}, got {items:?}"
        );
    }
    let to_item = emulator.associated_in(case, &share_of_item, -1).await;
    assert_eq!(
        to_item,
        vec![flat.clone()],
        "case {case:?}: the previous billing period must still report the item the percentage \
         is taken of, got {to_item:?}"
    );
    let to_group = emulator.associated_in(case, &share_of_group, -1).await;
    assert_eq!(
        to_group,
        vec![group.clone()],
        "case {case:?}: the previous billing period must still report the association to the \
         deleted group, got {to_group:?}"
    );

    // From the delete's own period on neither the items nor their associations are in
    // effect.
    let current_items = emulator.items_in(case, &group, 0).await;
    assert!(
        current_items.is_empty(),
        "case {case:?}: the period of the delete reports no item of the deleted group, got \
         {current_items:?}"
    );
    for (label, item) in [
        ("the share of another item", &share_of_item),
        ("the share of the group", &share_of_group),
    ] {
        let associated = emulator.associated_in(case, item, 0).await;
        assert!(
            associated.is_empty(),
            "case {case:?}: the period of the delete reports no association of {label}, got \
             {associated:?}"
        );
    }
}
