// SPDX-License-Identifier: CC0-1.0

//! The conditionally present members of a reported credit, end to end over the
//! awsJson1_0 wire.
//!
//! The RULE: a member the model documents as present only under a stated
//! condition on another member is reported only for a record that condition
//! holds for, whatever else the store happens to carry. `CreditData$costCategoryArn`
//! and `CreditData$ruleName` are both documented "Present only when
//! `creditSharingType` is `COST_CATEGORY_RULE`", so `GetCredits` reports them for
//! a credit shared by a Cost Category rule and leaves them out for every other
//! sharing type — while every other member of the same credit is reported exactly
//! as stored.
//!
//! The table spans the condition: every value of the modeled `CreditSharingType`
//! enum, a credit that records no sharing type at all, a stored sharing type
//! outside the enum, and the `COST_CATEGORY_RULE` credit that records only one of
//! the two members.

use service_com_amazonaws_billing_awsbilling::state::Credit;
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;

use crate::billing_views::{call, standalone_emulator_with_engine, ACCOUNT_ID};
use crate::credits::{credit, reported, request, seed};

/// The `CreditSharingType` values the model declares, from
/// `smithy_query(query="shape",shape="CreditSharingType")`: DEFAULT, DISABLED,
/// CUSTOM, COST_CATEGORY_RULE.
const SHARING_TYPES: &[&str] = &["DEFAULT", "DISABLED", "CUSTOM", "COST_CATEGORY_RULE"];

/// A Cost Category ARN a stored credit can carry.
const COST_CATEGORY_ARN: &str = "arn:aws:ce::000000000000:costcategory/credit-sharing";

/// A rule name within that Cost Category.
const RULE_NAME: &str = "share-with-linked";

/// One row: a stored credit, and what the two conditional members must report.
struct ScopeRow {
    case: &'static str,
    credit: Credit,
    /// The `costCategoryArn` the answer must carry, `None` for an absent member.
    expected_arn: Option<&'static str>,
    /// The `ruleName` the answer must carry, `None` for an absent member.
    expected_rule: Option<&'static str>,
}

/// A credit of [`ACCOUNT_ID`] under `sharing_type` that records BOTH conditional
/// members in the store.
fn scoped_credit(credit_id: &str, sharing_type: Option<&str>) -> Credit {
    let mut stored = credit(credit_id, ACCOUNT_ID);
    stored.credit_sharing_type = sharing_type.map(str::to_string);
    stored.cost_category_arn = Some(COST_CATEGORY_ARN.to_string());
    stored.rule_name = Some(RULE_NAME.to_string());
    stored
}

/// The rows: one per `CreditSharingType` value, plus the records whose sharing
/// type the enum does not name and the partially recorded Cost Category credit.
fn scope_rows() -> Vec<ScopeRow> {
    let mut rows = Vec::new();

    for (index, sharing_type) in SHARING_TYPES.iter().enumerate() {
        let shares_by_rule = *sharing_type == "COST_CATEGORY_RULE";
        rows.push(ScopeRow {
            case: sharing_type,
            credit: scoped_credit(&(100 + index).to_string(), Some(sharing_type)),
            expected_arn: shares_by_rule.then_some(COST_CATEGORY_ARN),
            expected_rule: shares_by_rule.then_some(RULE_NAME),
        });
    }

    rows.push(ScopeRow {
        case: "no sharing type recorded at all",
        credit: scoped_credit("200", None),
        expected_arn: None,
        expected_rule: None,
    });

    rows.push(ScopeRow {
        case: "a stored sharing type the enum does not name",
        credit: scoped_credit("201", Some("ORGANIZATION_RULE")),
        expected_arn: None,
        expected_rule: None,
    });

    let mut arn_only = scoped_credit("202", Some("COST_CATEGORY_RULE"));
    arn_only.rule_name = None;
    rows.push(ScopeRow {
        case: "shared by a Cost Category rule, no rule name recorded",
        credit: arn_only,
        expected_arn: Some(COST_CATEGORY_ARN),
        expected_rule: None,
    });

    let mut rule_only = scoped_credit("203", Some("COST_CATEGORY_RULE"));
    rule_only.cost_category_arn = None;
    rows.push(ScopeRow {
        case: "shared by a Cost Category rule, no Cost Category ARN recorded",
        credit: rule_only,
        expected_arn: None,
        expected_rule: Some(RULE_NAME),
    });

    let mut no_scope_recorded = credit("204", ACCOUNT_ID);
    no_scope_recorded.credit_sharing_type = Some("COST_CATEGORY_RULE".to_string());
    rows.push(ScopeRow {
        case: "shared by a Cost Category rule, neither member recorded",
        credit: no_scope_recorded,
        expected_arn: None,
        expected_rule: None,
    });

    rows
}

/// The period every seeded credit (which starts 30 days ago and never ends)
/// overlaps, as the request `GetCredits` is asked with.
const LOOKBACK_DAYS: u64 = 60;

#[tokio::test]
async fn cost_category_members_are_reported_only_for_a_cost_category_rule_credit() {
    let (router, engine) = standalone_emulator_with_engine();
    let rows = scope_rows();
    seed(&engine, rows.iter().map(|row| row.credit.clone()).collect());

    let (status, body) = call(&router, "GetCredits", &request(ACCOUNT_ID, LOOKBACK_DAYS)).await;
    assert_eq!(
        status, 200,
        "case \"report the account's credits\": expected 200, got {status} {body:?}"
    );

    for row in &rows {
        let case = row.case;
        let got = reported(&body, case, &row.credit.credit_id);
        assert_eq!(
            got["costCategoryArn"].as_str(),
            row.expected_arn,
            "case {case:?}: expected costCategoryArn {:?}, got {:?}",
            row.expected_arn,
            got["costCategoryArn"]
        );
        assert_eq!(
            got["ruleName"].as_str(),
            row.expected_rule,
            "case {case:?}: expected ruleName {:?}, got {:?}",
            row.expected_rule,
            got["ruleName"]
        );
        // The condition gates ONLY the two members it is documented for: the
        // sharing type itself, and every other member of the same credit, is
        // reported exactly as the store holds it.
        assert_eq!(
            got["creditSharingType"].as_str(),
            row.credit.credit_sharing_type.as_deref(),
            "case {case:?}: expected creditSharingType {:?}, got {:?}",
            row.credit.credit_sharing_type,
            got["creditSharingType"]
        );
        assert_eq!(
            got["description"].as_str(),
            Some(row.credit.description.as_str()),
            "case {case:?}: expected description {:?}, got {:?}",
            row.credit.description,
            got["description"]
        );
        assert_eq!(
            got["remainingAmount"]["currencyAmount"].as_str(),
            Some(row.credit.remaining_amount.currency_amount.as_str()),
            "case {case:?}: expected remainingAmount {:?}, got {:?}",
            row.credit.remaining_amount,
            got["remainingAmount"]
        );
    }
}

#[tokio::test]
async fn the_store_keeps_what_the_condition_withheld() {
    let (router, engine) = standalone_emulator_with_engine();
    // A credit whose record carries both members under a sharing type the
    // condition does not admit: reporting it must not consume or rewrite what
    // the store holds, so a later read answers the same, and the record is
    // reported WITH both members once its sharing type admits them.
    seed(&engine, vec![scoped_credit("300", Some("CUSTOM"))]);

    for pass in ["first read", "second read"] {
        let (status, body) =
            call(&router, "GetCredits", &request(ACCOUNT_ID, LOOKBACK_DAYS)).await;
        assert_eq!(
            status, 200,
            "case {pass:?}: expected 200, got {status} {body:?}"
        );
        let got = reported(&body, pass, "300");
        assert!(
            got["costCategoryArn"].is_null() && got["ruleName"].is_null(),
            "case {pass:?}: expected both conditional members absent, got {got:?}"
        );
    }

    let store = engine.state::<ComAmazonawsBillingAwsbillingState>();
    {
        let mut store = store.lock();
        let stored = store
            .credits
            .get_mut("300")
            .expect("case \"stored record\": the seeded credit is still in the store");
        assert_eq!(
            (stored.cost_category_arn.as_deref(), stored.rule_name.as_deref()),
            (Some(COST_CATEGORY_ARN), Some(RULE_NAME)),
            "case \"stored record\": reporting must not change what the store holds, got {stored:?}"
        );
        stored.credit_sharing_type = Some("COST_CATEGORY_RULE".to_string());
    }

    let (status, body) = call(&router, "GetCredits", &request(ACCOUNT_ID, LOOKBACK_DAYS)).await;
    assert_eq!(
        status, 200,
        "case \"sharing type now admits the members\": expected 200, got {status} {body:?}"
    );
    let got = reported(&body, "sharing type now admits the members", "300");
    assert_eq!(
        (got["costCategoryArn"].as_str(), got["ruleName"].as_str()),
        (Some(COST_CATEGORY_ARN), Some(RULE_NAME)),
        "case \"sharing type now admits the members\": expected both members reported, got {got:?}"
    );
}
