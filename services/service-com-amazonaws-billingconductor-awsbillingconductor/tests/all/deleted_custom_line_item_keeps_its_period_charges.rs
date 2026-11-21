// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteCustomLineItem` that names NO `BillingPeriodRange` leaves the billing
//! periods BEFORE it holding, end to end over the service's real `restJson1` wire through
//! [`MultiServiceRouter::dispatch`](smithy_server_runtime::MultiServiceRouter::dispatch):
//! `CreateBillingGroup` -> `CreateCustomLineItem` ->
//! `BatchAssociateResourcesToCustomLineItem` -> `DeleteCustomLineItem` ->
//! `GetBillingGroupCostReport` / `ListBillingGroupCostReports` / `ListCustomLineItems` /
//! `ListCustomLineItemVersions` / `ListResourcesAssociatedToCustomLineItem`.
//!
//! RULE: a delete that names no billing-period range ends every billing-period-scoped
//! record it takes with it FROM the billing period the call is made in and never earlier,
//! so every read scoped to a period before the call reports and prices the record exactly
//! as it did before the call, while no read from that period on reports it — and a record
//! in effect in no earlier period is left in effect in none at all, so nothing of it is
//! reported anywhere. `tests/all/deleted_billing_group_keeps_its_period_charges.rs` holds
//! the same rule for the delete of the billing group those items hang off — and is the
//! harness this suite drives rather than standing up a second one.
//!
//! The rows differ along what the rule generalises over: the charge the item carries (a
//! fee, a credit, a percentage of another item, a percentage with nothing associated to
//! it), the billing period it was put in effect from (the previous one, the current one,
//! none named at all), how its range is bounded (open-ended, ending at the current
//! period, ending past it), whether it was still in effect in the period of the delete at
//! all, and which of the five period-scoped reads asks.
//!
//! The wire can only create a billing GROUP now, so a row creates it through the real
//! wire and then backdates the group's own instants, the one thing these reads cannot ask
//! the service for; an ITEM needs no such help, since a create may put one in effect from
//! the previous billing period on.

use crate::bounded_delete_narrows_resource_associations::range;
use crate::deleted_billing_group_keeps_its_period_charges::{
    credit_flat, fee_flat, fee_percentage, Emulator, TOLERANCE,
};
use crate::percentage_custom_line_item_resource_associations::period;
use serde_json::{json, Value};

/// `DeleteCustomLineItem` carrying NO `BillingPeriodRange`, asserted to be served and to
/// answer the item's own ARN.
async fn delete_item(emulator: &Emulator, case: &str, item: &str) {
    let (status, body) = emulator
        .post("/delete-custom-line-item", &json!({ "Arn": item }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteCustomLineItem expected HTTP 200, got {status} with {body}"
    );
    assert_eq!(
        body.get("Arn").and_then(Value::as_str),
        Some(item),
        "case {case:?}: expected the deleted item's ARN, got {body}"
    );
}

/// How many generations `ListCustomLineItemVersions` reports for the item in the billing
/// period `months` from the current one, or `None` when the store holds no such item at
/// all — the read's modelled refusal, which is how an item nothing is left of answers.
async fn versions_in(emulator: &Emulator, case: &str, item: &str, months: i64) -> Option<usize> {
    let (status, body) = emulator
        .post(
            "/list-custom-line-item-versions",
            &json!({
                "Arn": item,
                "Filters": { "BillingPeriodRange": {
                    "StartBillingPeriod": period(months),
                    "EndBillingPeriod": period(months + 1),
                } },
            }),
        )
        .await;
    if status == 400 {
        // The ONLY refusal this read may answer here is "no such custom line item": the
        // `Arn` is one a create answered and the filter range is a well-ordered
        // half-open span, so any other reason would be a different defect rather than
        // the record being gone.
        assert_eq!(
            body.get("Reason").and_then(Value::as_str),
            Some("MISSING_CUSTOMLINEITEM"),
            "case {case:?}: expected the refusal for an item nothing is left of, got {body}"
        );
        return None;
    }
    assert_eq!(
        status, 200,
        "case {case:?}: ListCustomLineItemVersions for {} expected HTTP 200, got {status} with \
         {body}",
        period(months)
    );
    Some(
        body.get("CustomLineItemVersions")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
    )
}

/// One row: a billing group whose life began `created` months before the current billing
/// period, carrying one custom line item the delete then names with no range.
struct Row {
    case: &'static str,
    /// The name the group and its item carry, differing along the characters the modelled
    /// `BillingGroupName` / `CustomLineItemName` pattern `^[a-zA-Z0-9_\+=\.\-@]+$` allows
    /// (`smithy_query(query="shape",shape="CustomLineItemName")`).
    name: &'static str,
    /// How many months before the current billing period the group was created in.
    created: i64,
    /// The item's `ChargeDetails`.
    charge: Value,
    /// The item's `BillingPeriodRange`, as months relative to the current billing period:
    /// `None` names no range at all, which puts the item in effect from the current period
    /// on.
    applied: Option<(i64, Option<i64>)>,
    /// The pro forma amount the PREVIOUS billing period carries and whether it reports the
    /// item there — before the delete and, by the rule, after it too.
    previous: (f64, bool),
    /// The pro forma amount the CURRENT billing period carried, and whether it reported the
    /// item, BEFORE the delete; after it, both are what no record in effect reads as.
    current_before: (f64, bool),
}

/// THE RULE: a delete naming no billing period range ends the item from the period the
/// call is made in, so the closed bills before it report and price it exactly as they did
/// and the period of the call — and every later one — reports and prices nothing of it.
#[tokio::test]
async fn an_unbounded_delete_keeps_the_charges_of_the_billing_periods_before_it() {
    let rows: &[Row] = &[
        Row {
            case: "a flat fee applying from the previous billing period on, open ended",
            name: "Item_Fee_From_Prev",
            created: -1,
            charge: fee_flat(100.0),
            applied: Some((-1, None)),
            previous: (100.0, true),
            current_before: (100.0, true),
        },
        Row {
            case: "a flat credit applying from the previous billing period on",
            name: "Item.Credit-From-Prev",
            created: -2,
            charge: credit_flat(25.5),
            applied: Some((-1, None)),
            previous: (-25.5, true),
            current_before: (-25.5, true),
        },
        Row {
            case: "a flat fee bounded to the previous billing period alone",
            name: "Item=Prev@Only",
            created: -3,
            charge: fee_flat(7.0),
            applied: Some((-1, Some(0))),
            previous: (7.0, true),
            current_before: (0.0, false),
        },
        Row {
            case: "a flat fee running from the previous billing period past the current one",
            name: "Item+Spanning",
            created: -1,
            charge: fee_flat(12.25),
            applied: Some((-1, Some(2))),
            previous: (12.25, true),
            current_before: (12.25, true),
        },
        Row {
            case: "a percentage fee from the previous billing period on, nothing associated \
                   to it",
            name: "Item_Pct_From_Prev",
            created: -2,
            charge: fee_percentage(10.0),
            applied: Some((-1, None)),
            previous: (0.0, true),
            current_before: (0.0, true),
        },
        Row {
            case: "a flat fee applying from the current billing period on",
            name: "Item_Fee_From_Now",
            created: -1,
            charge: fee_flat(40.0),
            applied: Some((0, None)),
            previous: (0.0, false),
            current_before: (40.0, true),
        },
        Row {
            case: "a flat credit the create named no billing period range for",
            name: "Item_No_Range",
            created: -2,
            charge: credit_flat(55.0),
            applied: None,
            previous: (0.0, false),
            current_before: (-55.0, true),
        },
    ];

    let emulator = Emulator::new();
    for row in rows {
        let case = row.case;
        let group = emulator
            .group_created_in(case, row.name, &period(row.created))
            .await;
        let applied = row
            .applied
            .map(|(start, end)| range(&period(start), end.map(period).as_deref()));
        let item = emulator
            .item(case, &group, row.name, &row.charge, applied.as_ref())
            .await;

        // What the two periods answered while the item was still in effect.
        let (previous_cost, previous_reports) = row.previous;
        let before = emulator.proforma_in(case, &group, -1).await;
        assert!(
            (before - previous_cost).abs() < TOLERANCE,
            "case {case:?}: before the delete the previous billing period carried \
             {previous_cost}, got {before}"
        );
        let listed_before = emulator.items_in(case, &group, -1).await;
        assert_eq!(
            listed_before.contains(&item),
            previous_reports,
            "case {case:?}: before the delete, ListCustomLineItems for the previous billing \
             period reporting the item was expected to be {previous_reports}, got \
             {listed_before:?}"
        );
        let (current_cost, current_reports) = row.current_before;
        let current = emulator.proforma_in(case, &group, 0).await;
        assert!(
            (current - current_cost).abs() < TOLERANCE,
            "case {case:?}: before the delete the current billing period carried \
             {current_cost}, got {current}"
        );
        let current_listed = emulator.items_in(case, &group, 0).await;
        assert_eq!(
            current_listed.contains(&item),
            current_reports,
            "case {case:?}: before the delete, the current billing period reporting the item \
             was expected to be {current_reports}, got {current_listed:?}"
        );

        delete_item(&emulator, case, &item).await;

        // The closed bill before the delete answers exactly as it did: the item was in
        // effect there, so the charge it carried is still priced there, by both reports.
        let after = emulator.proforma_in(case, &group, -1).await;
        assert!(
            (after - previous_cost).abs() < TOLERANCE,
            "case {case:?}: the billing period before the delete must keep the charge it \
             carried, expected {previous_cost}, got {after}"
        );
        let listed_report = emulator.listed_proforma(case, &group, -1).await;
        assert!(
            listed_report.is_some_and(|cost| (cost - previous_cost).abs() < TOLERANCE),
            "case {case:?}: ListBillingGroupCostReports for the previous billing period must \
             report the group with {previous_cost}, got {listed_report:?}"
        );
        let listed_after = emulator.items_in(case, &group, -1).await;
        assert_eq!(
            listed_after, listed_before,
            "case {case:?}: the previous billing period must report the items it did before \
             the delete, {listed_before:?}, got {listed_after:?}"
        );
        // The version history of that closed bill reports the one generation it had — and
        // an item in effect in no period before the delete is left in effect in none at
        // all, so nothing of it is stored and this read has no item to answer for.
        let versions_previous = versions_in(&emulator, case, &item, -1).await;
        let expected_versions = if previous_reports { Some(1) } else { None };
        assert_eq!(
            versions_previous, expected_versions,
            "case {case:?}: the version history of the previous billing period was expected to \
             be {expected_versions:?}, got {versions_previous:?}"
        );

        // From the delete's own period on, nothing reports or prices the item.
        let current_after = emulator.proforma_in(case, &group, 0).await;
        assert!(
            current_after.abs() < TOLERANCE,
            "case {case:?}: the period of the delete carries no charge of the item it ended, \
             got {current_after}"
        );
        for months in [0, 1, 6] {
            let listed = emulator.items_in(case, &group, months).await;
            assert!(
                !listed.contains(&item),
                "case {case:?}: {:?} is at or after the delete, so it must report no item, got \
                 {listed:?}",
                period(months)
            );
            let versions = versions_in(&emulator, case, &item, months).await;
            assert!(
                versions.is_none_or(|count| count == 0),
                "case {case:?}: {:?} is at or after the delete, so it reports no generation of \
                 the item, got {versions:?}",
                period(months)
            );
        }

        // An item that was in effect in no earlier period is left in effect in none at
        // all, so nothing of it is stored and no read answers for it; an item an earlier
        // period still reports is still stored for that period.
        let stored = versions_previous.is_some();
        assert_eq!(
            stored, previous_reports,
            "case {case:?}: the record outliving the delete was expected to be \
             {previous_reports}, got {stored}"
        );
    }
}

/// THE SAME RULE on the association graph a percentage item charges through: the delete of
/// the item a percentage is taken OF ends the association to it at the period of the call,
/// so the closed bill before it prices the whole graph as it did, and the period of the
/// call prices neither the deleted item nor the share of it.
#[tokio::test]
async fn an_unbounded_delete_keeps_the_association_graph_of_the_periods_before_it() {
    let case = "a percentage taken of an item deleted with no range";
    let emulator = Emulator::new();
    let group = emulator
        .group_created_in(case, "Item_Graph_Group", &period(-2))
        .await;
    let applied = range(&period(-1), None);
    let flat = emulator
        .item(
            case,
            &group,
            "Item_Graph_Flat",
            &fee_flat(100.0),
            Some(&applied),
        )
        .await;
    let share = emulator
        .item(
            case,
            &group,
            "Item_Graph_Share",
            &fee_percentage(10.0),
            Some(&applied),
        )
        .await;
    emulator.associate(case, &share, &flat, &period(-1)).await;

    // The flat fee itself, plus the tenth of it the percentage item charges.
    let expected = 110.0;
    for months in [-1, 0] {
        let before = emulator.proforma_in(case, &group, months).await;
        assert!(
            (before - expected).abs() < TOLERANCE,
            "case {case:?}: before the delete {:?} carried {expected}, got {before}",
            period(months)
        );
    }

    delete_item(&emulator, case, &flat).await;

    let after = emulator.proforma_in(case, &group, -1).await;
    assert!(
        (after - expected).abs() < TOLERANCE,
        "case {case:?}: the billing period before the delete must keep the charge of the whole \
         graph, expected {expected}, got {after}"
    );
    let items = emulator.items_in(case, &group, -1).await;
    for (label, arn) in [("the deleted item", &flat), ("the share of it", &share)] {
        assert!(
            items.contains(arn),
            "case {case:?}: the previous billing period must still report {label}, got {items:?}"
        );
    }
    let associated = emulator.associated_in(case, &share, -1).await;
    assert_eq!(
        associated,
        vec![flat.clone()],
        "case {case:?}: the previous billing period must still report the item the percentage \
         is taken of, got {associated:?}"
    );

    // From the delete's own period on the item is gone, so the share of it charges nothing
    // and holds no association: the only item that period reports is the share itself.
    let current = emulator.proforma_in(case, &group, 0).await;
    assert!(
        current.abs() < TOLERANCE,
        "case {case:?}: the period of the delete prices neither the ended item nor the share \
         of it, got {current}"
    );
    let current_items = emulator.items_in(case, &group, 0).await;
    assert_eq!(
        current_items,
        vec![share.clone()],
        "case {case:?}: the period of the delete reports only the item the delete did not \
         name, got {current_items:?}"
    );
    let current_associated = emulator.associated_in(case, &share, 0).await;
    assert!(
        current_associated.is_empty(),
        "case {case:?}: the period of the delete reports no association to the ended item, got \
         {current_associated:?}"
    );
}
