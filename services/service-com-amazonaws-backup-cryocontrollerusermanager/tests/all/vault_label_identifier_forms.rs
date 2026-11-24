// SPDX-License-Identifier: CC0-1.0

//! The FORM of the vault label does not change the decision a vault-scoped call
//! takes.
//!
//! The rule under test: `DescribeBackupVault` and `DeleteBackupVault` bind
//! `BackupVaultName` as an `@httpLabel` targeting a plain `string` — the model
//! states no pattern for either (`smithy_query(query="operation",
//! operation="DescribeBackupVault,DeleteBackupVault")` reports
//! `BackupVaultName` with target `string` and no `pattern`, where
//! `ListRecoveryPointsByBackupVault` reports the patterned `BackupVaultName`
//! shape). So neither operation judges that label on its own: both authorize
//! against the vault the label NAMES, and a label that names no vault this
//! account holds in this Region is refused with HTTP 403
//! `AccessDeniedException` and the service's fixed sentence, whatever form the
//! label takes — a vault ARN, an ARN of another resource, or a name outside the
//! vault-name alphabet. The refusal discloses nothing about whether a vault
//! exists, and it removes nothing.
//!
//! Every case drives real HTTP through the router, so what is asserted is the
//! wire behaviour a `backup` SDK client sees. The rows differ along the label's
//! form, which is what the rule generalises over; the recorded client mistake
//! (the vault's own ARN passed where the name is modelled) is one row of the
//! table, and a stored vault addressed by its NAME is the positive control.
//!
//! The harness is the one `vault_read_visibility.rs` publishes to this test
//! crate.

use crate::vault_and_backup_job::{vault_arn, ACCOUNT, REGION};
use crate::vault_read_visibility::{assert_denied, call, error_code, prepare, router, Setup};

/// The vault every case stores before addressing one by a label of another form,
/// so each row runs against an account that really does hold a vault.
const STORED: &str = "label-form-vault";

/// The two operations that take a vault label typed as a plain `string`, with the
/// method each is bound to on `/backup-vaults/{BackupVaultName}`.
const VAULT_LABEL_OPERATIONS: &[(&str, &str)] = &[
    ("DescribeBackupVault", "GET"),
    ("DeleteBackupVault", "DELETE"),
];

/// The label forms the rows span: every one of them names no vault this account
/// holds in this Region under THAT label, so every one of them is the refusal.
fn label_forms() -> Vec<(&'static str, String)> {
    let fifty_one: String = std::iter::repeat_n('v', 51).collect();
    vec![
        (
            "the stored vault's own ARN, where the bare name is modelled",
            vault_arn(REGION, STORED),
        ),
        (
            "the ARN of a vault in another Region",
            vault_arn("eu-west-1", STORED),
        ),
        (
            "the ARN of a vault of another account",
            format!("arn:aws:backup:{REGION}:111122223333:backup-vault:{STORED}"),
        ),
        (
            "an ARN of another resource of this service",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:plan:8F81F553-3A74-4A3F-B93D-B3360DC80C50"),
        ),
        ("a bare name nothing was created under", "not-a-vault".to_string()),
        ("one character — under the vault-name minimum of 2", "q".to_string()),
        ("51 characters — over the vault-name maximum of 50", fifty_one),
        ("a dot, which the vault-name alphabet excludes", "has.dot".to_string()),
        ("a plus sign, which the alphabet excludes", "has+plus".to_string()),
        (
            "a percent-encoded space, which the alphabet excludes",
            "has%20space".to_string(),
        ),
    ]
}

/// The `BackupVaultName`s `ListBackupVaults` reports.
async fn listed_names(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let listed = call(router, "GET", "/backup-vaults", None).await;
    assert_eq!(
        listed.status, 200,
        "ListBackupVaults must answer 200, got {listed:?}"
    );
    listed.body["BackupVaultList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            entry
                .get("BackupVaultName")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Whatever form the label takes, a label that names no vault this account holds
/// in this Region is the authorization refusal on BOTH operations — not a
/// parameter refusal and not an absent resource.
#[tokio::test]
async fn a_label_naming_no_visible_vault_is_denied_whatever_its_form() {
    for (form, label) in label_forms() {
        for (operation, method) in VAULT_LABEL_OPERATIONS {
            let case = format!("{operation} addressed by {form}");
            let router = router();
            prepare(&router, Setup::Created, STORED).await;

            let answer = call(&router, method, &format!("/backup-vaults/{label}"), None).await;
            assert_denied(&case, &answer, &label);

            // The refusal removed nothing: the vault the account really holds
            // still reads back, and the listing still reports exactly it.
            let kept = call(&router, "GET", &format!("/backup-vaults/{STORED}"), None).await;
            assert_eq!(
                kept.status, 200,
                "case {case:?}: the stored vault must still read back, got {kept:?}"
            );
            assert_eq!(
                listed_names(&router).await,
                vec![STORED.to_string()],
                "case {case:?}: the refused call must leave the account's vaults as they were"
            );
        }
    }
}

/// The positive control, which is also the round trip: the vault the account
/// holds answers its NAME with 200 on the read, and its delete takes it away —
/// after which the SAME label is the refusal the forms above get.
#[tokio::test]
async fn the_stored_vault_answers_its_own_name() {
    let router = router();
    prepare(&router, Setup::Created, STORED).await;

    let described = call(&router, "GET", &format!("/backup-vaults/{STORED}"), None).await;
    assert_eq!(
        (described.status, error_code(&described.body).as_str()),
        (200, "<none>"),
        "case \"the read of the stored vault\": expected 200, got {described:?}"
    );
    assert_eq!(
        described
            .body
            .get("BackupVaultArn")
            .and_then(serde_json::Value::as_str),
        Some(vault_arn(REGION, STORED).as_str()),
        "case \"the read of the stored vault\": expected its own ARN, got {described:?}"
    );

    let deleted = call(&router, "DELETE", &format!("/backup-vaults/{STORED}"), None).await;
    assert_eq!(
        deleted.status, 200,
        "case \"the delete of the stored vault\": expected 200, got {deleted:?}"
    );

    for (operation, method) in VAULT_LABEL_OPERATIONS {
        let answer = call(&router, method, &format!("/backup-vaults/{STORED}"), None).await;
        assert_denied(
            &format!("{operation} on the name after its vault was deleted"),
            &answer,
            STORED,
        );
    }
    assert!(
        listed_names(&router).await.is_empty(),
        "case \"after the delete\": the account holds no vault to list"
    );
}
