// SPDX-License-Identifier: CC0-1.0

//! In which ORDER a vault-scoped read of a recovery point takes its access
//! decisions.
//!
//! The rule under test: the ACCOUNT a read scopes itself to is decided before any
//! other resource it names — a request that addresses another account's vault with
//! `BackupVaultAccountId` asks to act in that account, which only that account could
//! grant, so it is refused with AWS Backup's own fixed access-denied sentence
//! whatever else the request carries — and only then is the `RecoveryPointArn`
//! judged, where an ARN of another account earns IAM's resource-policy denial naming
//! the principal, the action and that ARN. The vault's EXISTENCE comes last, because
//! it is state: a vault this account and Region do not hold is refused for lack of
//! privileges too, and in the service's own sentence.
//!
//! Every answer here is HTTP 403 `AccessDeniedException`, so what the table spans is
//! WHICH SENTENCE a request gets: the rows differ along the account the request
//! scopes itself to and the vault it addresses, crossed with the recovery point it
//! names (another account's, this account's). The controls are the other half of the
//! rule: a request that clears every decision reaches the read itself and is
//! answered the one not-found refusal, so the refusals above are the decisions and
//! not the read failing for some other reason.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! behaviour a `backup` SDK client sees; the harness is the shared one in
//! `vault_and_backup_job.rs`.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::recovery_point_cross_account_authorization::denial;
use crate::vault_and_backup_job::{call_reported, create_vault, error_code, string_at, ACCOUNT, REGION};

/// The vault this account holds, which every row addresses unless it is about a
/// vault the account does not have.
const VAULT: &str = "read-order-source";

/// A vault name nothing was ever created under.
const ABSENT_VAULT: &str = "read-order-never-created";

/// An account id that is not the one serving these requests — what a
/// `BackupVaultAccountId` naming ANOTHER account looks like.
const OTHER_ACCOUNT: &str = "111122223333";

/// A second one, so no row is the only foreign account id.
const THIRD_ACCOUNT: &str = "210987654321";

/// The sentence AWS Backup refuses a call over a vault outside the caller's
/// visibility with: one fixed string, naming nothing of the request.
///
/// Spelled out here rather than read from the service, so the wording is what this
/// suite asserts.
const INSUFFICIENT_PRIVILEGES: &str = "Insufficient privileges to perform this action.";

/// `DescribeRecoveryPoint` for one `(vault, recovery point)` pair, optionally scoped
/// to `vault_account_id` with the documented cross-account query parameter.
async fn describe(
    router: &MultiServiceRouter,
    vault: &str,
    recovery_point_arn: &str,
    vault_account_id: Option<&str>,
) -> (u16, String, Value) {
    let path = format!("/backup-vaults/{vault}/recovery-points/{recovery_point_arn}");
    let path = match vault_account_id {
        Some(account) => format!("{path}?backupVaultAccountId={account}"),
        None => path,
    };
    call_reported(router, "GET", &path, None).await
}

/// A recovery-point ARN of `account` in this Region.
fn recovery_point_arn(account: &str, id: &str) -> String {
    format!("arn:aws:backup:{REGION}:{account}:recovery-point:{id}")
}

/// The vault this account holds, created once per case's router.
async fn with_vault() -> MultiServiceRouter {
    let router = crate::vault_and_backup_job::router();
    let (status, created) = create_vault(&router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );
    router
}

/// The account a read scopes itself to decides the call BEFORE the recovery point it
/// names: every row scoped to another account is refused with AWS Backup's fixed
/// sentence, even when the `RecoveryPointArn` it carries would have earned the other
/// refusal. The last row is the vault's own existence, decided last and answered in
/// the same sentence.
#[tokio::test]
async fn the_account_a_read_is_scoped_to_is_decided_before_the_point_it_names() {
    // (case, the vault addressed, the BackupVaultAccountId, the recovery point)
    let rows: Vec<(&str, &str, Option<&str>, String)> = vec![
        (
            "another account's vault and that account's point",
            VAULT,
            Some(OTHER_ACCOUNT),
            recovery_point_arn(OTHER_ACCOUNT, "1EB3B5E7"),
        ),
        (
            "another account's vault and this account's point",
            VAULT,
            Some(OTHER_ACCOUNT),
            recovery_point_arn(ACCOUNT, "9fd3b1c4"),
        ),
        (
            "a different other account's vault",
            VAULT,
            Some(THIRD_ACCOUNT),
            recovery_point_arn(THIRD_ACCOUNT, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        ),
        (
            "another account's vault under a name this account never created",
            ABSENT_VAULT,
            Some(OTHER_ACCOUNT),
            recovery_point_arn(ACCOUNT, "1EB3B5E7"),
        ),
        (
            "this account's own point under a vault it never created",
            ABSENT_VAULT,
            None,
            recovery_point_arn(ACCOUNT, "1EB3B5E7"),
        ),
    ];

    for (case, vault, vault_account_id, point) in rows {
        let router = with_vault().await;
        let (status, error_type, refused) = describe(&router, vault, &point, vault_account_id).await;
        assert_eq!(
            (
                status,
                error_type.as_str(),
                error_code(&refused).as_str(),
                string_at(&refused, "Message")
            ),
            (
                403,
                "AccessDeniedException",
                "AccessDeniedException",
                INSUFFICIENT_PRIVILEGES.to_string()
            ),
            "case {case:?}: expected the service's own access-denied sentence, got {status} \
             {error_type} {refused:?}"
        );
    }
}

/// A read the account scoping admits is then judged on the recovery point it names:
/// a `RecoveryPointArn` of another account earns IAM's resource-policy denial, and
/// an own-account ARN that names nothing reaches the read and is answered as absent.
#[tokio::test]
async fn a_read_scoped_to_this_account_is_judged_on_the_point_it_names() {
    // The rows differ along the foreign account the point belongs to and the form of
    // its id, which is what the refusal generalises over: it names the ARN the
    // request carried, whatever that ARN is.
    let foreign: Vec<(&str, String)> = vec![
        (
            "another account, this Region",
            recovery_point_arn(OTHER_ACCOUNT, "1EB3B5E7"),
        ),
        (
            "a different other account",
            recovery_point_arn(THIRD_ACCOUNT, "9fd3b1c4"),
        ),
        (
            "another account in another Region",
            "arn:aws:backup:eu-central-1:999988887777:recovery-point:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
                .to_string(),
        ),
    ];

    for (case, point) in foreign {
        let router = with_vault().await;
        let (status, error_type, refused) = describe(&router, VAULT, &point, None).await;
        assert_eq!(
            (
                status,
                error_type.as_str(),
                error_code(&refused).as_str(),
                string_at(&refused, "Message")
            ),
            (
                403,
                "AccessDeniedException",
                "AccessDeniedException",
                denial("DescribeRecoveryPoint", &point)
            ),
            "case {case:?}: expected the point's own authorization refusal, got {status} \
             {error_type} {refused:?}"
        );
    }

    // The control: a request that clears every decision — this account's own scope,
    // this account's own point, a vault it holds — reaches the read, which reports
    // the point absent rather than refusing the call.
    let router = with_vault().await;
    let own = recovery_point_arn(ACCOUNT, "does-not-exist");
    let (status, error_type, answered) = describe(&router, VAULT, &own, None).await;
    assert_eq!(
        (status, error_type.as_str(), error_code(&answered).as_str()),
        (400, "ResourceNotFoundException", "ResourceNotFoundException"),
        "case \"an own-account point under the account's own vault\": expected the read's \
         not-found refusal, got {status} {error_type} {answered:?}"
    );

    // ... and the same control scoped EXPLICITLY to this account's own id, because a
    // `BackupVaultAccountId` naming the caller's own account addresses the caller's
    // own vault.
    let (status, error_type, answered) = describe(&router, VAULT, &own, Some(ACCOUNT)).await;
    assert_eq!(
        (status, error_type.as_str(), error_code(&answered).as_str()),
        (400, "ResourceNotFoundException", "ResourceNotFoundException"),
        "case \"the account's own id as BackupVaultAccountId\": expected the read's not-found \
         refusal, got {status} {error_type} {answered:?}"
    );
}
