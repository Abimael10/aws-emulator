// SPDX-License-Identifier: CC0-1.0

//! What the period-scoped READS of a pricing rule answer for a rule a delete has since
//! removed, end to end over the service's real `restJson1` wire through
//! [`MultiServiceRouter::dispatch`] — the same path the production router serves.
//!
//! RULE: a delete that names no billing-period range of its own ends the
//! billing-period-scoped record it takes FROM the billing period the call is made in and
//! never earlier, so every read scoped to a billing period reports the pricing rules that
//! existed in THAT period — from the period a rule was created in through the last period
//! before the delete, whether or not the rule is still stored — each as the generation of
//! itself that was in force then, and reports no such rule from the period of the delete
//! onward. One rule, so the period-scoped LISTING of the rules (`ListPricingRules`) and
//! the period-scoped read that addresses ONE rule by ARN
//! (`ListPricingPlansAssociatedWithPricingRule`, which answers the modelled 404 for a
//! period that held no such rule) cannot disagree about the period they are read for.
//!
//! The rows differ along what the rule generalises over: how many billing periods the
//! rule's life spanned (none at all — created and deleted in one period — one, two and
//! thirteen, crossing a year), where that life falls relative to the current billing
//! period (ending in it, in the previous one, a year back), the period the read asks
//! about (a year before the creation, the period before it, the creation itself, the one
//! after, the last period before the delete, the period of the delete, the one after it,
//! the current one and a future one), and how many generations of itself the rule went
//! through.
//!
//! `tests/all/pricing_rule_billing_periods.rs` owns this rule for a LIVE rule and
//! `tests/all/pricing_rule_delete_reader_agreement.rs` what the readers of a plan's
//! ASSOCIATION to the rule answer around the delete; this suite owns what the reads of
//! the rule itself answer once it is deleted.
//!
//! The wire can neither create nor delete a rule in a PAST billing period — a write
//! happens now — so a row writes through the real wire and then backdates the stored
//! instants, which is the one thing the test cannot ask the service for.

use serde_json::{json, Value};

use crate::association_delete_gate_spans_billing_periods::error_code;
use crate::pricing_rule_association_periods::{shift_period, Emulator, DELETE_RULE_PATH};

/// `PUT /update-pricing-rule` — the write that opens a new generation of a rule
/// (`smithy_query(query="operation",operation="UpdatePricingRule")`).
const UPDATE_RULE_PATH: &str = "/update-pricing-rule";

/// `POST /list-pricing-rules`, the period-scoped listing of the rules.
const LIST_RULES_PATH: &str = "/list-pricing-rules";

/// `ModifierPercentage` every rule here is created with.
const CREATED_MODIFIER: f64 = 10.0;

/// The billing period the service calls current, read from the same clock it answers from
/// so an expectation holds whatever month the suite runs in.
fn current_period() -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::current_billing_period()
}

/// The `PricingRuleListElement` `ListPricingRules` reports for ONE rule in `period`, or
/// `None` when that period reports no such rule.
async fn element_in(em: &Emulator, case: &str, rule: &str, period: &str) -> Option<Value> {
    let (status, body) = em
        .post(
            LIST_RULES_PATH,
            &json!({"BillingPeriod": period, "Filters": {"Arns": [rule]}}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case}: ListPricingRules for {period} expected 200, got {status} with {body:?}"
    );
    body["PricingRules"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|element| element["Arn"].as_str() == Some(rule))
}

/// Delete the rule through the real wire, which is the only way a rule is deleted.
async fn delete_rule(em: &Emulator, case: &str, rule: &str) {
    let (status, body) = em.post(DELETE_RULE_PATH, &json!({"Arn": rule})).await;
    assert_eq!(
        status, 200,
        "case {case}: setup DeletePricingRule expected 200, got {status} with {body:?}"
    );
}

/// One row of the table: a rule whose life ran from `created` months from the current
/// billing period to the delete `deleted` months from it.
struct Row {
    case: &'static str,
    /// Names differ along the characters the modelled `PricingRuleName` pattern
    /// `^[a-zA-Z0-9_\+=\.\-@]+$` allows
    /// (`smithy_query(query="shape",shape="PricingRuleName")`).
    name: &'static str,
    created: i64,
    deleted: i64,
}

/// The rows differ along how many billing periods the rule's life covered and where that
/// life falls relative to the CURRENT billing period: a life of no period at all (created
/// and deleted in one month, so no period ever reports it), of one period, of two, and of
/// thirteen crossing a year — ending in the current period, in the previous one, and a
/// year back.
const ROWS: &[Row] = &[
    Row {
        case: "created and deleted in the current billing period",
        name: "Erased_Now",
        created: 0,
        deleted: 0,
    },
    Row {
        case: "created and deleted in the previous billing period",
        name: "Erased@Prev",
        created: -1,
        deleted: -1,
    },
    Row {
        case: "one period of life, ending in the current billing period",
        name: "Erased+One",
        created: -1,
        deleted: 0,
    },
    Row {
        case: "two periods of life, ending in the previous billing period",
        name: "Erased.Two",
        created: -3,
        deleted: -1,
    },
    Row {
        case: "thirteen periods of life, ending in the current billing period",
        name: "Erased-Year",
        created: -13,
        deleted: 0,
    },
    Row {
        case: "thirteen periods of life, ending a year back",
        name: "Erased=Ancient",
        created: -25,
        deleted: -12,
    },
];

/// The periods one row reads, as offsets in months from the current billing period: a
/// year before the creation, the period before it, the creation itself, the one after it,
/// the last period before the delete, the period of the delete, the one after it, the
/// current one and a year ahead.
fn read_offsets(row: &Row) -> Vec<i64> {
    vec![
        row.created - 13,
        row.created - 1,
        row.created,
        row.created + 1,
        row.deleted - 1,
        row.deleted,
        row.deleted + 1,
        0,
        12,
    ]
}

#[tokio::test]
async fn a_deleted_rule_is_reported_for_the_periods_it_existed_in_and_for_no_other() {
    let em = Emulator::new();
    let now = current_period();

    for row in ROWS {
        let case = row.case;
        let created = shift_period(&now, row.created);
        let deleted = shift_period(&now, row.deleted);
        let rule = em.create_rule(row.name).await;
        em.create_rule_in(&rule, &created);
        delete_rule(&em, case, &rule).await;
        em.delete_rule_in(&rule, &deleted);

        for offset in read_offsets(row) {
            let period = shift_period(&now, offset);
            // The rule existed in every period from the one it was created in up to, but
            // not including, the one the delete was made in: the delete takes effect in
            // its own period, and the periods before it are closed bills the rule really
            // was in force for.
            let existed = offset >= row.created && offset < row.deleted;
            let element = element_in(&em, case, &rule, &period).await;
            assert_eq!(
                element.is_some(),
                existed,
                "case {case}: expected ListPricingRules for {period} to report {rule}: \
                 {existed}, got {element:?}"
            );
            if let Some(element) = &element {
                assert_eq!(
                    element["Name"].as_str(),
                    Some(row.name),
                    "case {case}: expected the name the rule carried in {period}, got \
                     {element:?}"
                );
                assert_eq!(
                    element["ModifierPercentage"].as_f64(),
                    Some(CREATED_MODIFIER),
                    "case {case}: expected the modifier the rule carried in {period}, got \
                     {element:?}"
                );
                assert_eq!(
                    element["AssociatedPricingPlanCount"].as_i64(),
                    Some(0),
                    "case {case}: no pricing plan ever associated the rule, so {period} must \
                     count none, got {element:?}"
                );
            }

            // The read that addresses the ONE rule by ARN resolves it in the period being
            // read, so it agrees with the listing of that period: 200 while the period
            // held the rule, and the modelled 404 otherwise.
            let (status, answer) = em.plans_for_rule_response(&rule, Some(&period)).await;
            let expected_status = if existed { 200 } else { 404 };
            assert_eq!(
                status, expected_status,
                "case {case}: expected ListPricingPlansAssociatedWithPricingRule for {period} \
                 to answer {expected_status}, got {status} with {answer:?}"
            );
            if !existed {
                assert_eq!(
                    error_code(&answer),
                    "ResourceNotFoundException",
                    "case {case}: expected ResourceNotFoundException for {period}, got \
                     {answer:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_generation_a_period_reports_for_a_deleted_rule_is_the_one_in_force_then() {
    let em = Emulator::new();
    let now = current_period();
    let case = "created a year back, changed in between, deleted now";

    // The rule is created a year before the current period, its second generation takes
    // effect six periods later, and the delete is made now — so the periods it existed in
    // split between the two generations, six periods each.
    let created = shift_period(&now, -12);
    let changed = shift_period(&now, -6);
    let rule = em.create_rule("GenerationFirst").await;
    em.create_rule_in(&rule, &created);
    let (status, body) = em
        .put(
            UPDATE_RULE_PATH,
            &json!({
                "Arn": rule,
                "Name": "GenerationSecond",
                "Description": "the generation in force from the change on",
                "ModifierPercentage": 25.5,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case}: setup UpdatePricingRule expected 200, got {status} with {body:?}"
    );
    em.restate_generation_in(&rule, &changed);
    delete_rule(&em, case, &rule).await;

    // Per period: the generation the rule reports there, or `None` from the delete on.
    let expected: [(i64, Option<(&str, f64)>); 7] = [
        (-13, None),
        (-12, Some(("GenerationFirst", CREATED_MODIFIER))),
        (-7, Some(("GenerationFirst", CREATED_MODIFIER))),
        (-6, Some(("GenerationSecond", 25.5))),
        (-1, Some(("GenerationSecond", 25.5))),
        (0, None),
        (1, None),
    ];
    for (offset, generation) in expected {
        let period = shift_period(&now, offset);
        let element = element_in(&em, case, &rule, &period).await;
        match generation {
            None => assert!(
                element.is_none(),
                "case {case}: {period} is outside the periods the rule existed in, so it must \
                 report no such rule, got {element:?}"
            ),
            Some((name, modifier)) => {
                let element = element.unwrap_or_else(|| {
                    panic!("case {case}: expected {period} to report the rule {rule}")
                });
                assert_eq!(
                    element["Name"].as_str(),
                    Some(name),
                    "case {case}: expected the name of the generation in force in {period}, got \
                     {element:?}"
                );
                assert_eq!(
                    element["ModifierPercentage"].as_f64(),
                    Some(modifier),
                    "case {case}: expected the modifier of the generation in force in {period}, \
                     got {element:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_period_lists_the_rules_it_held_live_and_deleted_in_one_sequence() {
    let em = Emulator::new();
    let now = current_period();
    let read_period = shift_period(&now, -6);
    let case = "three rules created together, the second and third deleted since";

    // Three rules created in the same earlier period, of which the SECOND is deleted in a
    // period after that one (so the period read still reports it) and the THIRD in the
    // very period the read asks about (so that period reports it no more).
    let mut rules: Vec<String> = Vec::new();
    for index in 0..3usize {
        let rule = em.create_rule(&format!("Sequenced{index}")).await;
        em.create_rule_in(&rule, &read_period);
        rules.push(rule);
    }
    delete_rule(&em, case, &rules[1]).await;
    em.delete_rule_in(&rules[1], &shift_period(&now, -3));
    delete_rule(&em, case, &rules[2]).await;
    em.delete_rule_in(&rules[2], &read_period);

    let (status, body) = em
        .post(LIST_RULES_PATH, &json!({"BillingPeriod": read_period}))
        .await;
    assert_eq!(
        status, 200,
        "case {case}: ListPricingRules expected 200, got {status} with {body:?}"
    );
    let reported: Vec<String> = body["PricingRules"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|element| element["Arn"].as_str().unwrap_or_default().to_string())
        .collect();
    // An ARN carries the monotonic id this service mints, so the one sequence a period
    // lists its rules in is creation order whether or not a rule is still stored.
    assert_eq!(
        reported,
        vec![rules[0].clone(), rules[1].clone()],
        "case {case}: expected {read_period} to list the live rule and the rule deleted later, \
         in creation order, got {reported:?}"
    );
}
