// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TRUST POLICY of the role a `Task` state's `Credentials` names,
//! driven over all three services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions, IAM and DynamoDB mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — so the documents the assume is judged
//! against are the ones IAM's own handlers wrote and the far-end effect is read back
//! through DynamoDB's own API.
//!
//! RULE: a role is assumed only where its OWN trust policy admits the identity making the
//! assume. For a `Task` state's `Credentials` that identity is the state machine's
//! execution role, so a target role whose trust policy does not cover it is a role this
//! service may not take on however much the execution role's identity-based policies grant:
//! the task FAILS with `States.TaskFailed` and the service's own sentence naming both roles,
//! and the integration call is not made. A trust statement that names the execution role
//! directly is enough on its own; one that names only its ACCOUNT delegates to that
//! account's identity-based policies, so the assume then needs a grant among them too.
//!
//! The rows differ along what the rule generalises over: who the trust policy's `Principal`
//! names (the execution role, its account's root, an unrelated role, this service's own
//! principal), the `Effect` it states, whether a `Condition` narrows it to something the
//! assume does not carry, and whether the execution role holds the assume grant at all.
//!
//! What a `Credentials.RoleArn` is WRITTEN as, and whose policies then decide the call, is
//! `tests/all/task_credentials_role.rs`, whose harness and helpers this suite drives its
//! rows through — `tests/all.rs` is one crate root.

use serde_json::{json, Value};

use crate::task_credentials_role::{
    definition, grants, trusts_the_role, trusts_the_service, unassumable_cause, Chain,
    ASSUME_ACTION, TABLE, TASK_FAILED_ERROR,
};

/// One row: the case, the trust document the target role carries, and whether the assume it
/// admits is expected to happen.
struct Row {
    case: &'static str,
    trust: Value,
    /// Whether the `Task` is expected to run its call under the target role.
    assumed: bool,
}

/// The trust documents a target role can carry, and what each means for the assume.
///
/// `execution_role` is the identity the assume is made as, and `unrelated_role` another role
/// of the same account that is not it — a real one, because IAM refuses a trust policy whose
/// `Principal` names no principal it holds.
fn rows(execution_role: &str, unrelated_role: &str) -> Vec<Row> {
    let account = execution_role
        .split(':')
        .nth(4)
        .expect("a role ARN carries its account");
    vec![
        Row {
            case: "a trust policy naming the execution role",
            trust: trusts_the_role(execution_role),
            assumed: true,
        },
        Row {
            // "arn:aws:iam::<account>:root" names the account the execution role belongs
            // to, which delegates to that account's identity-based policies — and the
            // execution role here carries the assume grant, so the assume is allowed.
            case: "a trust policy naming the execution role's account",
            trust: trusts_the_role(&format!("arn:aws:iam::{account}:root")),
            assumed: true,
        },
        Row {
            case: "a trust policy naming an unrelated role",
            trust: trusts_the_role(unrelated_role),
            assumed: false,
        },
        Row {
            // The identity making the assume is the execution role, not this service: a
            // document that trusts the service alone does not cover it.
            case: "a trust policy naming this service's own principal",
            trust: trusts_the_service(),
            assumed: false,
        },
        Row {
            case: "a trust policy denying the execution role outright",
            trust: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "AWS": execution_role },
                    "Action": ASSUME_ACTION
                }]
            }),
            assumed: false,
        },
        Row {
            // A `Condition` is part of the grant, and this assume carries no external id,
            // so the statement covers no request this service can show it satisfies.
            case: "a trust policy naming the execution role under an external-id condition",
            trust: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": execution_role },
                    "Action": ASSUME_ACTION,
                    "Condition": { "StringEquals": { "sts:ExternalId": "shared-secret" } }
                }]
            }),
            assumed: false,
        },
        Row {
            case: "a trust policy that grants nothing at all",
            trust: json!({ "Version": "2012-10-17", "Statement": [] }),
            assumed: false,
        },
    ]
}

/// The assume of a `Task` state's own role is decided by THAT role's trust policy: a target
/// role whose trust does not admit the execution role fails the task, and no call is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_target_roles_trust_policy_decides_the_assume() {
    let chain = Chain::new();
    let table = chain.create_table().await;
    // The execution role may assume ANY role and carries no grant over the table, so every
    // row differs only in what the target role's own trust policy says.
    let execution_role = chain
        .role(
            "TrustExecRole",
            &trusts_the_service(),
            Some(&grants(&[ASSUME_ACTION], "*")),
        )
        .await;
    // Another role of the same account, for the row whose target trusts somebody else: IAM
    // refuses a trust policy naming a principal it does not hold, so this one exists.
    let unrelated_role = chain
        .role("UnrelatedRole", &trusts_the_service(), None)
        .await;

    for (index, row) in rows(&execution_role, &unrelated_role)
        .into_iter()
        .enumerate()
    {
        let case = row.case;
        let target = chain
            .role(
                &format!("TrustTarget{index}"),
                &row.trust,
                // Whichever row it is, the target role may write the table: what differs is
                // whether this service may act as it at all.
                Some(&grants(&["dynamodb:PutItem"], &table)),
            )
            .await;
        let pk = format!("trust-{index}");
        let (_execution, described) = chain
            .ran(
                &format!("trusted-{index}"),
                &definition(Some(json!({ "RoleArn": target })), false, &pk),
                &execution_role,
                &json!({ "role": target }),
            )
            .await;
        let status = described.get("status").and_then(Value::as_str);
        let written = chain.item(&pk).await;
        match row.assumed {
            true => {
                assert_eq!(
                    status,
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the assume the trust admits to let the task run, \
                     got {described:?}"
                );
                assert!(
                    written.is_some(),
                    "case {case:?}: expected the call made under the target role to have \
                     written {pk} into {TABLE}"
                );
            }
            false => {
                assert_eq!(
                    status,
                    Some("FAILED"),
                    "case {case:?}: expected the task to fail on the refused assume, got \
                     {described:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(TASK_FAILED_ERROR),
                    "case {case:?}: expected {TASK_FAILED_ERROR}, got {described:?}"
                );
                let cause = described
                    .get("cause")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_eq!(
                    cause,
                    unassumable_cause(&execution_role, &target),
                    "case {case:?}: expected the sentence naming both roles, got {cause:?}"
                );
                assert_eq!(
                    written, None,
                    "case {case:?}: a call that was not made must have written nothing"
                );
            }
        }
    }
}

/// The two documents are both required where the trust only names the ACCOUNT: that
/// statement delegates, so an execution role carrying no assume grant of its own is refused
/// — while the same role is admitted by a trust that names it directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_wide_trust_still_needs_the_execution_roles_own_grant() {
    let chain = Chain::new();
    let table = chain.create_table().await;
    // This execution role may do nothing but write its own logs: it holds no assume grant.
    let execution_role = chain
        .role(
            "UngrantedExecRole",
            &trusts_the_service(),
            Some(&grants(&["logs:PutLogEvents"], "*")),
        )
        .await;
    let account = execution_role
        .split(':')
        .nth(4)
        .expect("a role ARN carries its account")
        .to_string();

    for (index, (case, trust, assumed)) in [
        (
            "a trust naming the account, with no identity grant to delegate to",
            trusts_the_role(&format!("arn:aws:iam::{account}:root")),
            false,
        ),
        (
            "a trust naming the execution role itself, which grants on its own",
            trusts_the_role(&execution_role),
            true,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let target = chain
            .role(
                &format!("DelegatingTarget{index}"),
                &trust,
                Some(&grants(&["dynamodb:PutItem"], &table)),
            )
            .await;
        let pk = format!("delegated-{index}");
        let (_execution, described) = chain
            .ran(
                &format!("delegated-{index}"),
                &definition(Some(json!({ "RoleArn": target })), false, &pk),
                &execution_role,
                &json!({ "role": target }),
            )
            .await;
        let expected = match assumed {
            true => "SUCCEEDED",
            false => "FAILED",
        };
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected {expected}, got {described:?}"
        );
        assert_eq!(
            chain.item(&pk).await.is_some(),
            assumed,
            "case {case:?}: expected the call to have {}been made",
            match assumed {
                true => "",
                false => "not ",
            }
        );
    }
}
