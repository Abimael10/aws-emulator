// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the OPERATOR a `PutAccountPolicy` selection filter is written with.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: a `selectionCriteria` filter states its field, an operator and the
//! values it selects on, and the operator is either a set operator over a list (`IN`,
//! `NOT IN` — the lists `PutAccountPolicy$selectionCriteria` bounds at "up to 50 values in
//! each IN or NOT IN list") or EQUALITY on a single value (`LogGroupNamePrefix =
//! "my-prefix"`). The two select the same log groups when they carry the same value, so a
//! policy put with either is accepted where that field is supported, selects the same groups,
//! and conflicts with another policy under the same overlap rule — while the criteria string
//! itself is echoed and stored exactly as it was written.
//!
//! The tables span what the rule generalises over: which operator the filter is written with,
//! which policy type it is put with (so a field the type does not support is still refused
//! whichever operator writes it), which log groups the value then selects, and which way an
//! equality is written wrongly.

use serde_json::{json, Value};

use crate::account_policies::{
    described_names, field_index_document, member, put_body, put_policy, transformer_document,
};
use crate::harness::Logs;

/// The account-level policy type whose criteria select by name prefix and whose effect on one
/// log group is readable back through `DescribeIndexPolicies`. Source: the model's
/// `PolicyType` enum (`smithy_query(query="shape", shape="PolicyType")`).
const FIELD_INDEX_POLICY: &str = "FIELD_INDEX_POLICY";

/// The account-level policy type whose only supported filter is `LogGroupNamePrefix`. Source:
/// `PutAccountPolicy$selectionCriteria` — "If policyType is TRANSFORMER_POLICY, the only
/// supported selectionCriteria filter is LogGroupNamePrefix".
const TRANSFORMER_POLICY: &str = "TRANSFORMER_POLICY";

/// The account-level policy type whose only supported filter is `LogGroupName NOT IN []`. See
/// [`TRANSFORMER_POLICY`] for the sentence that states each type's filters.
const SUBSCRIPTION_FILTER_POLICY: &str = "SUBSCRIPTION_FILTER_POLICY";

/// A `policyDocument` each type admits, so a case about the criteria is decided by the
/// criteria alone.
fn document_for(policy_type: &str) -> String {
    match policy_type {
        TRANSFORMER_POLICY => transformer_document(),
        _ => field_index_document(&["requestId"]),
    }
}

/// The index policies `DescribeIndexPolicies` reports for one log group.
async fn index_policies(logs: &Logs, group: &str) -> Vec<Value> {
    let page = logs
        .ok(
            "DescribeIndexPolicies",
            &json!({"logGroupIdentifiers": [group]}),
        )
        .await;
    page.get("indexPolicies")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {group}: a page reports indexPolicies, got {page}"))
        .clone()
}

/// RULE: a prefix filter written as an equality is accepted wherever the same filter written
/// over a list is, and the criteria string is echoed and stored exactly as written.
#[tokio::test]
async fn a_prefix_filter_may_be_written_as_an_equality() {
    // (case, the policy type, the criteria string)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a transformer policy scoped by an equality",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix = "/aws/transform""#,
        ),
        (
            "a transformer policy scoped by a list",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix IN ["/aws/transform"]"#,
        ),
        (
            "a field index policy scoped by an equality",
            FIELD_INDEX_POLICY,
            r#"LogGroupNamePrefix = "/aws/index""#,
        ),
        (
            "an equality written without spaces",
            FIELD_INDEX_POLICY,
            r#"LogGroupNamePrefix="/aws/index""#,
        ),
        (
            "an equality whose value is a whole log group name",
            FIELD_INDEX_POLICY,
            r#"LogGroupNamePrefix = "/aws/index/one""#,
        ),
    ];
    for (case, policy_type, criteria) in rows {
        // One account per row, so what the reader reports is this policy's.
        let logs = Logs::new();
        let name = "scoped-policy";
        let put = put_policy(
            &logs
                .ok(
                    "PutAccountPolicy",
                    &put_body(name, policy_type, &document_for(policy_type), Some(criteria)),
                )
                .await,
        );
        assert_eq!(
            member(&put, "selectionCriteria"),
            criteria,
            "case {case:?}: expected the criteria echoed verbatim, got {put}"
        );
        let described = described_names(&logs, &json!({"policyType": policy_type})).await;
        assert_eq!(
            described,
            vec![name.to_string()],
            "case {case:?}: expected the reader to report the scoped policy, got {described:?}"
        );
    }
}

/// RULE: the two operators select the SAME log groups when they carry the same value, which
/// is what the policy's effect on a group is decided by.
#[tokio::test]
async fn an_equality_selects_the_same_groups_as_the_one_value_list() {
    let selected = "/aws/index/one";
    let other = "app/two";
    // (case, the criteria the policy is scoped by)
    for (case, criteria) in [
        ("an equality", r#"LogGroupNamePrefix = "/aws/index""#),
        ("a one-value list", r#"LogGroupNamePrefix IN ["/aws/index"]"#),
    ] {
        let logs = Logs::new();
        for group in [selected, other] {
            logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
                .await;
        }
        let document = field_index_document(&["requestId"]);
        logs.ok(
            "PutAccountPolicy",
            &put_body("by-prefix", FIELD_INDEX_POLICY, &document, Some(criteria)),
        )
        .await;

        let reported = index_policies(&logs, selected).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the policy to select {selected}, got {reported:?}"
        );
        assert_eq!(
            member(&reported[0], "policyDocument"),
            document,
            "case {case:?}: expected the policy's own document, got {:?}",
            reported[0]
        );
        let unselected = index_policies(&logs, other).await;
        assert!(
            unselected.is_empty(),
            "case {case:?}: expected {other} not to be selected, got {unselected:?}"
        );
    }
}

/// RULE: the overlap rule is about the log groups two policies select, not about the strings
/// their criteria were written as — "no two of them can use the same or overlapping log group
/// name prefixes" — so a second policy is refused whichever operator either one used.
#[tokio::test]
async fn an_overlapping_prefix_is_refused_whichever_operator_wrote_it() {
    // (case, the first policy's criteria, the second policy's criteria, whether the second
    // overlaps the first)
    let rows: Vec<(&str, &str, &str, bool)> = vec![
        (
            "the same prefix, both as equalities",
            r#"LogGroupNamePrefix = "/aws/same""#,
            r#"LogGroupNamePrefix = "/aws/same""#,
            true,
        ),
        (
            "an equality over a prefix a list already took",
            r#"LogGroupNamePrefix IN ["/aws/same"]"#,
            r#"LogGroupNamePrefix = "/aws/same/narrower""#,
            true,
        ),
        (
            "a list over a prefix an equality already took",
            r#"LogGroupNamePrefix = "/aws/same""#,
            r#"LogGroupNamePrefix IN ["/aws/same"]"#,
            true,
        ),
        (
            "a disjoint prefix as an equality",
            r#"LogGroupNamePrefix = "/aws/first""#,
            r#"LogGroupNamePrefix = "/aws/second""#,
            false,
        ),
    ];
    for (case, first, second, overlaps) in rows {
        let logs = Logs::new();
        let document = transformer_document();
        logs.ok(
            "PutAccountPolicy",
            &put_body("first-policy", TRANSFORMER_POLICY, &document, Some(first)),
        )
        .await;
        let body = put_body("second-policy", TRANSFORMER_POLICY, &document, Some(second));
        if overlaps {
            let (code, message) = logs.refusal("PutAccountPolicy", &body).await;
            assert_eq!(
                code, "InvalidParameterException",
                "case {case:?}: expected the overlap to be refused, got {code} ({message})"
            );
            // The rule broken is "no two of them can use the same or overlapping log group
            // name prefixes", whose sentence is about the prefix itself.
            let expected = "SelectionCriteria prefix is already supported by an existing policy";
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            let held = described_names(&logs, &json!({"policyType": TRANSFORMER_POLICY})).await;
            assert_eq!(
                held,
                vec!["first-policy".to_string()],
                "case {case:?}: a refused put stores nothing, got {held:?}"
            );
            continue;
        }
        logs.ok("PutAccountPolicy", &body).await;
        let mut held = described_names(&logs, &json!({"policyType": TRANSFORMER_POLICY})).await;
        held.sort();
        assert_eq!(
            held,
            vec!["first-policy".to_string(), "second-policy".to_string()],
            "case {case:?}: expected both policies to be held, got {held:?}"
        );
    }
}

/// RULE: the operator does not widen which FIELDS a policy type supports, and a string that
/// is not a filter at all is still refused however it is punctuated.
#[tokio::test]
async fn an_equality_is_still_held_to_the_grammar_and_the_type() {
    let logs = Logs::new();
    // (case, the policy type, the criteria the put is refused for)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a subscription filter policy, whose only filter is LogGroupName NOT IN []",
            SUBSCRIPTION_FILTER_POLICY,
            r#"LogGroupNamePrefix = "/aws/sink""#,
        ),
        (
            "a transformer policy scoped by an exact name",
            TRANSFORMER_POLICY,
            r#"LogGroupName = "group-one""#,
        ),
        (
            "an equality whose value is not quoted",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix = /aws/transform"#,
        ),
        (
            "an equality with no value at all",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix ="#,
        ),
        (
            "an equality with no field",
            TRANSFORMER_POLICY,
            r#"= "/aws/transform""#,
        ),
        (
            "an equality whose value is never closed",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix = "/aws/transform"#,
        ),
        (
            "two words before the operator",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix Prefix = "/aws/transform""#,
        ),
        (
            "a doubled operator",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix == "/aws/transform""#,
        ),
        (
            "an equality joined to a second filter by something else",
            TRANSFORMER_POLICY,
            r#"LogGroupNamePrefix = "/aws/a" OR LogGroupNamePrefix = "/aws/b""#,
        ),
    ];
    for (case, policy_type, criteria) in rows {
        let body = put_body(case, policy_type, &document_for(policy_type), Some(criteria));
        let (code, message) = logs.refusal("PutAccountPolicy", &body).await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected the criteria to be refused, got {code} ({message})"
        );
        let held = described_names(&logs, &json!({"policyType": policy_type})).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused put stores nothing, got {held:?}"
        );
    }
}
