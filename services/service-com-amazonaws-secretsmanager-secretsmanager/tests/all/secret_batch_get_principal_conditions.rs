// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a batched read whose per-entry decision is taken under the CALLER's
//! global condition keys, driven over the real wire path (an awsJson1_1 request through the
//! multi-service router into the mounted service beside the IAM and STS services on one
//! shared engine), so what is asserted is what a client holding those credentials reads.
//!
//! THE RULE, in two halves that meet here:
//!
//! * a batched read is judged for EACH secret it answers for — "you must have
//!   secretsmanager:GetSecretValue for each secret" (`BatchGetSecretValue`, this service's
//!   Smithy model) — so a secret the caller may not read is one entry in `Errors` inside the
//!   200 and its value is never answered, while every other selection is answered in full:
//!   "If Secrets Manager encounters errors such as AccessDeniedException while attempting to
//!   retrieve any of the secrets, you can see the errors in Errors in the response."
//! * that decision is taken under the condition context of the request, where the global key
//!   naming the calling principal answers with the IDENTITY whose permissions are in force —
//!   "for IAM roles, the request context returns the ARN of the role, not the ARN of the user
//!   that assumed the role" (`aws:PrincipalArn`, IAM's global condition keys) — so a clause
//!   over it is DECIDED rather than left undecided, and a statement a session's role
//!   satisfies does not apply to that session's requests.
//!
//! Together: a conditioned `Deny` in ONE selected secret's resource-based policy degrades
//! exactly that entry for the principals the condition covers, and degrades nothing at all
//! for the principal the condition exempts.
//!
//! The table spans what the rule generalises over: the operator (`StringEquals`,
//! `StringNotEquals`, `ArnLike`), the key (the principal's ARN, the principal's account), the
//! ARN form the clause names (the role behind the session, the session's own ARN, another
//! role of the account, a wildcard over the account's roles), where in the batch the
//! conditioned secret sits, and the form of the `SecretId` the request names the secrets by.
//!
//! `tests/all/secret_batch_get_authorization.rs` spans the per-entry partition under
//! UNconditioned policies; `tests/all/caller_principal_conditions.rs` spans the caller keys
//! on a read of ONE secret; `tests/all/identity_based_authorization.rs` owns the harness all
//! three reuse.

use serde_json::{json, Value};

use crate::caller_principal_conditions::{
    any_role_of_this_account, other_role_arn, resource_policy_if,
};
use crate::identity_based_authorization::{
    allow_the_service, call_as, errors_of, given_role_policy, given_secret, given_session,
    refusal_of_the_read, role_arn, router_with_identity, session_arn, ACCESS_DENIED,
    ACCOUNT_KEY_ID, RESOURCE_DENY_CLAUSE, SECRET_VALUE, SESSION_KEY_ID,
};
use crate::secret_batch_get_authorization::values_of;

/// The three secrets a case holds, so the conditioned one can sit first, in the middle or
/// last.
const SECRETS: [&str; 3] = [
    "principal-batch-first",
    "principal-batch-middle",
    "principal-batch-last",
];

/// The action each entry of a batched read is judged against: "you must have
/// secretsmanager:GetSecretValue for each secret" (`BatchGetSecretValue`, this service's
/// Smithy model).
const READ_ACTION: &str = "secretsmanager:GetSecretValue";

/// A resource-based policy denying every principal that read, qualified by `condition`.
///
/// Built by the sibling suite's policy writer, so both suites attach the same document shape;
/// the wildcard principal is admitted because "by default, public policies aren't blocked"
/// (`PutResourcePolicyRequest$BlockPublicPolicy`, this service's Smithy model).
fn deny_reads_unless(condition: Value) -> String {
    resource_policy_if("Deny", READ_ACTION, condition)
}

/// Attach `document` to the secret `secret_id` names, as the account's own credential — the
/// read the cases then take is the session's, so nothing about the attach decides them.
async fn given_resource_policy(
    router: &::smithy_server_runtime::MultiServiceRouter,
    secret_id: &str,
    document: &str,
    case: &str,
) {
    let (status, answered) = call_as(
        router,
        ACCOUNT_KEY_ID,
        "PutResourcePolicy",
        &json!({ "SecretId": secret_id, "ResourcePolicy": document }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the policy must attach, got {status} {answered}"
    );
}

/// The batched read of `ids` as the seeded session, which must always be a 200: a problem
/// with one secret is an entry in `Errors`, not a failure of the call.
async fn batch_read(
    router: &::smithy_server_runtime::MultiServiceRouter,
    ids: &[String],
    case: &str,
) -> Value {
    let (status, answer) = call_as(
        router,
        SESSION_KEY_ID,
        "BatchGetSecretValue",
        &json!({ "SecretIdList": ids }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a per-secret refusal must not fail the call, got {status} {answer}"
    );
    answer
}

/// RULE: the per-entry decision of a batched read is taken under the caller's principal keys,
/// so a conditioned `Deny` in one selected secret's policy reports exactly that entry for a
/// principal the condition covers — and reports nothing for the principal it exempts, whose
/// batch is answered in full.
#[tokio::test]
async fn a_principal_clause_decides_one_entry_of_a_batched_read() {
    let role = role_arn();
    let session = session_arn();
    let other = other_role_arn();
    let any_role = any_role_of_this_account();
    let account = ::engine::emulator_account_id().to_string();
    // (what the clause says, the `Condition` block, whether the conditioned secret stays
    // READABLE for the seeded session)
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "a deny of everything but the role behind the session",
            json!({ "StringNotEquals": { "aws:PrincipalArn": role.clone() } }),
            true,
        ),
        (
            "a deny of everything but the SESSION ARN, which the key does not answer",
            json!({ "StringNotEquals": { "aws:PrincipalArn": session.clone() } }),
            false,
        ),
        (
            "a deny of everything but another role of this account",
            json!({ "StringNotEquals": { "aws:PrincipalArn": other.clone() } }),
            false,
        ),
        (
            "a deny naming the role behind the session",
            json!({ "StringEquals": { "aws:PrincipalArn": role.clone() } }),
            false,
        ),
        (
            "a deny over every role of this account",
            json!({ "ArnLike": { "aws:PrincipalArn": any_role.clone() } }),
            false,
        ),
        (
            "a deny of everything but the caller's own account",
            json!({ "StringNotEquals": { "aws:PrincipalAccount": account.clone() } }),
            true,
        ),
        (
            "a deny of the caller's own account",
            json!({ "StringEquals": { "aws:PrincipalAccount": account.clone() } }),
            false,
        ),
    ];

    for (row, (clause, condition, readable)) in rows.into_iter().enumerate() {
        // Where in the batch the conditioned secret sits, walked across the table.
        let conditioned_at = row % SECRETS.len();
        let (router, engine) = router_with_identity();
        given_session(&engine);
        given_role_policy(&engine, Some(&allow_the_service()));
        let mut arns: Vec<String> = Vec::new();
        for name in SECRETS {
            arns.push(given_secret(&router, name).await);
        }
        let conditioned_arn = arns[conditioned_at].clone();
        given_resource_policy(
            &router,
            &conditioned_arn,
            &deny_reads_unless(condition.clone()),
            clause,
        )
        .await;

        // The form the request names each secret by: its name, or the ARN the account holds
        // it under. Both address the same secrets, and an entry reports the id as the
        // request spelled it.
        let id_forms: [(&str, Vec<String>); 2] = [
            ("their names", SECRETS.iter().map(|name| name.to_string()).collect()),
            ("their ARNs", arns.clone()),
        ];
        for (form, ids) in id_forms {
            let case = format!(
                "{clause}, on {} of the batch, addressed by {form}",
                SECRETS[conditioned_at]
            );
            let answer = batch_read(&router, &ids, &case).await;
            let answered = values_of(&answer, &case);
            let reported = errors_of(&answer, &case);
            if readable {
                let expected: Vec<(String, String)> = arns
                    .iter()
                    .map(|arn| (arn.clone(), SECRET_VALUE.to_string()))
                    .collect();
                assert_eq!(
                    answered, expected,
                    "case {case:?}: the exempted principal must read every secret, got \
                     {answered:?}"
                );
                assert!(
                    reported.is_empty(),
                    "case {case:?}: expected no error entry, got {reported:?}"
                );
            } else {
                let expected: Vec<(String, String)> = arns
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != conditioned_at)
                    .map(|(_, arn)| (arn.clone(), SECRET_VALUE.to_string()))
                    .collect();
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected the other secrets' values, got {answered:?}"
                );
                let supplied = ids[conditioned_at].clone();
                assert_eq!(
                    reported,
                    vec![(
                        supplied.clone(),
                        ACCESS_DENIED.to_string(),
                        refusal_of_the_read(&supplied, RESOURCE_DENY_CLAUSE),
                    )],
                    "case {case:?}: expected one access-denied entry for the denied secret, got \
                     {reported:?}"
                );
            }
        }
    }
}

/// RULE: the partition follows the policies in force at the instant of the read — replacing a
/// conditioned `Deny` the caller does NOT satisfy with one it does, and deleting it, each
/// restore the withheld entry to `SecretValues`.
#[tokio::test]
async fn changing_the_clause_moves_the_entry_between_the_two_partitions() {
    let (router, engine) = router_with_identity();
    given_session(&engine);
    given_role_policy(&engine, Some(&allow_the_service()));
    let mut arns: Vec<String> = Vec::new();
    for name in SECRETS {
        arns.push(given_secret(&router, name).await);
    }
    let locked = arns[1].clone();

    // (what the row puts in force, the clause it conditions the deny on — `None` deletes the
    // policy — how many values the batch then answers)
    let rows: [(&str, Option<Value>, usize); 4] = [
        ("before any policy", None, SECRETS.len()),
        (
            "a deny of everything but another role",
            Some(json!({ "StringNotEquals": { "aws:PrincipalArn": other_role_arn() } })),
            SECRETS.len() - 1,
        ),
        (
            "a deny of everything but the role behind the session",
            Some(json!({ "StringNotEquals": { "aws:PrincipalArn": role_arn() } })),
            SECRETS.len(),
        ),
        ("after the policy is deleted", None, SECRETS.len()),
    ];
    for (case, condition, expected) in rows {
        match condition {
            Some(condition) => {
                given_resource_policy(&router, &locked, &deny_reads_unless(condition), case).await;
            }
            None => {
                let (status, answered) = call_as(
                    &router,
                    ACCOUNT_KEY_ID,
                    "DeleteResourcePolicy",
                    &json!({ "SecretId": locked.clone() }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the policy delete must be served, got {status} {answered}"
                );
            }
        }

        let answer = batch_read(&router, &arns, case).await;
        let answered = values_of(&answer, case);
        assert_eq!(
            answered.len(),
            expected,
            "case {case:?}: expected {expected} value(s), got {answered:?}"
        );
        let reported = errors_of(&answer, case);
        assert_eq!(
            reported.len(),
            SECRETS.len() - expected,
            "case {case:?}: expected {} error entry(ies), got {reported:?}",
            SECRETS.len() - expected
        );
    }
}
