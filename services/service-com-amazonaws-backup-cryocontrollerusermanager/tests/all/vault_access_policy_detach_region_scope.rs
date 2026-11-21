// SPDX-License-Identifier: CC0-1.0

//! `DeleteBackupVaultAccessPolicy` resolves the vault it detaches in the Region
//! the call was made in.
//!
//! The rule under test: "Backup vaults are identified by names that are unique
//! to the account used to create them and the Amazon Web Services Region where
//! they are created" (the model doc for this operation's `BackupVaultName`), and
//! the resource policy is a member OF a vault, so the detach reaches the vault
//! of that name in the caller's Region alone:
//!
//! * a same-named vault in another Region keeps the document it carries, and the
//!   policy read still reports it there;
//! * the detached vault's policy read answers the one not-found refusal, naming
//!   that Region's own vault ARN;
//! * a detach made in a Region that holds no vault of the name is refused for
//!   lack of privileges — access is decided before existence — and leaves the
//!   holding Region's policy in force.
//!
//! The rows span pairs of Regions the rule generalises over, in both orders,
//! including Regions no other case in this suite works in and one of another
//! partition, rather than the one pair a single deployment happens to use.
//!
//! Every case drives real HTTP through the router, each request signed for the
//! Region it is made in; the harness is the shared one in
//! `vault_and_backup_job.rs` and the policy documents are the ones the
//! `vault_access_policy` suite already states.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_access_policy::{policy_allowing, principal_arn};
use crate::vault_and_backup_job::{
    call_in_region, create_vault_in, error_code, string_at, vault_arn, router, REGION,
};

/// The name every case deploys into more than one Region.
const SHARED_NAME: &str = "shared-policy-vault";

/// `PUT /backup-vaults/{BackupVaultName}/access-policy` through a client bound
/// to `region`.
async fn put_policy_in(
    router: &MultiServiceRouter,
    region: &str,
    vault: &str,
    policy: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "PUT",
        &format!("/backup-vaults/{vault}/access-policy"),
        Some(&serde_json::json!({"Policy": policy}).to_string()),
    )
    .await
}

/// `GET /backup-vaults/{BackupVaultName}/access-policy` through a client bound
/// to `region`.
async fn get_policy_in(
    router: &MultiServiceRouter,
    region: &str,
    vault: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GET",
        &format!("/backup-vaults/{vault}/access-policy"),
        None,
    )
    .await
}

/// `DELETE /backup-vaults/{BackupVaultName}/access-policy` through a client
/// bound to `region`.
async fn delete_policy_in(
    router: &MultiServiceRouter,
    region: &str,
    vault: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "DELETE",
        &format!("/backup-vaults/{vault}/access-policy"),
        None,
    )
    .await
}

/// The (status, `__type`, `Message`) the policy read reports for a vault of
/// `region` carrying no policy: the fixed sentence whose one variable part is
/// that vault's own ARN.
fn no_policy(region: &str, vault: &str) -> (u16, String, String) {
    (
        400,
        "ResourceNotFoundException".to_string(),
        format!(
            "Vault {} has no associated vault policy",
            vault_arn(region, vault)
        ),
    )
}

/// The (status, `__type`, `Message`) an answer reports.
fn reported(status: u16, body: &Value) -> (u16, String, String) {
    (status, error_code(body), string_at(body, "Message"))
}

/// The document the policy read reports, as a JSON document rather than bytes.
fn reported_policy(case: &str, body: &Value) -> Value {
    serde_json::from_str(&string_at(body, "Policy"))
        .unwrap_or_else(|error| panic!("case {case:?}: the reported policy must be JSON: {error}"))
}

/// The pairs of Regions the rows span.
fn region_pairs() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("the suite's Region, then another", REGION, "us-east-1"),
        ("the same pair in the other order", "us-east-1", REGION),
        (
            "two Regions no other case works in",
            "eu-central-1",
            "ap-southeast-2",
        ),
        ("a Region of another partition", "us-gov-west-1", "eu-west-1"),
    ]
}

/// Create the vault and attach a policy naming a principal of its own Region, so
/// the two vaults of one name carry DIFFERENT documents.
async fn vault_carrying_policy(router: &MultiServiceRouter, region: &str, vault: &str) -> String {
    let (status, created) = create_vault_in(router, region, vault, "{}").await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBackupVault 200 in {region}, got {status} {created:?}"
    );
    let policy = policy_allowing(&principal_arn(region), "backup:DescribeBackupVault");
    let (status, attached) = put_policy_in(router, region, vault, &policy).await;
    assert_eq!(
        status, 200,
        "setup: expected PutBackupVaultAccessPolicy 200 in {region}, got {status} {attached:?}"
    );
    policy
}

/// Detaching one Region's policy leaves the same-named vault of every other
/// Region carrying its own.
#[tokio::test]
async fn the_detach_reaches_the_vault_of_its_own_region() {
    for (case, detached_in, kept_in) in region_pairs() {
        let router = router();
        vault_carrying_policy(&router, detached_in, SHARED_NAME).await;
        let kept = vault_carrying_policy(&router, kept_in, SHARED_NAME).await;

        let (status, answer) = delete_policy_in(&router, detached_in, SHARED_NAME).await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DeleteBackupVaultAccessPolicy 200 in {detached_in}, got \
             {status} {answer:?}"
        );

        let (status, read) = get_policy_in(&router, detached_in, SHARED_NAME).await;
        assert_eq!(
            reported(status, &read),
            no_policy(detached_in, SHARED_NAME),
            "case {case:?}: the vault of {detached_in} must carry no policy, got {status} {read:?}"
        );

        let (status, read) = get_policy_in(&router, kept_in, SHARED_NAME).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: the vault of {kept_in} must still report its policy, got {status} \
             {read:?}"
        );
        assert_eq!(
            string_at(&read, "BackupVaultArn"),
            vault_arn(kept_in, SHARED_NAME),
            "case {case:?}: the surviving policy belongs to {kept_in}'s own vault, got {read:?}"
        );
        assert_eq!(
            reported_policy(case, &read),
            serde_json::from_str::<Value>(&kept).expect("a well-formed policy literal"),
            "case {case:?}: {kept_in} must still report the document attached there"
        );

        // The delete states the END state, so the same call in the Region that
        // now carries nothing answers the same success — and still reaches no
        // other Region's vault.
        let (status, answer) = delete_policy_in(&router, detached_in, SHARED_NAME).await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (200, "<none>"),
            "case {case:?}: a second detach in {detached_in} answers the same success, got \
             {status} {answer:?}"
        );
        let (status, read) = get_policy_in(&router, kept_in, SHARED_NAME).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {kept_in}'s policy survives the repeated detach, got {status} \
             {read:?}"
        );
    }
}

/// A detach made in a Region that holds no vault of the name is refused for lack
/// of privileges, and the Region that does hold one keeps its policy.
#[tokio::test]
async fn a_detach_in_a_region_holding_no_such_vault_changes_nothing() {
    let home = REGION;
    let strangers = ["us-east-1", "eu-central-1", "ap-southeast-2", "us-gov-west-1"];

    let router = router();
    let policy = vault_carrying_policy(&router, home, SHARED_NAME).await;

    for region in strangers {
        let case = format!("a detach in {region}, which holds no vault of that name");
        let (status, refused) = delete_policy_in(&router, region, SHARED_NAME).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (403, "AccessDeniedException"),
            "case {case:?}: an invisible vault is refused for lack of privileges, got {status} \
             {refused:?}"
        );

        let (status, read) = get_policy_in(&router, home, SHARED_NAME).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: {home}'s policy must stay in force, got {status} {read:?}"
        );
        assert_eq!(
            reported_policy(&case, &read),
            serde_json::from_str::<Value>(&policy).expect("a well-formed policy literal"),
            "case {case:?}: {home} must still report the document attached there"
        );
    }
}
