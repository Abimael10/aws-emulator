// SPDX-License-Identifier: CC0-1.0

//! The one refusal an account list earns for naming the SAME account twice, driven end
//! to end over the service's real `restJson1` wire through
//! [`MultiServiceRouter::dispatch`] from EVERY operation that takes an account list.
//!
//! RULE under test: an account list that names one account more than once is refused
//! identically whichever operation carried it — the same status, the same
//! `ValidationExceptionReason` and the same sentence, built from the repeated id — and
//! the only thing that varies with the operation is the input member named in `Fields`,
//! which is the member the model declares for the list that operation takes
//! (`smithy_query(query="operation",operation="AssociateAccounts,DisassociateAccounts,CreateBillingGroup")`:
//! `AccountIds`, `AccountIds`, `AccountGrouping.LinkedAccountIds`).
//!
//! The rows span the class rather than one call: all three operations that take an
//! account list, both declared member paths, lists of two and of three ids, the repeat
//! at the front and in the middle, an id repeated three times, and ids that are and are
//! not members of the group addressed.

use crate::tagging_surface::{error_code, Emulator};
use serde_json::{json, Value};

/// The status the modelled `ValidationException` is served on
/// (`smithy_query(query="shape",shape="ValidationException")`: `httpError` 400).
const VALIDATION_STATUS: u16 = 400;

/// The reason `ValidationExceptionReason` enumerates for an account named twice in one
/// request (`smithy_query(query="shape",shape="ValidationExceptionReason")`).
const DUPLICATE_ACCOUNT: &str = "DUPLICATE_ACCOUNT";

/// The member a refusal names in `Fields`, or `None` when it names none.
fn named_member(body: &Value) -> Option<&str> {
    body.get("Fields")?
        .as_array()?
        .first()?
        .get("Name")?
        .as_str()
}

/// One refusal's sentence with the repeated account id blanked out, so two answers about
/// two different ids are comparable as the TEMPLATE each was built from.
fn message_template(body: &Value, account_id: &str) -> String {
    body["Message"]
        .as_str()
        .unwrap_or_default()
        .replace(account_id, "<account>")
}

/// One row: the case, the request, the declared member the operation's list travels in,
/// and the account id the list repeats.
struct Row {
    case: &'static str,
    path: &'static str,
    member: &'static str,
    duplicate: &'static str,
    body: Value,
}

/// RULE: every operation that takes an account list refuses a repeated id the same way —
/// one status, one reason, one sentence — naming its own declared input member.
#[tokio::test]
async fn every_operation_taking_an_account_list_refuses_a_repeat_the_same_way() {
    let em = Emulator::new();
    let plan = em.create_plan("DuplicateAccountPlan", None).await;
    // The harness's group holds `111122223333` as its primary and only linked account,
    // so the rows below cover both a repeat of a MEMBER and a repeat of a non-member.
    let group = em.create_group("DuplicateAccountGroup", &plan, None).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "AssociateAccounts repeats a non-member twice",
            path: "/associate-accounts",
            member: "AccountIds",
            duplicate: "222233334444",
            body: json!({
                "Arn": group.clone(),
                "AccountIds": ["222233334444", "222233334444"],
            }),
        },
        Row {
            case: "AssociateAccounts repeats an id among others",
            path: "/associate-accounts",
            member: "AccountIds",
            duplicate: "333344445555",
            body: json!({
                "Arn": group.clone(),
                "AccountIds": ["333344445555", "444455556666", "333344445555"],
            }),
        },
        Row {
            case: "DisassociateAccounts repeats the group's own member twice",
            path: "/disassociate-accounts",
            member: "AccountIds",
            duplicate: "111122223333",
            body: json!({
                "Arn": group.clone(),
                "AccountIds": ["111122223333", "111122223333"],
            }),
        },
        Row {
            case: "DisassociateAccounts repeats a non-member three times",
            path: "/disassociate-accounts",
            member: "AccountIds",
            duplicate: "555566667777",
            body: json!({
                "Arn": group.clone(),
                "AccountIds": ["555566667777", "555566667777", "555566667777"],
            }),
        },
        Row {
            case: "CreateBillingGroup repeats a linked account twice",
            path: "/create-billing-group",
            member: "AccountGrouping.LinkedAccountIds",
            duplicate: "666677778888",
            body: json!({
                "Name": "DuplicateAccountRefusedGroup",
                "AccountGrouping": {"LinkedAccountIds": ["666677778888", "666677778888"]},
                "ComputationPreference": {"PricingPlanArn": plan.clone()},
            }),
        },
        Row {
            case: "CreateBillingGroup repeats a linked account among others",
            path: "/create-billing-group",
            member: "AccountGrouping.LinkedAccountIds",
            duplicate: "777788889999",
            body: json!({
                "Name": "DuplicateAccountRefusedGroupTwo",
                "AccountGrouping": {
                    "LinkedAccountIds": ["777788889999", "888899990000", "777788889999"],
                },
                "ComputationPreference": {"PricingPlanArn": plan.clone()},
            }),
        },
    ];

    // (case, the template the refusal was built from) for every row, compared at the end.
    let mut templates: Vec<(&str, String)> = Vec::new();
    for row in &rows {
        let case = row.case;
        let (status, body) = em.call("POST", row.path, Some(&row.body)).await;
        assert_eq!(
            status, VALIDATION_STATUS,
            "case {case:?}: expected HTTP {VALIDATION_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
        assert_eq!(
            body["Reason"].as_str(),
            Some(DUPLICATE_ACCOUNT),
            "case {case:?}: expected the {DUPLICATE_ACCOUNT} reason, got {body}"
        );
        assert_eq!(
            named_member(&body),
            Some(row.member),
            "case {case:?}: expected Fields to name {:?}, got {body}",
            row.member
        );
        let message = body["Message"].as_str().unwrap_or_default().to_string();
        assert!(
            message.contains(row.duplicate),
            "case {case:?}: expected the sentence to state the repeated id {:?}, got {message:?}",
            row.duplicate
        );
        templates.push((case, message_template(&body, row.duplicate)));
    }

    // The one thing the shared refusal buys: whatever the wording is, every operation
    // states it, so none of the three can drift from the others.
    let (first_case, first_template) = templates
        .first()
        .cloned()
        .expect("the table has at least one row");
    for (case, template) in &templates {
        assert_eq!(
            template, &first_template,
            "case {case:?}: expected the same sentence {first_case:?} gave \
             ({first_template:?}), got {template:?}"
        );
    }
}

/// RULE, where it bites on state: a refused list changes nothing — the group addressed
/// keeps its membership and no billing group is created for a refused create.
#[tokio::test]
async fn a_repeated_account_id_leaves_state_untouched() {
    let em = Emulator::new();
    let plan = em.create_plan("DuplicateAccountStatePlan", None).await;
    let group = em.create_group("DuplicateAccountStateGroup", &plan, None).await;

    let before = members_of(&em, "before the refusals", &group).await;
    assert_eq!(
        before,
        vec!["111122223333".to_string()],
        "setup: expected the group to hold its one linked account, got {before:?}"
    );
    let groups_before = billing_group_count(&em, "before the refusals").await;

    let refusals: Vec<(&str, &str, Value)> = vec![
        (
            "AssociateAccounts with a repeat",
            "/associate-accounts",
            json!({
                "Arn": group.clone(),
                "AccountIds": ["222233334444", "222233334444"],
            }),
        ),
        (
            "DisassociateAccounts with a repeat",
            "/disassociate-accounts",
            json!({
                "Arn": group.clone(),
                "AccountIds": ["111122223333", "111122223333"],
            }),
        ),
        (
            "CreateBillingGroup with a repeat",
            "/create-billing-group",
            json!({
                "Name": "DuplicateAccountStateRefusedGroup",
                "AccountGrouping": {"LinkedAccountIds": ["333344445555", "333344445555"]},
                "ComputationPreference": {"PricingPlanArn": plan.clone()},
            }),
        ),
    ];
    for (case, path, body) in &refusals {
        let (status, answered) = em.call("POST", path, Some(body)).await;
        assert_eq!(
            status, VALIDATION_STATUS,
            "case {case:?}: expected HTTP {VALIDATION_STATUS}, got {status} with {answered}"
        );
    }

    let after = members_of(&em, "after the refusals", &group).await;
    assert_eq!(
        after, before,
        "expected the refused calls to leave the membership alone: it was {before:?}, \
         it is now {after:?}"
    );
    let groups_after = billing_group_count(&em, "after the refusals").await;
    assert_eq!(
        groups_after, groups_before,
        "expected the refused create to store no billing group: there were \
         {groups_before} groups, there are now {groups_after}"
    );
}

/// The accounts `ListAccountAssociations` reports for `group_arn`, sorted — the read that
/// proves the membership out of state rather than out of the write's own answer.
async fn members_of(em: &Emulator, case: &str, group_arn: &str) -> Vec<String> {
    let (status, body) = em
        .call(
            "POST",
            "/list-account-associations",
            Some(&json!({"Filters": {"Association": group_arn}})),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListAccountAssociations expected 200, got {status} with {body}"
    );
    let mut accounts: Vec<String> = body["LinkedAccounts"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no LinkedAccounts in {body}"))
        .iter()
        .map(|entry| entry["AccountId"].as_str().unwrap_or_default().to_string())
        .collect();
    accounts.sort();
    accounts
}

/// How many billing groups `ListBillingGroups` reports.
async fn billing_group_count(em: &Emulator, case: &str) -> usize {
    let (status, body) = em
        .call("POST", "/list-billing-groups", Some(&json!({})))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBillingGroups expected 200, got {status} with {body}"
    );
    body["BillingGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no BillingGroups in {body}"))
        .len()
}
