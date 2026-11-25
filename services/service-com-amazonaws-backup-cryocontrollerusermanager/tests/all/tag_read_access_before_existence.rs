// SPDX-License-Identifier: CC0-1.0

//! The ARN-addressed tag read decides ACCESS before EXISTENCE for the vault its
//! ARN names.
//!
//! The rule under test: `ListTags` is authorized against the resource its
//! `ResourceArn` names, so an ARN naming a backup vault is a vault-scoped read
//! reached through the ARN rather than through `BackupVaultName` — and a
//! vault-scoped call whose vault is outside the caller's visibility (this
//! account holds none of that name in that Region, because it was never created
//! or because it has been deleted, or the ARN names another account's vault) is
//! refused with HTTP 403 and the error type `AccessDeniedException` carrying the
//! service's fixed sentence, disclosing nothing about whether that vault exists.
//! An ARN of any other kind this account holds no taggable resource under keeps
//! the modelled not-found refusal, which is the refusal those resources' own
//! reads state.
//!
//! The first table spans the reasons a vault's ARN resolves to no vault of this
//! caller — the reason, the Region and account the ARN names, the vault-name
//! spelling (including names no vault of this account could ever have been
//! stored under) and the paging query the read carries. The second pins the
//! boundary on the kinds whose ARNs keep the not-found refusal, and the third
//! the positive control: a resolvable ARN still answers its own tags, including
//! the very ARN that was refused once its name holds a vault again.
//!
//! Every case drives real HTTP through the router, so what is asserted is the
//! wire behaviour a `backup` SDK client sees. The harness (the router, the
//! one-request `call` that also reports `x-amzn-errortype`, and `assert_denied`)
//! is the shared one in `vault_read_visibility.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::resource_tags::NOT_FOUND;
use crate::vault_read_visibility::{
    assert_denied, call, error_code, message, router, Answer, DENIED_MESSAGE,
};

/// The one account id a generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region the harness's `Authorization` header puts in its credential scope,
/// so the Region every vault these cases create belongs to.
const REGION: &str = "us-west-2";

/// An account id that is not this emulator's, in the 12-digit form an account id
/// takes.
const OTHER_ACCOUNT: &str = "111122223333";

/// A vault's ARN as AWS Backup mints it:
/// `arn:aws:backup:<region>:<account>:backup-vault:<name>`.
fn vault_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:backup:{region}:{account}:backup-vault:{name}")
}

/// `PUT /backup-vaults/{BackupVaultName}` — the create, answering the vault's
/// own ARN.
async fn create_vault(router: &MultiServiceRouter, name: &str, tags: Value) -> String {
    let created = call(
        router,
        "PUT",
        &format!("/backup-vaults/{name}"),
        Some(&json!({"BackupVaultTags": tags}).to_string()),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBackupVault {name:?} must succeed, got {created:?}"
    );
    created
        .body
        .get("BackupVaultArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `DELETE /backup-vaults/{BackupVaultName}` — the delete that makes the vault's
/// ARN resolve to nothing.
async fn delete_vault(router: &MultiServiceRouter, name: &str) {
    let deleted = call(router, "DELETE", &format!("/backup-vaults/{name}"), None).await;
    assert_eq!(
        deleted.status, 200,
        "setup: DeleteBackupVault {name:?} must succeed, got {deleted:?}"
    );
}

/// `GET /tags/{ResourceArn}{query}`, as the model binds the tag read.
async fn list_tags(router: &MultiServiceRouter, resource_arn: &str, query: &str) -> Answer {
    call(router, "GET", &format!("/tags/{resource_arn}{query}"), None).await
}

/// The `Tags` map a successful read reports.
fn reported_tags(body: &Value) -> Value {
    body.get("Tags").cloned().unwrap_or(Value::Null)
}

/// A tag read of a vault ARN outside the caller's visibility is refused with 403
/// `AccessDeniedException`, whatever makes the vault invisible and whatever the
/// ARN spells.
#[tokio::test]
async fn a_tag_read_of_an_invisible_vault_s_arn_is_denied() {
    let router = router();
    // A vault that stays, and one created and then deleted: the two differ only
    // in whether this account still holds them.
    create_vault(&router, "live-vault", json!({"team": "storage"})).await;
    let doomed = create_vault(&router, "doomed-vault", json!({"team": "storage"})).await;
    assert_eq!(
        doomed,
        vault_arn(REGION, ACCOUNT, "doomed-vault"),
        "setup: the create must answer the vault's own ARN, got {doomed:?}"
    );
    // The tags were readable while the vault was there, so each row below is
    // about the vault being gone rather than about the tags never having been
    // written.
    let before = list_tags(&router, &doomed, "").await;
    assert_eq!(
        (before.status, reported_tags(&before.body)),
        (200, json!({"team": "storage"})),
        "setup: the live vault's ARN must read its tags, got {before:?}"
    );
    delete_vault(&router, "doomed-vault").await;

    // A name of 51 characters and one of a single character are both outside the
    // `BackupVaultName` pattern `^[a-zA-Z0-9\-\_]{2,50}$` (the model's own, from
    // smithy_query(query="shape", shape="BackupVaultName")), so no vault of this
    // account could ever have been stored under them.
    let too_long: String = std::iter::repeat_n('v', 51).collect();
    let invisible: Vec<(&str, String, &str)> = vec![
        (
            "a vault deleted by DeleteBackupVault",
            doomed.clone(),
            "",
        ),
        (
            "the same deleted vault, read with a page size",
            doomed.clone(),
            "?maxResults=10",
        ),
        (
            "a vault that was never created",
            vault_arn(REGION, ACCOUNT, "never-created"),
            "",
        ),
        (
            "the live vault's name in another Region",
            vault_arn("eu-central-1", ACCOUNT, "live-vault"),
            "",
        ),
        (
            "the live vault's name in another account",
            vault_arn(REGION, OTHER_ACCOUNT, "live-vault"),
            "",
        ),
        (
            "a deleted vault's name in another partition",
            vault_arn(REGION, ACCOUNT, "doomed-vault").replace("arn:aws:", "arn:aws-us-gov:"),
            "",
        ),
        (
            "a single-character name no vault could be stored under",
            vault_arn(REGION, ACCOUNT, "v"),
            "",
        ),
        (
            "a 51-character name past the name bound",
            vault_arn(REGION, ACCOUNT, &too_long),
            "",
        ),
        (
            "a name carrying a character the vault-name pattern admits none of",
            vault_arn(REGION, ACCOUNT, "live*vault"),
            "",
        ),
    ];

    for (case, resource_arn, query) in invisible {
        let answer = list_tags(&router, &resource_arn, query).await;
        assert_denied(case, &answer, &resource_arn);
        assert_eq!(
            message(&answer.body),
            DENIED_MESSAGE,
            "case {case:?}: the refusal states the service's fixed sentence, got {answer:?}"
        );
        assert_eq!(
            reported_tags(&answer.body),
            Value::Null,
            "case {case:?}: a refused read reports no tags, got {answer:?}"
        );
    }
}

/// An ARN this account holds no taggable resource under, and that names no
/// vault, keeps the modelled not-found refusal.
#[tokio::test]
async fn a_tag_read_of_another_kind_s_unheld_arn_is_not_found() {
    let router = router();
    create_vault(&router, "live-vault", json!({"team": "storage"})).await;

    // The ARN forms AWS Backup mints for the other kinds this service stores
    // (state::recovery_point_arn, plan_arn, framework_arn, legal_hold_arn,
    // tiering_configuration_arn), plus an ARN of another service and a
    // vault-shaped resource field in another service's namespace.
    let unheld: Vec<(&str, String)> = vec![
        (
            "a recovery point nothing answers to",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:does-not-exist"),
        ),
        (
            "a backup plan nothing answers to",
            format!(
                "arn:aws:backup:{REGION}:{ACCOUNT}:backup-plan:\
                 00000000-0000-0000-0000-000000000000"
            ),
        ),
        (
            "a framework nothing answers to",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:framework:absent-1"),
        ),
        (
            "a legal hold nothing answers to",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:legal-hold:absent"),
        ),
        (
            "a tiering configuration nothing answers to",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:tiering-configuration:absent"),
        ),
        (
            "a resource of another service",
            format!("arn:aws:sns:{REGION}:{ACCOUNT}:backup-notifications"),
        ),
        (
            "a vault-shaped resource field in another service's namespace",
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:backup-vault:live-vault"),
        ),
    ];

    for (case, resource_arn) in unheld {
        let answer = list_tags(&router, &resource_arn, "").await;
        assert_eq!(
            (
                answer.status,
                error_code(&answer.body).as_str(),
                message(&answer.body).as_str()
            ),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?}: expected the modelled not-found refusal, got {answer:?}"
        );
    }
}

/// The refusal is about the STATE the ARN resolves against, not about the ARN
/// string: every ARN this account does hold a resource under still answers its
/// own tags, and the refused ARN answers again once its name holds a vault.
#[tokio::test]
async fn a_resolvable_arn_still_answers_its_own_tags() {
    let router = router();
    let live = create_vault(&router, "live-vault", json!({"team": "storage"})).await;
    let doomed = create_vault(&router, "doomed-vault", json!({"team": "storage"})).await;
    delete_vault(&router, "doomed-vault").await;

    // The deleted vault's refusal left its sibling alone.
    let refused = list_tags(&router, &doomed, "").await;
    assert_denied("the deleted vault's ARN", &refused, &doomed);
    let read = list_tags(&router, &live, "").await;
    assert_eq!(
        (read.status, reported_tags(&read.body)),
        (200, json!({"team": "storage"})),
        "case \"the vault that was not deleted\": its ARN must still read its tags, got {read:?}"
    );

    // The name is free again, and the ARN that was refused now resolves to the
    // vault created under it — reporting THAT vault's tags.
    let recreated = create_vault(&router, "doomed-vault", json!({"team": "archive"})).await;
    assert_eq!(
        recreated, doomed,
        "case \"the re-created vault\": a vault's ARN is its name's, got {recreated:?}"
    );
    let read = list_tags(&router, &doomed, "").await;
    assert_eq!(
        (read.status, reported_tags(&read.body)),
        (200, json!({"team": "archive"})),
        "case \"the re-created vault\": its ARN must read the new vault's tags, got {read:?}"
    );
}
