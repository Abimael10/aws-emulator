// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SPELLING a resource-policy read reports a principal
//! in: a policy store holds a whole account named by its bare account id as that
//! account's root ARN, and leaves every other value and every other byte of the
//! submitted document as the caller wrote them.
//!
//! The action rule (which actions a statement may grant) lives in
//! `resource_policy_action_subsets`, and the per-kind lifecycle in
//! `resource_policies` and `rule_group_capacity_and_resource_policy`; these tables
//! vary the PRINCIPAL across the class the spelling rule covers.
//!
//! Every test drives the service the way a client does, through the shared
//! [`Harness`] in `crate::harness`: an awsJson1_0 `POST /` with
//! `X-Amz-Target: NetworkFirewall_20201112.<OperationName>`, so what is asserted
//! is the WIRE behaviour and not a handler's return value.

use serde_json::json;
use service_com_amazonaws_networkfirewall_networkfirewall_20201112::state;

use crate::harness::{error_code, Harness, ACCOUNT};

/// An account id that is not this emulator's own, so a row proves the spelling is
/// built from the value the statement names rather than from the caller's account.
const OTHER_ACCOUNT: &str = "210987654321";

/// A policy document whose statements carry `members` verbatim — one statement per
/// entry — each granting `actions` on `resource`.
///
/// The statements' leading members are written out as TEXT so a row controls every
/// byte of them: the spelling of the principal, the member order, the list form and
/// the whitespace between the tokens included.
fn document(members: &[&str], actions: &[&str], resource: &str) -> String {
    let actions: Vec<String> = actions.iter().map(|action| format!("\"{action}\"")).collect();
    let actions = actions.join(",");
    let statements: Vec<String> = members
        .iter()
        .map(|members| format!("{{{members},\"Action\":[{actions}],\"Resource\":\"{resource}\"}}"))
        .collect();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        statements.join(",")
    )
}

/// One row: what a failing assertion calls it, the statement members the caller
/// submits, and the ones the read reports.
type Row = (String, Vec<String>, Vec<String>);

/// The rows the spelling rule covers, for the principal forms the policy language
/// defines ([`state`]'s documented `AWS`, `Service` and untyped-wildcard forms, as
/// the AWS policy language lists the principal types) plus the ways a statement
/// writes them: alone, in a list, beside another principal type, and beside a
/// member that merely LOOKS like an account id.
fn rows() -> Vec<Row> {
    let root = format!("arn:aws:iam::{ACCOUNT}:root");
    let other_root = format!("arn:aws:iam::{OTHER_ACCOUNT}:root");
    let cases: [(&str, String, String); 13] = [
        (
            "a bare account id",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}}"),
        ),
        (
            "another account's bare id",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{OTHER_ACCOUNT}\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{other_root}\"}}"),
        ),
        (
            "a root ARN already",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}}"),
        ),
        (
            "the wildcard under the AWS type",
            "\"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"*\"}".to_string(),
            "\"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"*\"}".to_string(),
        ),
        (
            "the untyped wildcard shorthand",
            "\"Effect\":\"Allow\",\"Principal\":\"*\"".to_string(),
            "\"Effect\":\"Allow\",\"Principal\":\"*\"".to_string(),
        ),
        (
            "an IAM user's ARN",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT}:user/sharer\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT}:user/sharer\"}}"),
        ),
        (
            "an IAM role's ARN with a path",
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT}:role/service-role/share\"}}"
            ),
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{ACCOUNT}:role/service-role/share\"}}"
            ),
        ),
        (
            "an STS assumed-role session",
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:sts::{ACCOUNT}:assumed-role/share/session\"}}"
            ),
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:sts::{ACCOUNT}:assumed-role/share/session\"}}"
            ),
        ),
        (
            "a list of a bare id and an ARN",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"{OTHER_ACCOUNT}\",\"{root}\"]}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"{other_root}\",\"{root}\"]}}"),
        ),
        (
            "a one-element list of a bare id, whose list form survives",
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"{ACCOUNT}\"]}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"{root}\"]}}"),
        ),
        (
            "a service principal, which is another principal type",
            "\"Effect\":\"Allow\",\"Principal\":{\"Service\":\"ram.amazonaws.com\"}".to_string(),
            "\"Effect\":\"Allow\",\"Principal\":{\"Service\":\"ram.amazonaws.com\"}".to_string(),
        ),
        (
            "an account id beside a service principal",
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\",\"Service\":\"ram.amazonaws.com\"}}"
            ),
            format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\",\"Service\":\"ram.amazonaws.com\"}}"
            ),
        ),
        (
            "a Sid of twelve digits, which is no principal",
            format!(
                "\"Sid\":\"{OTHER_ACCOUNT}\", \"Effect\": \"Allow\", \"Principal\": {{ \"AWS\": \"{ACCOUNT}\" }}"
            ),
            format!(
                "\"Sid\":\"{OTHER_ACCOUNT}\", \"Effect\": \"Allow\", \"Principal\": {{ \"AWS\": \"{root}\" }}"
            ),
        ),
    ];

    let mut rows: Vec<Row> = cases
        .into_iter()
        .map(|(label, submitted, reported)| {
            (label.to_string(), vec![submitted], vec![reported])
        })
        .collect();
    // Two statements at once: every statement's principal is read, not just the
    // first, and a statement needing no rewrite is left alone beside one that does.
    rows.push((
        "two statements, one of them already canonical".to_string(),
        vec![
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{OTHER_ACCOUNT}\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}}"),
        ],
        vec![
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{other_root}\"}}"),
            format!("\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{root}\"}}"),
        ],
    ));
    rows
}

/// RULE: the resource-policy document a read reports is the submitted document with
/// every `AWS` principal value that names a whole account by its bare account id
/// written as that account's root principal ARN (`arn:aws:iam::<account id>:root`),
/// and every other value and every other byte — the other principal types, the
/// wildcard, an identity's ARN, a root ARN already, the member order, the list form
/// and the whitespace between the tokens — exactly as the caller submitted them.
///
/// The rows vary the principal's FORM, the element it sits in and how many values
/// it names; both kinds of shareable resource are driven with the action set their
/// own type permits, so the spelling is the store's and not one resource kind's.
#[tokio::test]
async fn an_account_principal_reads_back_as_that_accounts_root_arn() {
    let harness = Harness::new();
    let kinds: [(&str, &[&str], String); 2] = [
        (
            "rule-group",
            &state::RULE_GROUP_POLICY_ACTIONS,
            harness
                .shared_rule_group("principal-spelling", "STATEFUL", 10)
                .await,
        ),
        (
            "firewall-policy",
            &state::FIREWALL_POLICY_POLICY_ACTIONS,
            harness.shared_policy("principal-spelling").await,
        ),
    ];

    for (kind, actions, arn) in kinds {
        for (label, submitted, reported) in rows() {
            let case = format!("{kind}/{label}");
            let submitted: Vec<&str> = submitted.iter().map(String::as_str).collect();
            let reported: Vec<&str> = reported.iter().map(String::as_str).collect();
            let submitted = document(&submitted, actions, &arn);
            let expected = document(&reported, actions, &arn);

            // Start each row from "nothing attached", so it is judged on its own.
            let _ = harness
                .call("DeleteResourcePolicy", &json!({ "ResourceArn": arn }))
                .await;
            let (status, body) = harness
                .call(
                    "PutResourcePolicy",
                    &json!({ "ResourceArn": arn, "Policy": submitted }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: the document must be accepted, got {status} {body:?}"
            );

            let described = harness
                .ok("DescribeResourcePolicy", &json!({ "ResourceArn": arn }))
                .await;
            let actual = described["Policy"].as_str().unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );

            // The spelling is a FIXED POINT: putting back what was read stores the
            // same bytes again, so a client that re-puts the document it read never
            // sees it change.
            harness
                .ok(
                    "PutResourcePolicy",
                    &json!({ "ResourceArn": arn, "Policy": expected }),
                )
                .await;
            let described = harness
                .ok("DescribeResourcePolicy", &json!({ "ResourceArn": arn }))
                .await;
            let actual = described["Policy"].as_str().unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case}: re-putting the reported document must change nothing, got \
                 {actual:?}"
            );
        }
    }
}

/// RULE: the spelling is a property of the STORE, so a read reports it however the
/// document reached the store — and a refused document stores nothing at all, so no
/// rewritten text survives a refusal.
///
/// The rows are the ways a document carrying an account principal reaches
/// `PutResourcePolicy`: as pretty-printed text a caller indented, and with an
/// action outside the resource type's permitted set, which is refused.
#[tokio::test]
async fn the_spelling_is_applied_to_indented_text_and_nothing_is_stored_on_a_refusal() {
    let harness = Harness::new();
    let arn = harness
        .shared_rule_group("principal-spelling-text", "STATEFUL", 10)
        .await;
    let root = format!("arn:aws:iam::{ACCOUNT}:root");
    let action = state::RULE_GROUP_POLICY_ACTIONS[0];

    let indented = format!(
        "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \"Effect\": \
         \"Allow\",\n      \"Principal\": {{\n        \"AWS\": \"{ACCOUNT}\"\n      }},\n      \
         \"Action\": [\n        \"{action}\"\n      ],\n      \"Resource\": \"{arn}\"\n    }}\n  \
         ]\n}}"
    );
    let expected = indented.replace(&format!("\"{ACCOUNT}\""), &format!("\"{root}\""));
    harness
        .ok(
            "PutResourcePolicy",
            &json!({ "ResourceArn": arn, "Policy": indented }),
        )
        .await;
    let described = harness
        .ok("DescribeResourcePolicy", &json!({ "ResourceArn": arn }))
        .await;
    let actual = described["Policy"].as_str().unwrap_or_default();
    assert_eq!(
        actual, expected,
        "case indented-text: expected {expected:?}, got {actual:?}"
    );

    harness
        .ok("DeleteResourcePolicy", &json!({ "ResourceArn": arn }))
        .await;
    let refused = document(
        &[&format!(
            "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}}"
        )],
        &["network-firewall:DeleteRuleGroup"],
        &arn,
    );
    let (status, body) = harness
        .call(
            "PutResourcePolicy",
            &json!({ "ResourceArn": arn, "Policy": refused }),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidResourcePolicyException"),
        "case out-of-set-action: expected InvalidResourcePolicyException, got {status} {body:?}"
    );
    let (status, body) = harness
        .call("DescribeResourcePolicy", &json!({ "ResourceArn": arn }))
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ResourceNotFoundException"),
        "case out-of-set-action: a refused document stores nothing, got {status} {body:?}"
    );
}
