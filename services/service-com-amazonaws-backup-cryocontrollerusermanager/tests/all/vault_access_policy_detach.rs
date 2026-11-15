// SPDX-License-Identifier: CC0-1.0

//! `DeleteBackupVaultAccessPolicy`: detaching the resource policy a vault
//! carries, and what the policy read then answers for it.
//!
//! The rules under test:
//!
//! * a vault carries exactly ONE resource policy, so detaching it leaves the
//!   vault in the state a vault that never had one is in: the policy read
//!   answers the one not-found refusal, naming the vault's own ARN, whatever
//!   document was attached before;
//! * the call states the END state, so it answers the same success for a vault
//!   that carries no policy to detach — a second delete, and a delete of a
//!   vault nothing was ever attached to, both succeed (the model marks the
//!   operation `@idempotent`);
//! * the write is per VAULT: detaching one vault's policy leaves every other
//!   vault's policy in force;
//! * like every vault-scoped call, it decides access before existence, so a
//!   vault this account does not hold is refused for lack of privileges rather
//!   than reported absent.
//!
//! The rows span the documents the policy language admits and the vault names
//! the model's pattern admits, including both published ends of its length
//! bound.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the route bindings and policy documents are
//! the ones the sibling `vault_access_policy` suite already states.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_access_policy::{
    get_policy, no_policy_message, policy_allowing, principal_arn, put_policy, vault_named,
};
use crate::vault_and_backup_job::{call, error_code, name_of_length, router, string_at};

/// `DELETE /backup-vaults/{BackupVaultName}/access-policy`, as the model binds
/// the detach.
async fn delete_policy(router: &MultiServiceRouter, vault: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/backup-vaults/{vault}/access-policy"),
        None,
    )
    .await
}

/// What the policy read reports for a vault carrying no policy.
fn no_policy(vault: &str) -> (u16, String, String) {
    (
        400,
        "ResourceNotFoundException".to_string(),
        no_policy_message(vault),
    )
}

/// The (status, `__type`, `Message`) an answer reports.
fn reported(status: u16, body: &Value) -> (u16, String, String) {
    (status, error_code(body), string_at(body, "Message"))
}

/// Detaching the policy a vault carries leaves the vault with none, and the
/// read then answers exactly as it does for a vault that never had one.
///
/// The rows differ along the document that was attached and the name of the
/// vault carrying it — both published ends of the name's length bound among
/// them — so the rule is asserted over the class rather than over one vault.
#[tokio::test]
async fn detaching_a_policy_leaves_the_vault_carrying_none() {
    let cases: Vec<(&str, String, String)> = vec![
        (
            "one statement",
            "policy-vault".to_string(),
            policy_allowing(&principal_arn("reader"), "backup:DescribeBackupVault"),
        ),
        (
            "two statements, one of each effect",
            name_of_length(2),
            json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": {"AWS": principal_arn("reader")},
                        "Action": "backup:DescribeBackupVault",
                    },
                    {
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "backup:DeleteRecoveryPoint",
                    },
                ],
            })
            .to_string(),
        ),
        (
            "the complement elements, on the longest admitted name",
            name_of_length(50),
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "NotPrincipal": {"AWS": principal_arn("owner")},
                    "NotAction": "backup:DescribeBackupVault",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
    ];

    for (case, vault, policy) in cases {
        let router = router();
        vault_named(&router, &vault).await;

        let (status, attached) = put_policy(&router, &vault, &policy).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup PutBackupVaultAccessPolicy should succeed, got {status} \
             {attached:?}"
        );
        let (status, read) = get_policy(&router, &vault).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup read should report the attached policy, got {status} {read:?}"
        );

        let (status, detached) = delete_policy(&router, &vault).await;
        assert_eq!(
            (status, error_code(&detached).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DeleteBackupVaultAccessPolicy 200, got {status} {detached:?}"
        );

        let (status, read) = get_policy(&router, &vault).await;
        assert_eq!(
            reported(status, &read),
            no_policy(&vault),
            "case {case:?}: a detached policy leaves the vault with none, got {status} {read:?}"
        );

        // The vault itself survives the detach: it is still readable, so what
        // the delete removed is the policy and not the vault.
        let (status, described) =
            call(&router, "GET", &format!("/backup-vaults/{vault}"), None).await;
        assert_eq!(
            (status, string_at(&described, "BackupVaultName")),
            (200, vault.clone()),
            "case {case:?}: the vault must survive the detach, got {status} {described:?}"
        );

        // A policy attached after the detach is in force again, so the detach
        // cleared the member rather than closing the vault to policies.
        let again = policy_allowing(&principal_arn("second"), "backup:DeleteRecoveryPoint");
        let (status, attached) = put_policy(&router, &vault, &again).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a policy may be attached again, got {status} {attached:?}"
        );
        let (status, read) = get_policy(&router, &vault).await;
        assert_eq!(status, 200, "case {case:?}: the new policy must be readable");
        let reported_policy: Value = serde_json::from_str(&string_at(&read, "Policy"))
            .unwrap_or_else(|error| panic!("case {case:?}: the reported policy must be JSON: {error}"));
        let submitted: Value =
            serde_json::from_str(&again).expect("a well-formed policy literal in the table");
        assert_eq!(
            reported_policy, submitted,
            "case {case:?}: the read must report the document attached after the detach"
        );
    }
}

/// The delete states the END state, so a vault with nothing to detach answers
/// the same success — however many times it is called.
///
/// The rows differ along how the vault came to carry no policy: never given
/// one, and one whose policy a previous delete already detached.
#[tokio::test]
async fn detaching_nothing_answers_the_same_success() {
    let router = router();
    for vault in ["bare-vault", "policy-vault"] {
        vault_named(&router, vault).await;
    }
    let policy = policy_allowing(&principal_arn("reader"), "backup:DescribeBackupVault");
    let (status, attached) = put_policy(&router, "policy-vault", &policy).await;
    assert_eq!(
        status, 200,
        "setup: PutBackupVaultAccessPolicy should succeed, got {status} {attached:?}"
    );

    let cases = [
        ("a vault that was never given a policy", "bare-vault", 2),
        ("a vault whose policy is detached twice", "policy-vault", 2),
    ];
    for (case, vault, deletes) in cases {
        for attempt in 1..=deletes {
            let (status, detached) = delete_policy(&router, vault).await;
            assert_eq!(
                (status, error_code(&detached).as_str()),
                (200, "<none>"),
                "case {case:?} attempt {attempt}: every delete answers the same success, got \
                 {status} {detached:?}"
            );
            let (status, read) = get_policy(&router, vault).await;
            assert_eq!(
                reported(status, &read),
                no_policy(vault),
                "case {case:?} attempt {attempt}: the vault must carry no policy, got {status} \
                 {read:?}"
            );
        }
    }
}

/// The detach is per vault: another vault's policy stays in force.
#[tokio::test]
async fn the_detach_reaches_only_the_vault_it_names() {
    let router = router();
    for vault in ["kept-vault", "cleared-vault"] {
        vault_named(&router, vault).await;
        let policy = policy_allowing(&principal_arn(vault), "backup:DescribeBackupVault");
        let (status, attached) = put_policy(&router, vault, &policy).await;
        assert_eq!(
            status, 200,
            "setup {vault}: PutBackupVaultAccessPolicy should succeed, got {status} {attached:?}"
        );
    }

    let (status, detached) = delete_policy(&router, "cleared-vault").await;
    assert_eq!(
        status, 200,
        "expected DeleteBackupVaultAccessPolicy 200, got {status} {detached:?}"
    );

    let (status, read) = get_policy(&router, "cleared-vault").await;
    assert_eq!(
        reported(status, &read),
        no_policy("cleared-vault"),
        "the named vault must carry no policy, got {status} {read:?}"
    );
    let (status, read) = get_policy(&router, "kept-vault").await;
    assert_eq!(
        status, 200,
        "another vault's policy must stay in force, got {status} {read:?}"
    );
    let reported_policy: Value =
        serde_json::from_str(&string_at(&read, "Policy")).expect("the reported policy is JSON");
    let expected: Value = serde_json::from_str(&policy_allowing(
        &principal_arn("kept-vault"),
        "backup:DescribeBackupVault",
    ))
    .expect("a well-formed policy literal");
    assert_eq!(
        reported_policy, expected,
        "the untouched vault must still report its own document"
    );
}

/// The detach decides access before existence, like every vault-scoped call: a
/// vault this account does not hold is refused for lack of privileges.
#[tokio::test]
async fn a_vault_the_caller_cannot_see_is_refused_for_lack_of_privileges() {
    let router = router();
    for (case, vault) in [
        ("a vault nothing created", "never-created"),
        ("the shortest admitted name", "ab"),
    ] {
        let (status, refused) = delete_policy(&router, vault).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (403, "AccessDeniedException"),
            "case {case:?}: an invisible vault is refused for lack of privileges, got {status} \
             {refused:?}"
        );
    }
}
