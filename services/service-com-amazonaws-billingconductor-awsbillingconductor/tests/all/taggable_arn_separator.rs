// SPDX-License-Identifier: CC0-1.0

//! The ARN SPELLING the tagging surface resolves, driven end to end over the service's
//! real `restJson1` wire through the harness in `tests/all/tagging_surface.rs`.
//!
//! RULE under test: the `/` between an ARN's resource-type segment and the resource id is
//! part of how that resource is spelled, and this service's ARN grammar writes it
//! omissible in exactly ONE alternative (the billing group's). A value that runs the type
//! segment straight into the id therefore names a resource only for that kind; for every
//! other kind it names NO resource, so the tagging surface answers its not-found and
//! writes nothing — a resource cannot be read or mutated under an identifier the
//! service's own ARN grammar does not define.
//!
//! The rows differ along what the rule generalises over: the resource KIND the ARN names
//! (all four the modelled tagging `Arn` enumerates, so both sides of the rule are
//! exercised), the resource id (every one minted by this suite's own creates), whether
//! the separator-bearing spelling of the same id names a stored resource, one Amazon Web
//! Services owns, or nothing at all, and the partition the ARN carries.

use crate::tagging_surface::{assert_tagging_not_found, expected, tag_map, Emulator, TAGGING_STATUS};
use serde_json::json;

/// The resource kinds the modelled tagging `Arn` enumerates, each with whether ITS
/// alternative writes the `/` after the type segment as optional
/// (`smithy_query(query="shape",shape="Arn")`:
/// `billinggroup/?[0-9]{12}` | `pricingplan/[a-zA-Z0-9]{10}` |
/// `pricingrule/[a-zA-Z0-9]{10}` | `(customlineitem/)?[a-zA-Z0-9]{10}` — the `?` of the
/// last alternative governs the whole ARN prefix, not the separator). Every per-kind ARN
/// shape of this service spells it the same way
/// (`smithy_query(query="shape",shape="BillingGroupArn,PricingRuleArn,CustomLineItemArn,PricingPlanFullArn")`).
const SEPARATOR_OPTIONAL_BY_SEGMENT: &[(&str, bool)] = &[
    ("billinggroup", true),
    ("pricingplan", false),
    ("pricingrule", false),
    ("customlineitem", false),
];

/// What the grammar says about `segment`'s separator, read from the catalog above.
fn separator_is_optional(segment: &str) -> bool {
    let Some((_, optional)) = SEPARATOR_OPTIONAL_BY_SEGMENT
        .iter()
        .find(|(name, _)| *name == segment)
    else {
        panic!("{segment:?} is not a resource kind the modelled Arn enumerates");
    };
    *optional
}

/// `arn` with the separator after its resource-type segment removed — the one spelling
/// difference every case of this suite turns on.
fn without_separator(arn: &str, segment: &str) -> String {
    arn.replace(&format!("{segment}/"), segment)
}

/// One stored resource: the kind its ARN names, the ARN its create minted, and the tags
/// that create persisted.
struct KindRow {
    label: &'static str,
    segment: &'static str,
    arn: String,
    created: Vec<(&'static str, &'static str)>,
}

/// RULE: a separator-less spelling addresses the resource only for the kind whose grammar
/// makes the separator optional; for the other three kinds it names nothing, the surface
/// refuses it, and the resource keeps exactly the tags its create stored.
#[tokio::test]
async fn the_type_separator_is_omissible_only_where_the_arn_grammar_writes_it_optional() {
    let em = Emulator::new();
    let plan = em
        .create_plan("SeparatorPlan", Some(tag_map(&[("kind", "pricingplan")])))
        .await;
    let group = em
        .create_group(
            "SeparatorGroup",
            &plan,
            Some(tag_map(&[("kind", "billinggroup")])),
        )
        .await;
    let rule = em
        .create_rule("SeparatorRule", Some(tag_map(&[("kind", "pricingrule")])))
        .await;
    let item = em
        .create_item(
            "SeparatorItem",
            &group,
            json!({ "Type": "FEE", "Flat": { "ChargeValue": 10.0 } }),
            Some(tag_map(&[("kind", "customlineitem")])),
        )
        .await;

    let rows = [
        KindRow {
            label: "a billing group",
            segment: "billinggroup",
            arn: group,
            created: vec![("kind", "billinggroup")],
        },
        KindRow {
            label: "a pricing plan",
            segment: "pricingplan",
            arn: plan,
            created: vec![("kind", "pricingplan")],
        },
        KindRow {
            label: "a pricing rule",
            segment: "pricingrule",
            arn: rule,
            created: vec![("kind", "pricingrule")],
        },
        KindRow {
            label: "a custom line item",
            segment: "customlineitem",
            arn: item,
            created: vec![("kind", "customlineitem")],
        },
    ];

    for row in &rows {
        let unseparated = without_separator(&row.arn, row.segment);
        assert_ne!(
            unseparated, row.arn,
            "case {:?}: the minted ARN {:?} should carry a {:?} separator to remove",
            row.label, row.arn, row.segment
        );

        if separator_is_optional(row.segment) {
            let (status, body) = em
                .tag(&unseparated, &tag_map(&[("spelling", "unseparated")]))
                .await;
            assert_eq!(
                status, TAGGING_STATUS,
                "case {:?}: the grammar makes this kind's separator optional, so {unseparated:?} \
                 must address the same resource with HTTP {TAGGING_STATUS}, got {status} with \
                 {body}",
                row.label
            );
            let mut want = row.created.clone();
            want.push(("spelling", "unseparated"));
            let stored = em.stored_tags(row.label, &row.arn).await;
            assert_eq!(
                stored,
                expected(&want),
                "case {:?}: expected both spellings of {:?} to reach one resource, got {stored:?}",
                row.label,
                row.arn
            );
        } else {
            // The spelling names no resource of this service, so every operation of the
            // surface answers its not-found and nothing is created by the attempt.
            assert_tagging_not_found(&em, row.label, &unseparated).await;
            let stored = em.stored_tags(row.label, &row.arn).await;
            assert_eq!(
                stored,
                expected(&row.created),
                "case {:?}: a refused spelling must leave {:?} with the tags its create \
                 stored, got {stored:?}",
                row.label,
                row.arn
            );
        }

        // Whatever the grammar says about the separator-less form, the spelling that
        // carries the separator still addresses this resource: the refusal above is about
        // the SPELLING and not about the resource.
        let (status, body) = em.tag(&row.arn, &tag_map(&[("env", "prod")])).await;
        assert_eq!(
            status, TAGGING_STATUS,
            "case {:?}: expected the separator-bearing spelling {:?} to be served with HTTP \
             {TAGGING_STATUS}, got {status} with {body}",
            row.label, row.arn
        );
        let stored = em.stored_tags(row.label, &row.arn).await;
        assert!(
            stored.contains(&("env".to_string(), "prod".to_string())),
            "case {:?}: expected the write through {:?} to land, got {stored:?}",
            row.label,
            row.arn
        );
    }
}

/// RULE, past the resources this account stores: the spelling decides whether an ARN
/// names a resource AT ALL, before any question of who owns it. A separator-less value is
/// refused as naming nothing even when the separator-bearing spelling of the same id
/// names a pricing plan Amazon Web Services owns — which on its own earns the modelled
/// 403 (`tests/all/aws_managed_pricing_plan_tagging.rs`) — or an id no create ever
/// minted.
#[tokio::test]
async fn a_separator_less_spelling_names_no_resource_in_any_account_or_partition() {
    let em = Emulator::new();
    let account = engine::emulator_account_id();
    // The plans Amazon Web Services owns, as the model enumerates them
    // (`smithy_query(query="shape",shape="PricingPlanFullArn")`:
    // `(aws|[0-9]{12}):pricingplan/(BasicPricingPlan|Passthrough|[a-zA-Z0-9]{10})`).
    let managed = ["BasicPricingPlan", "Passthrough"];
    let mut cases: Vec<(String, String)> = managed
        .iter()
        .map(|id| {
            (
                format!("the plan Amazon Web Services owns as {id:?}, without the separator"),
                format!("arn:aws:billingconductor::aws:pricingplan{id}"),
            )
        })
        .collect();
    cases.extend([
        (
            "a plan id no create minted, without the separator".to_string(),
            format!("arn:aws:billingconductor::{account}:pricingplanABCDEFGHIJ"),
        ),
        (
            "a pricing rule id no create minted, without the separator".to_string(),
            format!("arn:aws:billingconductor::{account}:pricingrule0123456789"),
        ),
        (
            "a custom line item id no create minted, without the separator".to_string(),
            format!("arn:aws:billingconductor::{account}:customlineitemZYXWVUTSRQ"),
        ),
        (
            "a plan id in the China partition, without the separator".to_string(),
            "arn:aws-cn:billingconductor::123456789012:pricingplanABCDEFGHIJ".to_string(),
        ),
    ]);

    for (case, arn) in &cases {
        assert_tagging_not_found(&em, case, arn).await;
    }
}
