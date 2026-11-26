// SPDX-License-Identifier: CC0-1.0

//! Which billing period decides whether the custom line item a resource-association read
//! ADDRESSES exists, end to end over the wire: `CreateBillingGroup` ->
//! `CreateCustomLineItem` -> `BatchAssociateResourcesToCustomLineItem` ->
//! `DeleteCustomLineItem` -> `ListResourcesAssociatedToCustomLineItem` /
//! `ListCustomLineItems`.
//!
//! RULE: a read that addresses ONE resource by ARN and is answered FOR one billing period
//! resolves whether that resource exists IN the period being read, so a period no
//! generation of it applies in is refused with the operation's modelled
//! `ResourceNotFoundException` (HTTP 404, carrying the resource addressed and its type)
//! instead of being served a page — such a read cannot disagree with the period-scoped
//! LISTING of the same resource, which leaves it out of exactly those periods. It is the
//! answer the two sibling cross-reference listings of this service already give for the
//! pricing rule and the pricing plan they reference
//! (`tests/all/pricing_plans_associated_with_pricing_rule.rs`,
//! `tests/all/pricing_rules_associated_to_pricing_plan.rs`).
//!
//! The rows span what the rule generalises over rather than one reported call: where the
//! period asked about falls relative to the stretches the item applies in (a month long
//! before this account held any item, a year before, the period before its first, its
//! first period, the current one, a later one, the exclusive end of a bounded item and
//! past it, a period a bounded delete took it out of), how the item's own periods are
//! bounded (from the period of the create on, from the previous one on, bounded at both
//! ends, narrowed by a bounded delete), how the request spells the item (full ARN / bare
//! id) and the month (`2026-7` / `2026-07`), whether the request names a month at all, and
//! what page it asks for — a refusal about the resource precedes the page.
//!
//! Every assertion goes over the service's real `restJson1` wire through
//! [`smithy_server_runtime::MultiServiceRouter::dispatch`], on the harness of
//! `tests/all/percentage_custom_line_item_resource_associations.rs` — this service's
//! control plane answers the payer account alone.

use crate::bounded_delete_narrows_resource_associations::item_in;
use crate::percentage_custom_line_item_resource_associations::{
    one_digit_month, period, Emulator,
};
use serde_json::{json, Value};

/// The route the model binds `ListResourcesAssociatedToCustomLineItem` to
/// (`smithy_query(query="operation",operation="ListResourcesAssociatedToCustomLineItem")`).
const READ_PATH: &str = "/list-resources-associated-to-custom-line-item";

/// The `ResourceType` the modelled 404 names for a custom line item, as every refusal of
/// this service spells the resource classes of its ARNs.
const ITEM_RESOURCE_TYPE: &str = "customlineitem";

/// The modelled error shape a refusal carries, as `restJson1` puts it on the wire.
fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or_default()
}

/// The bare resource id of a full ARN — the other spelling the modelled
/// `CustomLineItemArn` pattern admits
/// (`^(arn:aws(-cn)?:billingconductor::[0-9]{12}:customlineitem/)?[a-zA-Z0-9]{10}$`).
fn bare_id(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

/// A PERCENTAGE custom line item — the only kind resources can be associated to —
/// applying over the billing periods `billing_period_range` names, or from the period of
/// the create on when it names none.
async fn percentage_item_over(
    emulator: &Emulator,
    case: &str,
    group: &str,
    name: &str,
    billing_period_range: Option<Value>,
) -> String {
    let mut body = json!({
        "Name": name,
        "Description": "an item read for one billing period",
        "BillingGroupArn": group,
        "ChargeDetails": { "Type": "FEE", "Percentage": { "PercentageValue": 10.0 } },
    });
    if let Some(range) = billing_period_range {
        body["BillingPeriodRange"] = range;
    }
    let (status, answered) = emulator
        .send("POST", "/create-custom-line-item", &body)
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

/// `BatchAssociateResourcesToCustomLineItem` of one resource, asserted to succeed.
async fn associate_ok(emulator: &Emulator, case: &str, target: &str, resource: &str) {
    let resources = vec![resource.to_string()];
    let (status, answered) = emulator.associate(target, &resources, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association of {resource} to answer HTTP 200, got {status} \
         with {answered}"
    );
    assert_eq!(
        answered.get("FailedAssociatedResources"),
        Some(&json!([])),
        "case {case:?}: expected no failed resource, got {answered}"
    );
}

/// `DeleteCustomLineItem` bounded to the billing periods from `inclusive_start` on,
/// asserted to succeed: the write that narrows an item out of a period it applied in.
async fn delete_from(emulator: &Emulator, case: &str, item: &str, inclusive_start: &str) {
    let (status, answered) = emulator
        .send(
            "POST",
            "/delete-custom-line-item",
            &json!({
                "Arn": item,
                "BillingPeriodRange": { "InclusiveStartBillingPeriod": inclusive_start },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete from {inclusive_start} to answer HTTP 200, got \
         {status} with {answered}"
    );
}

/// The read under test, as the request members `extra` carries name it.
async fn read(emulator: &Emulator, item: &str, extra: Value) -> (u16, Value) {
    let mut body = json!({ "Arn": item });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    emulator.send("POST", READ_PATH, &body).await
}

/// Assert the read of `billing_period` is REFUSED with the operation's modelled
/// `ResourceNotFoundException`: HTTP 404, the error shape on the wire, the item addressed
/// as `ResourceId` in its full-ARN form however the request spelled it, the resource class
/// as `ResourceType`, and no page of associations at all.
fn assert_not_found(case: &str, item: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 404,
        "case {case:?}: the period holds no version of the item, so the modelled 404 is \
         expected, got {status} with {body}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected the modelled ResourceNotFoundException, got {body}"
    );
    assert_eq!(
        body.get("ResourceId").and_then(Value::as_str),
        Some(item),
        "case {case:?}: expected the refusal to name the item addressed, got {body}"
    );
    assert_eq!(
        body.get("ResourceType").and_then(Value::as_str),
        Some(ITEM_RESOURCE_TYPE),
        "case {case:?}: expected the refusal to name the resource type, got {body}"
    );
    assert!(
        body.get("AssociatedResources").is_none(),
        "case {case:?}: a refusal reports no association, got {body}"
    );
    assert!(
        body.get("NextToken").is_none(),
        "case {case:?}: a refusal hands back no cursor, got {body}"
    );
}

/// One probed billing period of a row: the month a read asks for and whether any
/// generation of the item applies in it — which is whether the read is served at all.
struct Probe {
    billing_period: String,
    item_applies: bool,
}

/// `billing_period` with whether the item applies there.
fn probe(billing_period: &str, item_applies: bool) -> Probe {
    Probe {
        billing_period: billing_period.to_string(),
        item_applies,
    }
}

/// One row: how the item's own billing periods are bounded, and what the read of each
/// probed month must answer.
struct Row {
    case: &'static str,
    /// The `BillingPeriodRange` the item is created with — `None` for a create that names
    /// none, which applies from the period the call lands in on.
    item_range: Option<Value>,
    probes: Vec<Probe>,
}

/// THE RULE: the read is answered FOR one billing period, and the item it addresses is the
/// resource it references, so a period no generation of that item applies in is the
/// modelled 404 — never a 200 reporting no association — while a period the item does
/// apply in is served. Either way the read agrees with `ListCustomLineItems` for the same
/// period, which reports a version of the item in exactly the periods it applies in.
#[tokio::test]
async fn a_period_the_item_applies_in_no_version_of_is_refused_with_the_modelled_not_found() {
    let rows: Vec<Row> = vec![
        Row {
            case: "an item applying from the billing period of its create on",
            item_range: None,
            probes: vec![
                // A fixed month long before this account held anything, whatever the
                // clock says.
                probe("2020-01", false),
                probe(&period(-12), false),
                probe(&period(-1), false),
                probe(&period(0), true),
                probe(&one_digit_month(&period(0)), true),
                probe(&period(1), true),
                probe(&period(12), true),
            ],
        },
        Row {
            case: "an item applying from the previous billing period on",
            item_range: Some(json!({ "InclusiveStartBillingPeriod": period(-1) })),
            probes: vec![
                probe("2020-01", false),
                probe(&period(-2), false),
                probe(&period(-1), true),
                probe(&one_digit_month(&period(-1)), true),
                probe(&period(0), true),
                probe(&period(6), true),
            ],
        },
        Row {
            case: "an item bounded to three billing periods",
            item_range: Some(json!({
                "InclusiveStartBillingPeriod": period(-1),
                "ExclusiveEndBillingPeriod": period(2),
            })),
            probes: vec![
                probe(&period(-2), false),
                probe(&period(-1), true),
                probe(&period(0), true),
                probe(&period(1), true),
                // The range's exclusive end: the item applies in no part of it.
                probe(&period(2), false),
                probe(&period(3), false),
                probe(&period(12), false),
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let item =
            percentage_item_over(&emulator, case, &group, "read-for-a-period", row.item_range)
                .await;
        // The resource the item holds a share of: its own billing group, which is in scope
        // of it. The association is bounded to the periods the item applies in, so it can
        // never be in effect in a period the item is not.
        associate_ok(&emulator, case, &item, &group).await;

        for Probe {
            billing_period,
            item_applies,
        } in &row.probes
        {
            let case = format!("{case} read for {billing_period}");
            let (status, body) = read(
                &emulator,
                &item,
                json!({ "BillingPeriod": billing_period }),
            )
            .await;

            // THE RULE, stated as the agreement the two period-scoped reads owe each
            // other: the one read addressing the item by ARN is served for exactly the
            // periods the listing reports a version of it in.
            let listed = item_in(&emulator, &case, &item, billing_period).await;
            assert_eq!(
                listed.is_some(),
                *item_applies,
                "case {case:?}: expected ListCustomLineItems to report a version of the item: \
                 {item_applies}, got {listed:?}"
            );

            if !*item_applies {
                assert_not_found(&case, &item, status, &body);
                continue;
            }
            assert_eq!(
                status, 200,
                "case {case:?}: the item applies in that period, so the read is served, got \
                 {status} with {body}"
            );
            assert_eq!(
                body.get("Arn").and_then(Value::as_str),
                Some(item.as_str()),
                "case {case:?}: expected the answer to name the item read, got {body}"
            );
            assert!(
                body.get("AssociatedResources")
                    .and_then(Value::as_array)
                    .is_some(),
                "case {case:?}: expected a page of associations, got {body}"
            );
        }

        // ROUND TRIP: the association really is readable in a period the item applies in —
        // the current one, which the read that names no month is answered for.
        let (status, body) = read(&emulator, &item, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the default read to be served, got {status} with {body}"
        );
        let arns: Vec<String> = body
            .get("AssociatedResources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|element| element.get("Arn").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert_eq!(
            arns,
            vec![group.clone()],
            "case {case:?}: expected the default read to report the associated group, got \
             {arns:?}"
        );
    }
}

/// THE RULE on the write that MOVES the periods an item applies in: a bounded delete takes
/// the item out of the periods its range names, so from then on a read of one of those
/// periods is refused with the modelled 404 while a read of a period the delete left
/// untouched is still served — the same move `ListCustomLineItems` makes for those
/// periods.
///
/// The rows differ along where the delete's range starts (the current billing period, the
/// previous one — the earliest a write may name) and so along how many periods of the item
/// are left.
#[tokio::test]
async fn a_bounded_delete_moves_the_period_gate_with_the_item() {
    // (case, the period the delete is bounded from, the periods still served, the periods
    // refused)
    let rows: [(&str, String, Vec<String>, Vec<String>); 2] = [
        (
            "a delete from the current billing period on",
            period(0),
            vec![period(-1), one_digit_month(&period(-1))],
            vec![period(0), period(1), period(12)],
        ),
        (
            "a delete from the previous billing period on, which empties the item",
            period(-1),
            Vec::new(),
            vec![period(-1), period(0), period(1)],
        ),
    ];

    for (case, deleted_from, served, refused) in rows {
        let emulator = Emulator::new();
        let group = emulator.billing_group("payer").await;
        let item = percentage_item_over(
            &emulator,
            case,
            &group,
            "narrowed-by-a-delete",
            Some(json!({ "InclusiveStartBillingPeriod": period(-1) })),
        )
        .await;
        associate_ok(&emulator, case, &item, &group).await;
        // Before the delete the item applies in the period the delete will take, so the
        // read of it is served: the refusal below is the delete's doing.
        let (status, body) = read(&emulator, &item, json!({ "BillingPeriod": deleted_from })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read of {deleted_from} to be served before the delete, \
             got {status} with {body}"
        );

        delete_from(&emulator, case, &item, &deleted_from).await;

        for billing_period in &refused {
            let case = format!("{case} read for {billing_period}");
            let (status, body) = read(
                &emulator,
                &item,
                json!({ "BillingPeriod": billing_period }),
            )
            .await;
            assert_not_found(&case, &item, status, &body);
            let listed = item_in(&emulator, &case, &item, billing_period).await;
            assert!(
                listed.is_none(),
                "case {case:?}: the listing reports no version of the item there either, got \
                 {listed:?}"
            );
        }

        for billing_period in &served {
            let case = format!("{case} read for {billing_period}");
            let (status, body) = read(
                &emulator,
                &item,
                json!({ "BillingPeriod": billing_period }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the delete left that period, so the read is still served, got \
                 {status} with {body}"
            );
            let listed = item_in(&emulator, &case, &item, billing_period).await;
            assert!(
                listed.is_some(),
                "case {case:?}: the listing reports a version of the item there, got {listed:?}"
            );
        }
    }
}

/// THE RULE's precedence and spellings: the refusal is about the RESOURCE the read
/// references, so it precedes every page bound and every cursor the request carries, and
/// it answers whichever spelling of the item the modelled pattern admits — naming the item
/// back in its full-ARN form.
#[tokio::test]
async fn the_period_refusal_precedes_the_page_in_either_spelling_of_the_item() {
    let case = "an item applying from the billing period of its create on";
    let emulator = Emulator::new();
    let group = emulator.billing_group("payer").await;
    let item = percentage_item_over(&emulator, case, &group, "paged-read", None).await;
    // Two associations, so a one-row page of a served month hands back a cursor for the
    // refused reads below to carry.
    associate_ok(&emulator, case, &item, &group).await;
    let sibling = percentage_item_over(&emulator, case, &group, "paged-sibling", None).await;
    associate_ok(&emulator, case, &item, &sibling).await;

    let (status, body) = read(&emulator, &item, json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first page of the current month, got {status} with {body}"
    );
    let token = body
        .get("NextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a NextToken for a one-row page, got {body}")
        })
        .to_string();

    // Months the item applies in no version of: one before the period of its create, and
    // a month long before this account held anything.
    for billing_period in [period(-1), "2020-01".to_string()] {
        for (spelled_how, spelled) in [
            ("the full ARN", item.clone()),
            ("the bare id", bare_id(&item)),
        ] {
            for page in [
                json!({}),
                json!({ "MaxResults": 1 }),
                json!({ "MaxResults": 100 }),
                json!({ "NextToken": token }),
            ] {
                let case = format!(
                    "{case} read for {billing_period} by {spelled_how} with {page}"
                );
                let mut extra = json!({ "BillingPeriod": billing_period });
                for (member, value) in page.as_object().cloned().unwrap_or_default() {
                    extra[member] = value;
                }
                let (status, body) = read(&emulator, &spelled, extra).await;
                assert_not_found(&case, &item, status, &body);
            }
        }
    }
}
