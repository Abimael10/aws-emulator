// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `GetDataProtectionPolicy` answers for a log group that holds no
//! data protection policy of its own.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: the policy this read reports is the one IN FORCE on the log group —
//! the group's own where it holds one, and otherwise the account-level
//! `DATA_PROTECTION_POLICY` whose `selectionCriteria` select it, which is the policy masking
//! that group's events while it carries none of its own. The only resource the read can fail
//! to find is the LOG GROUP, so a group that holds neither is answered with a successful page
//! reporting no policy, and a log group the account does not hold is the one
//! `ResourceNotFoundException`.
//!
//! The table spans what the rule generalises over: whether the group was never put on, had
//! its policy deleted, or is selected by an account-level policy, which identifier form names
//! it, and the log group that is not there at all.

use serde_json::{json, Value};

use crate::account_policies::put_body;
use crate::harness::Logs;

/// The account-level policy type that masks sensitive data. Source: the model's `PolicyType`
/// enum (`smithy_query(query="shape", shape="PolicyType")`).
const DATA_PROTECTION_POLICY: &str = "DATA_PROTECTION_POLICY";

/// The prefix every AWS-managed data identifier ARN carries. Source: the identifier ARNs
/// listed in "Types of data that you can mask", which
/// `PutDataProtectionPolicy$policyDocument` links to for its `DataIdentifier` array.
const DATA_IDENTIFIER_PREFIX: &str = "arn:aws:dataprotection::aws:data-identifier/";

/// A data protection policy document of the form the member documents: an Audit action with a
/// findings destination, and a Deidentify action with a mask configuration, over one
/// AWS-managed data identifier.
fn document(name: &str, identifier_name: &str) -> String {
    let identifiers = json!([format!("{DATA_IDENTIFIER_PREFIX}{identifier_name}")]);
    json!({
        "Name": name,
        "Version": "2021-06-01",
        "Statement": [
            {
                "Sid": "audit",
                "DataIdentifier": identifiers,
                "Operation": {"Audit": {"FindingsDestination": {}}},
            },
            {
                "Sid": "mask",
                "DataIdentifier": identifiers,
                "Operation": {"Deidentify": {"MaskConfig": {}}},
            },
        ],
    })
    .to_string()
}

/// The `policyDocument` one read reports, or `None` for a page that reports no policy.
async fn read_policy(logs: &Logs, case: &str, identifier: &str) -> Option<String> {
    let got = logs
        .ok(
            "GetDataProtectionPolicy",
            &json!({"logGroupIdentifier": identifier}),
        )
        .await;
    let reported = got
        .get("policyDocument")
        .and_then(Value::as_str)
        .map(str::to_string);
    if reported.is_none() {
        assert_eq!(
            got.get("lastUpdatedTime"),
            None,
            "case {case:?}: a page reporting no policy reports no update time either, got {got}"
        );
    }
    reported
}

/// RULE: a log group that holds no policy of its own and is selected by no account-level one
/// is answered with a page reporting no policy — never a missing resource — however it came
/// to hold none, and whichever identifier form names it.
#[tokio::test]
async fn a_group_holding_no_policy_is_answered_with_no_policy() {
    for selector in ["name", "arn"] {
        let logs = Logs::new();
        let never = format!("/data-protection/never-put-by-{selector}");
        let deleted = format!("/data-protection/deleted-by-{selector}");
        for group in [&never, &deleted] {
            logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
                .await;
        }
        let never_identifier = if selector == "arn" {
            logs.group_arns(&never).await.1
        } else {
            never.clone()
        };
        let case = format!("a group nobody put on, by {selector}");
        let reported = read_policy(&logs, &case, &never_identifier).await;
        assert_eq!(
            reported, None,
            "case {case:?}: expected no policy reported, got {reported:?}"
        );

        // A group whose policy was DELETED answers the same way: the policy is gone, and the
        // group is still there.
        let deleted_identifier = if selector == "arn" {
            logs.group_arns(&deleted).await.1
        } else {
            deleted.clone()
        };
        let stored = document("deleted-later", "EmailAddress");
        logs.ok(
            "PutDataProtectionPolicy",
            &json!({"logGroupIdentifier": deleted_identifier, "policyDocument": stored}),
        )
        .await;
        let case = format!("a group holding its own policy, by {selector}");
        let reported = read_policy(&logs, &case, &deleted_identifier).await;
        assert_eq!(
            reported.as_deref(),
            Some(stored.as_str()),
            "case {case:?}: expected the stored document, got {reported:?}"
        );
        logs.ok(
            "DeleteDataProtectionPolicy",
            &json!({"logGroupIdentifier": deleted_identifier}),
        )
        .await;
        let case = format!("a group whose policy was deleted, by {selector}");
        let reported = read_policy(&logs, &case, &deleted_identifier).await;
        assert_eq!(
            reported, None,
            "case {case:?}: expected no policy reported, got {reported:?}"
        );
        // The read changed nothing: a second read answers the same, and a put after it is
        // reported again.
        let reported = read_policy(&logs, &case, &deleted_identifier).await;
        assert_eq!(
            reported, None,
            "case {case:?}: a second read answers the same, got {reported:?}"
        );
        let again = document("put-again", "Name");
        logs.ok(
            "PutDataProtectionPolicy",
            &json!({"logGroupIdentifier": deleted_identifier, "policyDocument": again}),
        )
        .await;
        let reported = read_policy(&logs, &case, &deleted_identifier).await;
        assert_eq!(
            reported.as_deref(),
            Some(again.as_str()),
            "case {case:?}: expected the re-put document, got {reported:?}"
        );
    }
}

/// RULE: the policy reported for a group that holds none of its own is the ACCOUNT-level one
/// that selects it — and the group's own overrides it where it has one.
#[tokio::test]
async fn an_account_level_policy_is_reported_for_the_groups_it_selects() {
    let logs = Logs::new();
    let selected = "/data-protection/selected";
    let elsewhere = "app/elsewhere";
    for group in [selected, elsewhere] {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
    }
    let account_level = document("account-level", "EmailAddress");
    logs.ok(
        "PutAccountPolicy",
        &put_body(
            "scoped-to-data-protection",
            DATA_PROTECTION_POLICY,
            &account_level,
            None,
        ),
    )
    .await;

    for group in [selected, elsewhere] {
        let case = format!("a group selected by the account-level policy ({group})");
        let reported = read_policy(&logs, &case, group).await;
        assert_eq!(
            reported.as_deref(),
            Some(account_level.as_str()),
            "case {case:?}: expected the account-level document, got {reported:?}"
        );
    }

    // The group's OWN policy is what is reported once it has one.
    let own = document("the group's own", "Name");
    logs.ok(
        "PutDataProtectionPolicy",
        &json!({"logGroupIdentifier": selected, "policyDocument": own}),
    )
    .await;
    let case = "a group holding its own policy beside the account-level one";
    let reported = read_policy(&logs, case, selected).await;
    assert_eq!(
        reported.as_deref(),
        Some(own.as_str()),
        "case {case:?}: expected the group's own document, got {reported:?}"
    );
}

/// RULE: the one resource this read can fail to find is the LOG GROUP, which is refused
/// naming the group that was looked for and the account that would own it.
#[tokio::test]
async fn a_log_group_the_account_does_not_hold_is_the_one_missing_resource() {
    let logs = Logs::new();
    for group in ["/data-protection/never-created", "NeverCreated"] {
        let (code, message) = logs
            .refusal(
                "GetDataProtectionPolicy",
                &json!({"logGroupIdentifier": group}),
            )
            .await;
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {group:?}: expected ResourceNotFoundException, got {code} ({message})"
        );
        let expected = format!(
            "Log group '{group}' for owner '{}' cannot be found",
            engine::emulator_account_id()
        );
        assert_eq!(
            message, expected,
            "case {group:?}: expected {expected:?}, got {message:?}"
        );
    }
}
