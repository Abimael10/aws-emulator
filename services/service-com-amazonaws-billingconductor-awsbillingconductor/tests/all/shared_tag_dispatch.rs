// SPDX-License-Identifier: CC0-1.0

//! The READ half and the WRITE half of the tagging surface land on the SAME record, for
//! every taggable kind — driven end to end over the service's real `restJson1` wire
//! through the harness in `tests/all/tagging_surface.rs`.
//!
//! RULE under test: a tagging `ResourceArn` names ONE stored resource, so the operation
//! that reports a resource's tags (`ListTagsForResource`) and the operations that write
//! them (`TagResource`, `UntagResource`) reach that one record — whichever spelling of
//! the ARN each call used — and touch no other resource's tags.
//!
//! The rows differ along the resource KIND (all four the modelled tagging `Arn`
//! enumerates: `pricingplan`, `billinggroup`, `pricingrule` and `customlineitem`) and
//! along the pair of ARN SPELLINGS the two halves address one resource by (the full ARN
//! on both halves; the `billinggroup` separator the modelled pattern makes optional on
//! the write half only; the bare custom-line-item id on the write half only), so each
//! row writes through one spelling and reads back through another.
//!
//! `tests/all/tagging_surface.rs` asserts what each tagging operation ANSWERS; this suite
//! asserts the two halves agree on which record they act on, for every kind and in both
//! directions (a write the read sees, and a removal the read sees), plus that a write to
//! one kind leaves every other kind's tags where they were.
//!
//! A second RULE under test here, because it is what makes the first one reachable at
//! all: an id this service MINTS carries the spelling every modelled member that spells
//! that kind's id admits, so the resource a create returns is addressable by every
//! operation that takes its ARN — whatever position of this service's one id sequence the
//! create fell at. Those rows differ along the POSITION in that sequence and along the
//! resource kind.

use crate::tagging_surface::{bare_id, expected, tag_map, Emulator, TAGGING_STATUS};
use serde_json::json;

/// One row: a taggable resource, the pairs its create persisted, and the ARN spelling
/// each half of the surface addresses it by.
struct KindCase {
    label: &'static str,
    /// The spelling `TagResource` and `UntagResource` address the resource by.
    write_through: String,
    /// The spelling `ListTagsForResource` reads it back through.
    read_through: String,
    /// The pairs this resource's own create persisted.
    created: Vec<(&'static str, &'static str)>,
    /// The pair the write half merges in and then removes again.
    added: (&'static str, &'static str),
}

/// RULE: both halves of the tagging surface resolve one record per resource, for every
/// taggable kind and through every ARN spelling that kind admits — a merge is visible to
/// the read, a removal is visible to the read, and neither disturbs another kind.
#[tokio::test]
async fn both_halves_of_the_tagging_surface_resolve_one_record_per_kind() {
    let em = Emulator::new();
    let plan = em
        .create_plan("DispatchPlan", Some(tag_map(&[("kind", "pricingplan")])))
        .await;
    let group = em
        .create_group(
            "DispatchGroup",
            &plan,
            Some(tag_map(&[("kind", "billinggroup")])),
        )
        .await;
    let rule = em
        .create_rule("DispatchRule", Some(tag_map(&[("kind", "pricingrule")])))
        .await;
    let item = em
        .create_item(
            "DispatchItem",
            &group,
            json!({ "Type": "FEE", "Flat": { "ChargeValue": 3.5 } }),
            Some(tag_map(&[("kind", "customlineitem")])),
        )
        .await;

    let cases: Vec<KindCase> = vec![
        KindCase {
            label: "a pricing plan, both halves addressing its full ARN",
            write_through: plan.clone(),
            read_through: plan.clone(),
            created: vec![("kind", "pricingplan")],
            added: ("added", "plan"),
        },
        KindCase {
            label: "a billing group, written without the optional separator and read with it",
            write_through: group.replace("billinggroup/", "billinggroup"),
            read_through: group.clone(),
            created: vec![("kind", "billinggroup")],
            added: ("added", "group"),
        },
        KindCase {
            label: "a pricing rule, both halves addressing its full ARN",
            write_through: rule.clone(),
            read_through: rule.clone(),
            created: vec![("kind", "pricingrule")],
            added: ("added", "rule"),
        },
        KindCase {
            label: "a custom line item, written by its bare id and read by its full ARN",
            write_through: bare_id(&item).to_string(),
            read_through: item.clone(),
            created: vec![("kind", "customlineitem")],
            added: ("added", "item"),
        },
    ];

    // What each row's resource is expected to hold right now.
    let mut held: Vec<Vec<(&'static str, &'static str)>> =
        cases.iter().map(|case| case.created.clone()).collect();

    // Every row starts at what its own create persisted, as the read half reports it
    // through that row's read spelling.
    for (index, case) in cases.iter().enumerate() {
        let reported = em.stored_tags(case.label, &case.read_through).await;
        assert_eq!(
            reported,
            expected(&held[index]),
            "case {:?}: expected the create's own tags {:?}, got {reported:?}",
            case.label,
            expected(&held[index])
        );
    }

    for (index, case) in cases.iter().enumerate() {
        let label = case.label;

        // The WRITE half merges one pair, through this row's write spelling.
        let (status, body) = em.tag(&case.write_through, &tag_map(&[case.added])).await;
        assert_eq!(
            status, TAGGING_STATUS,
            "case {label:?}: TagResource through {:?} expected {TAGGING_STATUS}, got {status} \
             with {body}",
            case.write_through
        );
        held[index].push(case.added);

        // …and the READ half, through the OTHER spelling, reports it: one record.
        let merged = em.stored_tags(label, &case.read_through).await;
        assert_eq!(
            merged,
            expected(&held[index]),
            "case {label:?}: expected the merged tags {:?} read back through {:?}, got \
             {merged:?}",
            expected(&held[index]),
            case.read_through
        );
        let through_write_spelling = em.stored_tags(label, &case.write_through).await;
        assert_eq!(
            through_write_spelling, merged,
            "case {label:?}: expected both ARN spellings to report one record's tags, got \
             {through_write_spelling:?} through {:?} and {merged:?} through {:?}",
            case.write_through, case.read_through
        );
        assert_untouched(&em, &cases, &held, index, "the merge").await;

        // The WRITE half removes the same pair again, and the READ half sees that too.
        let (status, body) = em.untag(&case.write_through, &[case.added.0]).await;
        assert_eq!(
            status, TAGGING_STATUS,
            "case {label:?}: UntagResource through {:?} expected {TAGGING_STATUS}, got \
             {status} with {body}",
            case.write_through
        );
        held[index].pop();
        let left = em.stored_tags(label, &case.read_through).await;
        assert_eq!(
            left,
            expected(&held[index]),
            "case {label:?}: expected the removal to leave {:?}, got {left:?}",
            expected(&held[index])
        );
        assert_untouched(&em, &cases, &held, index, "the removal").await;
    }
}

/// Assert that every row OTHER than `acting` still holds exactly what it held: a write
/// addressed at one resource reaches that resource's record and no other kind's.
async fn assert_untouched(
    em: &Emulator,
    cases: &[KindCase],
    held: &[Vec<(&'static str, &'static str)>],
    acting: usize,
    what: &str,
) {
    for (index, other) in cases.iter().enumerate() {
        if index == acting {
            continue;
        }
        let reported = em.stored_tags(other.label, &other.read_through).await;
        assert_eq!(
            reported,
            expected(&held[index]),
            "case {:?}: {what} on {:?} must leave it holding {:?}, got {reported:?}",
            other.label,
            cases[acting].label,
            expected(&held[index])
        );
    }
}

/// The pricing plan Amazon Web Services owns rather than the caller, which a billing
/// group may name without any create first
/// (`smithy_query(query="shape",shape="PricingPlanArn")`:
/// `(BasicPricingPlan|Passthrough|[a-zA-Z0-9]{10})` under the `aws` account segment) — so
/// a group can be created at a chosen position of the shared id sequence without a plan
/// of this account's own shifting that position.
const MANAGED_PRICING_PLAN: &str = "arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan";

/// The id spelling one resource kind's modelled members AGREE on: the resource segment
/// its ARN carries, how many characters its id has and whether those characters are
/// digits only.
///
/// From `smithy_query(query="shape",shape="Arn,BillingGroupArn,PricingPlanArn,PricingRuleArn,CustomLineItemArn")`:
/// the tagging `Arn` spells a billing group `billinggroup/?[0-9]{12}`, a pricing plan
/// `pricingplan/[a-zA-Z0-9]{10}`, a pricing rule `pricingrule/[a-zA-Z0-9]{10}` and a
/// custom line item `(customlineitem/)?[a-zA-Z0-9]{10}`, while the per-kind members
/// admit `[a-zA-Z0-9]{10,12}` for a billing group and `[a-zA-Z0-9]{10}` for the other
/// three. A minted id has to satisfy both members of its own kind, so these are the
/// intersections.
#[derive(Clone, Copy, Debug)]
struct IdSpelling {
    segment: &'static str,
    width: usize,
    digits_only: bool,
}

const BILLING_GROUP_ID: IdSpelling = IdSpelling {
    segment: "billinggroup",
    width: 12,
    digits_only: true,
};

const PRICING_PLAN_ID: IdSpelling = IdSpelling {
    segment: "pricingplan",
    width: 10,
    digits_only: false,
};

const PRICING_RULE_ID: IdSpelling = IdSpelling {
    segment: "pricingrule",
    width: 10,
    digits_only: false,
};

const CUSTOM_LINE_ITEM_ID: IdSpelling = IdSpelling {
    segment: "customlineitem",
    width: 10,
    digits_only: false,
};

/// Assert that the id `arn` carries is spelled the way every modelled member that spells
/// this kind admits.
fn assert_id_spelling(case: &str, arn: &str, spelling: &IdSpelling) {
    let prefix = format!(":{}/", spelling.segment);
    assert!(
        arn.contains(&prefix),
        "case {case:?}: expected an ARN naming a {}, got {arn:?}",
        spelling.segment
    );
    let id = bare_id(arn);
    assert_eq!(
        id.chars().count(),
        spelling.width,
        "case {case:?}: expected a {}-character id, got {id:?}",
        spelling.width
    );
    assert!(
        id.chars().all(|character| character.is_ascii_alphanumeric()),
        "case {case:?}: expected an id of ASCII letters and digits, got {id:?}"
    );
    if spelling.digits_only {
        assert!(
            id.chars().all(|character| character.is_ascii_digit()),
            "case {case:?}: expected an id of digits only, got {id:?}"
        );
    }
}

/// Assert that the tagging surface serves the resource `read_through` names: the pairs
/// its create persisted are reported, a merge through `write_through` is visible to the
/// read, and the removal of that pair leaves what the create stored.
async fn assert_tagging_round_trip(
    em: &Emulator,
    case: &str,
    write_through: &str,
    read_through: &str,
    created: &[(&str, &str)],
) {
    let reported = em.stored_tags(case, read_through).await;
    assert_eq!(
        reported,
        expected(created),
        "case {case:?}: expected the create's own tags {:?} read back through \
         {read_through:?}, got {reported:?}",
        expected(created)
    );

    let added = ("added", "later");
    let (status, body) = em.tag(write_through, &tag_map(&[added])).await;
    assert_eq!(
        status, TAGGING_STATUS,
        "case {case:?}: TagResource through {write_through:?} expected {TAGGING_STATUS}, got \
         {status} with {body}"
    );
    let mut after = created.to_vec();
    after.push(added);
    let merged = em.stored_tags(case, read_through).await;
    assert_eq!(
        merged,
        expected(&after),
        "case {case:?}: expected the merged tags {:?}, got {merged:?}",
        expected(&after)
    );

    let (status, body) = em.untag(write_through, &[added.0]).await;
    assert_eq!(
        status, TAGGING_STATUS,
        "case {case:?}: UntagResource through {write_through:?} expected {TAGGING_STATUS}, got \
         {status} with {body}"
    );
    let left = em.stored_tags(case, read_through).await;
    assert_eq!(
        left,
        expected(created),
        "case {case:?}: expected the removal to leave {:?}, got {left:?}",
        expected(created)
    );
}

/// RULE: a billing group id this service mints is spelled the way EVERY modelled member
/// that carries one admits — the tagging `Arn`'s `billinggroup/?[0-9]{12}` included — so
/// the tagging surface addresses every group a create returns and the `Tags` that create
/// persisted are readable, extendable and removable.
///
/// The rows differ along the POSITION the group falls at in this service's one id
/// sequence, which is what the id's spelling must not depend on: the first resource of a
/// store, the tenth (the first position a wider alphabet would have rendered with a
/// letter), the one after it, the last position of a single character and the first of
/// two, and a position deep into the second character.
#[tokio::test]
async fn a_minted_billing_group_is_addressable_whatever_its_position_in_the_id_sequence() {
    for position in [1_usize, 10, 11, 36, 37, 100] {
        let case =
            format!("a billing group minted at position {position} of the shared id sequence");
        let em = Emulator::new();
        // Push this store's id sequence to just before `position`, with resources of
        // another kind: the group below is then the `position`-th id this store mints.
        for index in 1..position {
            em.create_plan(&format!("FillerPlan{index}"), None).await;
        }

        let group = em
            .create_group(
                "SequenceGroup",
                MANAGED_PRICING_PLAN,
                Some(tag_map(&[("stage", "test")])),
            )
            .await;
        assert_id_spelling(&case, &group, &BILLING_GROUP_ID);
        // Written without the separator the modelled pattern makes optional and read
        // through the full ARN, as the first suite above does for one position.
        assert_tagging_round_trip(
            &em,
            &case,
            &group.replace("billinggroup/", "billinggroup"),
            &group,
            &[("stage", "test")],
        )
        .await;
    }
}

/// RULE, for every kind at once: each create's minted id is spelled the way its own
/// kind's modelled members admit, and those ids stay distinct from one another, however
/// far into the shared id sequence the creates fall.
#[tokio::test]
async fn minted_ids_of_every_kind_keep_their_modelled_spelling_late_in_the_sequence() {
    let em = Emulator::new();
    // Far enough into the shared sequence that the last two characters of a 36-character
    // rendering are no longer digits.
    for index in 1..=40 {
        em.create_plan(&format!("FillerPlan{index}"), None).await;
    }

    let plan = em
        .create_plan("LatePlan", Some(tag_map(&[("kind", "pricingplan")])))
        .await;
    let rule = em
        .create_rule("LateRule", Some(tag_map(&[("kind", "pricingrule")])))
        .await;
    let group = em
        .create_group("LateGroup", &plan, Some(tag_map(&[("kind", "billinggroup")])))
        .await;
    let item = em
        .create_item(
            "LateItem",
            &group,
            json!({ "Type": "FEE", "Flat": { "ChargeValue": 1.25 } }),
            Some(tag_map(&[("kind", "customlineitem")])),
        )
        .await;

    /// One row: a resource minted late in the sequence, the spelling its kind's modelled
    /// members agree on, and the ARN spelling the write half addresses it by.
    struct LateCase {
        label: &'static str,
        arn: String,
        write_through: String,
        spelling: IdSpelling,
        created: (&'static str, &'static str),
    }

    let rows = vec![
        LateCase {
            label: "a pricing plan minted late in the shared id sequence",
            arn: plan.clone(),
            write_through: plan.clone(),
            spelling: PRICING_PLAN_ID,
            created: ("kind", "pricingplan"),
        },
        LateCase {
            label: "a billing group minted late in the shared id sequence",
            write_through: group.replace("billinggroup/", "billinggroup"),
            arn: group.clone(),
            spelling: BILLING_GROUP_ID,
            created: ("kind", "billinggroup"),
        },
        LateCase {
            label: "a pricing rule minted late in the shared id sequence",
            arn: rule.clone(),
            write_through: rule.clone(),
            spelling: PRICING_RULE_ID,
            created: ("kind", "pricingrule"),
        },
        LateCase {
            label: "a custom line item minted late in the shared id sequence",
            write_through: bare_id(&item).to_string(),
            arn: item.clone(),
            spelling: CUSTOM_LINE_ITEM_ID,
            created: ("kind", "customlineitem"),
        },
    ];

    for row in &rows {
        assert_id_spelling(row.label, &row.arn, &row.spelling);
        assert_tagging_round_trip(&em, row.label, &row.write_through, &row.arn, &[row.created])
            .await;
    }

    let mut ids: Vec<&str> = rows.iter().map(|row| bare_id(&row.arn)).collect();
    let minted = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        minted,
        "expected every minted id to be distinct, got {ids:?}"
    );
}
