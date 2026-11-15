// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal an account-level policy put is answered with when its
//! `selectionCriteria` overlaps a policy the account already holds.
//!
//! The rule under test: that refusal states the RULE the put broke — it names the held policy
//! whose criteria the put would overlap, except where the rule broken is "no two of them can
//! use the same or overlapping log group name prefixes", whose sentence is about that prefix
//! and names no policy. (The service's one missing-resource sentence, the other refusal
//! wording whose rule is known, is covered by the table in
//! `tests/all/refusal_messages.rs`.)
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.PutAccountPolicy` through the router that mounts it — so a
//! message that is not built from the request and the stored policy cannot pass.
//!
//! The table spans what the rule generalises over: the policy types whose criteria can
//! overlap, the criteria forms that overlap (the same name, a containing prefix, a second
//! excluding list, the same data source name and type), and the held policy's name under
//! several forms.

use crate::account_policies::put_body;
use crate::account_policy_selection_criteria::document_for;
use crate::harness::Logs;

/// The code answered for a parameter the service will not accept.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A `PutAccountPolicy` whose `selectionCriteria` overlaps a policy the account already
/// holds is refused with the sentence for the RULE that was broken: naming the policy whose
/// criteria the put overlaps, and — where the rule broken is "no two of them can use the same
/// or overlapping log group name prefixes" — the fixed sentence about that prefix. The rows
/// differ along the policy type whose criteria overlap, the criteria form, and the held
/// policy's name.
#[tokio::test]
async fn an_overlapping_selection_criteria_refusal_states_the_rule_it_broke() {
    // (case, policyType, the held policy's criteria, the criteria put against it, whether the
    // rule broken is the one about overlapping log group name prefixes)
    let rows: [(&str, &str, &str, &str, bool); 6] = [
        (
            "a metric extraction policy selecting the same log group",
            "METRIC_EXTRACTION_POLICY",
            r#"LogGroupName IN ["/aws/lambda/one"]"#,
            r#"LogGroupName IN ["/aws/lambda/one"]"#,
            false,
        ),
        (
            "a metric extraction policy selecting an overlapping prefix",
            "METRIC_EXTRACTION_POLICY",
            r#"LogGroupNamePrefix IN ["/aws/lambda"]"#,
            r#"LogGroupNamePrefix IN ["/aws/lambda/api"]"#,
            false,
        ),
        (
            "a second metric extraction policy excluding log groups",
            "METRIC_EXTRACTION_POLICY",
            r#"LogGroupNamePrefix NOT IN ["/aws/lambda"]"#,
            r#"LogGroupNamePrefix NOT IN ["/aws/ecs"]"#,
            false,
        ),
        (
            "a transformer policy on the same prefix",
            "TRANSFORMER_POLICY",
            r#"LogGroupNamePrefix IN ["my-log"]"#,
            r#"LogGroupNamePrefix IN ["my-logging"]"#,
            true,
        ),
        (
            "a field index policy on the same prefix",
            "FIELD_INDEX_POLICY",
            r#"LogGroupNamePrefix IN ["shared"]"#,
            r#"LogGroupNamePrefix IN ["shared/one"]"#,
            true,
        ),
        (
            "a field index policy on the same data source name and type",
            "FIELD_INDEX_POLICY",
            r#"DataSourceName IN ["amazon_vpc"] AND DataSourceType IN ["flow"]"#,
            r#"DataSourceName IN ["amazon_vpc"] AND DataSourceType IN ["flow"]"#,
            false,
        ),
    ];
    // The held policy is named under several forms, so a message that names it is read off
    // the stored policy rather than from a fixed string.
    let held_names = ["held", "Held.Policy-1", "team/platform/policy"];

    for (case, policy_type, held_criteria, putting_criteria, by_prefix) in rows {
        for held_name in held_names {
            let logs = Logs::new();
            let document = document_for(policy_type);
            logs.ok(
                "PutAccountPolicy",
                &put_body(held_name, policy_type, &document, Some(held_criteria)),
            )
            .await;

            let (code, message) = logs
                .refusal(
                    "PutAccountPolicy",
                    &put_body("putting", policy_type, &document, Some(putting_criteria)),
                )
                .await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case} against {held_name}: expected {INVALID_PARAMETER}, got {code} / {message}"
            );
            let expected = if by_prefix {
                // The rule broken is about the PREFIXES themselves, so the sentence is about
                // the prefix and names no policy.
                "SelectionCriteria prefix is already supported by an existing policy".to_string()
            } else {
                format!(
                    "SelectionCriteria is overlapping with an existing policy with policy name: \
                     {held_name}"
                )
            };
            assert_eq!(
                message, expected,
                "case {case} against {held_name}: expected {expected:?}, got {message:?}"
            );
            if by_prefix {
                assert!(
                    !message.contains(held_name),
                    "case {case} against {held_name}: the prefix sentence names no policy, got \
                     {message:?}"
                );
            }
        }
    }
}
