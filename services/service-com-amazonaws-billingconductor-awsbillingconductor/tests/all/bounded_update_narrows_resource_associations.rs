// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateCustomLineItem` BOUNDED to a billing period range does to the resource
//! associations keyed on the item, end to end over the wire: `CreateBillingGroup` ->
//! `CreateCustomLineItem` -> `BatchAssociateResourcesToCustomLineItem` ->
//! `UpdateCustomLineItem` -> `ListCustomLineItems` /
//! `ListResourcesAssociatedToCustomLineItem`.
//!
//! RULE: a resource association is in effect only in billing periods the resource it
//! names applies in, so a write that takes billing periods away from a custom line item
//! takes the same periods out of every association keyed on that item — the ones the item
//! itself holds and the ones other percentage items hold TO it — and drops an association
//! left in effect in no period at all. So no read of a billing period reports an
//! association to an item no version of which that period reports, whichever write took
//! the periods: a bounded update, whose range states the periods the item applies in from
//! then on and so ends it from that range's exclusive end, takes them exactly as the
//! bounded delete does (`tests/all/bounded_delete_narrows_resource_associations.rs`).
//!
//! The rows span what the rule generalises over rather than one recorded call: where the
//! update's range ends relative to the periods the association is in effect over
//! (inside them, at their start, past their end, before they begin), how the range is
//! bounded (open-ended / bounded at both ends) and where it starts (the current / the
//! previous billing period), how the item's own range is bounded (open-ended / bounded),
//! how the association's range is bounded (none at all / open-ended / bounded / starting
//! later than the item), and which other members the update carries. Each row is read
//! from BOTH sides of the rule: the association the updated item holds and the one
//! another item holds to it.

use crate::bounded_delete_narrows_resource_associations::{
    associated_arns, association_size, item_in, range,
};
use crate::percentage_custom_line_item_resource_associations::{period, Emulator};
use serde_json::{json, Value};

/// A PERCENTAGE custom line item — the only kind resources can be associated to —
/// applying over the billing periods `billing_period_range` names.
async fn percentage_item_over(
    emulator: &Emulator,
    case: &str,
    group: &str,
    name: &str,
    billing_period_range: &Value,
) -> String {
    let (status, answered) = emulator
        .send(
            "POST",
            "/create-custom-line-item",
            &json!({
                "Name": name,
                "Description": "an item whose associations are narrowed",
                "BillingGroupArn": group,
                "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
                "BillingPeriodRange": billing_period_range,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create of {name:?} to answer HTTP 200, got {status} with \
         {answered}"
    );
    answered
        .get("Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an Arn for {name:?}, got {answered}"))
        .to_string()
}

/// `BatchAssociateResourcesToCustomLineItem` of ONE resource to `holder`, over the
/// billing period range the row names, asserted to succeed for that resource.
async fn associate_ok(
    emulator: &Emulator,
    case: &str,
    holder: &str,
    resource: &str,
    billing_period_range: Option<Value>,
) {
    let resources = vec![resource.to_string()];
    let (status, answered) = emulator
        .associate(holder, &resources, billing_period_range)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association of {resource} to answer HTTP 200, got {status} \
         with {answered}"
    );
    assert_eq!(
        answered.get("FailedAssociatedResources"),
        Some(&json!([])),
        "case {case:?}: expected no failed resource associating {resource}, got {answered}"
    );
}

/// `UpdateCustomLineItem` on `arn` with the `BillingPeriodRange` the row names, if any,
/// and the members it carries beside it.
async fn update(
    emulator: &Emulator,
    arn: &str,
    billing_period_range: &Value,
    members: &Value,
) -> (u16, Value) {
    let mut body = json!({ "Arn": arn });
    if !billing_period_range.is_null() {
        body["BillingPeriodRange"] = billing_period_range.clone();
    }
    for (member, value) in members.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    emulator
        .send("POST", "/update-custom-line-item", &body)
        .await
}

/// One row: how the updated item and the associations keyed on it were set up, the range
/// the update names, and what every probed billing period must report afterwards.
struct Row {
    case: &'static str,
    /// The `BillingPeriodRange` the item the update narrows is created with.
    item_range: Value,
    /// The `BillingPeriodRange` the two associating calls name — `None` for the call that
    /// names none, which is in effect from the period it lands in on.
    association_range: Option<Value>,
    /// The `BillingPeriodRange` the update names.
    updated: Value,
    /// The members the update carries beside its range.
    members: Value,
    /// (the billing period a read asks about, does a read report the updated item there,
    /// is an association keyed on it in effect there)
    probes: Vec<(String, bool, bool)>,
}

/// THE RULE: the billing periods a bounded update takes from a custom line item come out
/// of every association keyed on it, so a period that reports no version of the item
/// reports no association to it either — while the periods the update leaves the item
/// applying in keep exactly the associations they had.
#[tokio::test]
async fn a_bounded_update_takes_the_periods_it_ends_the_item_in_out_of_every_association() {
    let rows: Vec<Row> = vec![
        Row {
            case: "a bounded update of an open-ended item, ending inside the association",
            item_range: range(&period(-1), None),
            association_range: Some(range(&period(-1), None)),
            updated: range(&period(0), Some(&period(1))),
            members: json!({ "Description": "narrowed" }),
            probes: vec![
                (period(-1), true, true),
                (period(0), true, true),
                (period(1), false, false),
                (period(6), false, false),
            ],
        },
        Row {
            case: "a bounded update ending two periods on",
            item_range: range(&period(-1), None),
            association_range: Some(range(&period(-1), None)),
            updated: range(&period(0), Some(&period(2))),
            members: json!({ "Name": "renamed" }),
            probes: vec![
                (period(-1), true, true),
                (period(0), true, true),
                (period(1), true, true),
                (period(2), false, false),
                (period(9), false, false),
            ],
        },
        Row {
            case: "a bounded update applied from the previous billing period",
            item_range: range(&period(-1), None),
            association_range: Some(range(&period(-1), None)),
            updated: range(&period(-1), Some(&period(0))),
            members: json!({ "Description": "narrowed to the closed month" }),
            probes: vec![
                (period(-1), true, true),
                (period(0), false, false),
                (period(3), false, false),
            ],
        },
        Row {
            case: "an update ending before the association begins",
            item_range: range(&period(-1), None),
            association_range: Some(range(&period(2), None)),
            updated: range(&period(0), Some(&period(1))),
            members: json!({ "Description": "narrowed" }),
            probes: vec![
                // The item applies here and the association begins later, so the period
                // reports the item and no association — before the update as after it.
                (period(-1), true, false),
                (period(0), true, false),
                (period(1), false, false),
                (period(2), false, false),
                (period(5), false, false),
            ],
        },
        Row {
            case: "an update bounded inside an item and an association both bounded already",
            item_range: range(&period(-1), Some(&period(6))),
            association_range: Some(range(&period(-1), Some(&period(4)))),
            updated: range(&period(0), Some(&period(2))),
            members: json!({
                "ChargeDetails": { "Percentage": { "PercentageValue": 25.0 } },
            }),
            probes: vec![
                (period(-1), true, true),
                (period(0), true, true),
                (period(1), true, true),
                (period(2), false, false),
                (period(4), false, false),
                (period(6), false, false),
            ],
        },
        Row {
            case: "an update bounded exactly where the association starts",
            item_range: range(&period(-1), None),
            association_range: None,
            updated: range(&period(-1), Some(&period(0))),
            members: json!({ "Description": "narrowed to the period before the association" }),
            probes: vec![
                // The association named no range, so it is in effect from the current
                // period on — which this update is what the item no longer applies in.
                (period(-1), true, false),
                (period(0), false, false),
                (period(4), false, false),
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        // The item the update narrows is a PERCENTAGE item, so it holds an association of
        // its own as well as being the resource another item's association names: both
        // sides of the rule are read off the one update.
        let narrowed =
            percentage_item_over(&emulator, case, &group, "narrowed", &row.item_range).await;
        let open = range(&period(-1), None);
        let held = percentage_item_over(&emulator, case, &group, "held", &open).await;
        let holder = percentage_item_over(&emulator, case, &group, "holder", &open).await;
        associate_ok(
            &emulator,
            case,
            &narrowed,
            &held,
            row.association_range.clone(),
        )
        .await;
        associate_ok(
            &emulator,
            case,
            &holder,
            &narrowed,
            row.association_range.clone(),
        )
        .await;
        let charge_before = item_in(&emulator, case, &holder, &period(0))
            .await
            .and_then(|element| element.get("ChargeDetails").cloned());

        let (status, answered) = update(&emulator, &narrowed, &row.updated, &row.members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer HTTP 200, got {status} with {answered}"
        );
        assert_eq!(
            answered.get("Arn").and_then(Value::as_str),
            Some(narrowed.as_str()),
            "case {case:?}: expected the update to answer the item's ARN, got {answered}"
        );

        for (billing_period, item_reported, associated) in &row.probes {
            let reported = item_in(&emulator, case, &narrowed, billing_period)
                .await
                .is_some();
            assert_eq!(
                reported, *item_reported,
                "case {case:?}: expected the read for {billing_period:?} to report the narrowed \
                 item: {item_reported}, got {reported}"
            );

            // The association ANOTHER percentage item holds to the narrowed item.
            let listed = associated_arns(&emulator, case, &holder, billing_period)
                .await
                .unwrap_or_default();
            let holds = listed.contains(&narrowed);
            assert_eq!(
                holds, *associated,
                "case {case:?}: expected the holder's association to the narrowed item to be in \
                 effect in {billing_period:?}: {associated}, got {holds} from {listed:?}"
            );
            let size = association_size(&emulator, case, &holder, billing_period).await;
            assert_eq!(
                size,
                Some(i64::from(*associated)),
                "case {case:?}: expected the holder's AssociationSize in {billing_period:?} to \
                 count its one association only where it is in effect, got {size:?}"
            );

            // The association the narrowed item HOLDS itself, over the same range: an item
            // a period reports no version of is not there to hold a resource in it.
            let held_listed = associated_arns(&emulator, case, &narrowed, billing_period).await;
            let holds_held = held_listed.clone().unwrap_or_default().contains(&held);
            assert_eq!(
                holds_held, *associated,
                "case {case:?}: expected the narrowed item's own association to be in effect in \
                 {billing_period:?}: {associated}, got {holds_held} from {held_listed:?}"
            );

            // THE RULE, stated as the invariant the two reads have to agree on.
            assert!(
                *item_reported || !holds && !holds_held,
                "case {case:?}: {billing_period:?} reports no version of the narrowed item, so no \
                 read of that period may report an association keyed on it, got {listed:?} and \
                 {held_listed:?}"
            );

            // An item the update did not name keeps every billing period it applied in.
            assert!(
                item_in(&emulator, case, &held, billing_period)
                    .await
                    .is_some(),
                "case {case:?}: expected the item the update did not name to stay readable in \
                 {billing_period:?}"
            );
        }

        // Nothing else about the item that merely held an association to the narrowed one
        // changed.
        let charge_after = item_in(&emulator, case, &holder, &period(0))
            .await
            .and_then(|element| element.get("ChargeDetails").cloned());
        assert_eq!(
            charge_after, charge_before,
            "case {case:?}: expected the holder's charge details to be unchanged, got \
             {charge_after:?}"
        );
    }
}

/// THE RULE from the other side: an update that takes NO billing period away from the item
/// leaves every association keyed on it in effect in exactly the periods it was — whether
/// it names no range at all, an open-ended one, or one restating the periods the item
/// already applies in.
#[tokio::test]
async fn an_update_that_takes_no_billing_period_away_leaves_every_association_in_effect() {
    // (case, the range the item is created with, the `BillingPeriodRange` the update
    // names) — `Value::Null` for the request that names none, which is applied to the
    // periods the item already applies in.
    let updates: [(&str, Value, Value); 4] = [
        (
            "no range at all",
            range(&period(-1), None),
            Value::Null,
        ),
        (
            "an open-ended range from the current billing period",
            range(&period(-1), None),
            range(&period(0), None),
        ),
        (
            "an open-ended range from the previous billing period",
            range(&period(-1), None),
            range(&period(-1), None),
        ),
        (
            "a range restating the periods the item already applies in",
            range(&period(-1), Some(&period(7))),
            range(&period(-1), Some(&period(7))),
        ),
    ];

    for (case, item_range, updated) in updates {
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let open = range(&period(-1), None);
        let narrowed = percentage_item_over(&emulator, case, &group, "narrowed", &item_range).await;
        let held = percentage_item_over(&emulator, case, &group, "held", &open).await;
        let holder = percentage_item_over(&emulator, case, &group, "holder", &open).await;
        associate_ok(&emulator, case, &narrowed, &held, Some(open.clone())).await;
        associate_ok(&emulator, case, &holder, &narrowed, Some(open.clone())).await;

        let (status, answered) = update(
            &emulator,
            &narrowed,
            &updated,
            &json!({ "Description": "the same periods, a new description" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer HTTP 200, got {status} with {answered}"
        );

        for billing_period in [period(-1), period(0), period(1), period(6)] {
            // Every probed period the item still applies in reports both associations; a
            // period the item never applied in reports neither, and no version of it.
            let reported = item_in(&emulator, case, &narrowed, &billing_period)
                .await
                .is_some();
            let listed = associated_arns(&emulator, case, &holder, &billing_period)
                .await
                .unwrap_or_default();
            assert_eq!(
                listed.contains(&narrowed),
                reported,
                "case {case:?}: expected the holder's association to the item to be in effect in \
                 {billing_period:?} exactly where the item applies ({reported}), got {listed:?}"
            );
            let held_listed = associated_arns(&emulator, case, &narrowed, &billing_period)
                .await
                .unwrap_or_default();
            assert_eq!(
                held_listed.contains(&held),
                reported,
                "case {case:?}: expected the item's own association to be in effect in \
                 {billing_period:?} exactly where the item applies ({reported}), got \
                 {held_listed:?}"
            );
            let size = association_size(&emulator, case, &holder, &billing_period).await;
            assert_eq!(
                size,
                reported.then_some(1),
                "case {case:?}: expected the holder's AssociationSize in {billing_period:?} to \
                 count its one association where the item applies, got {size:?}"
            );
        }
    }
}

/// THE RULE on a REFUSED bounded update: it writes nothing, so the associations keyed on
/// the item are in effect in exactly the periods they were.
#[tokio::test]
async fn a_refused_bounded_update_leaves_every_association_keyed_on_the_item_in_effect() {
    // (case, the `BillingPeriodRange` the refused update names) — a start outside the
    // current and previous billing periods, and a range covering no period at all.
    let refusals: [(&str, Value); 4] = [
        ("a start two periods back", range(&period(-2), None)),
        ("a start a year back", range(&period(-12), None)),
        ("a start in a later period", range(&period(3), None)),
        (
            "a range whose end is its own start",
            range(&period(0), Some(&period(0))),
        ),
    ];

    for (case, updated) in refusals {
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let open = range(&period(-1), None);
        let narrowed = percentage_item_over(&emulator, case, &group, "narrowed", &open).await;
        let held = percentage_item_over(&emulator, case, &group, "held", &open).await;
        let holder = percentage_item_over(&emulator, case, &group, "holder", &open).await;
        associate_ok(&emulator, case, &narrowed, &held, Some(open.clone())).await;
        associate_ok(&emulator, case, &holder, &narrowed, Some(open.clone())).await;

        let (status, body) = update(
            &emulator,
            &narrowed,
            &updated,
            &json!({ "Description": "a refused narrowing" }),
        )
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

        for billing_period in [period(-1), period(0), period(5)] {
            let listed = associated_arns(&emulator, case, &holder, &billing_period)
                .await
                .unwrap_or_default();
            assert_eq!(
                listed,
                vec![narrowed.clone()],
                "case {case:?}: expected the holder's association to survive the refusal in \
                 {billing_period:?}, got {listed:?}"
            );
            let held_listed = associated_arns(&emulator, case, &narrowed, &billing_period)
                .await
                .unwrap_or_default();
            assert_eq!(
                held_listed,
                vec![held.clone()],
                "case {case:?}: expected the item's own association to survive the refusal in \
                 {billing_period:?}, got {held_listed:?}"
            );
            assert!(
                item_in(&emulator, case, &narrowed, &billing_period)
                    .await
                    .is_some(),
                "case {case:?}: expected the item to stay readable in {billing_period:?} after \
                 the refusal"
            );
        }
    }
}
