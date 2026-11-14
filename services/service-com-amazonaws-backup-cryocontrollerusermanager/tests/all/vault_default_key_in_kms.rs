// SPDX-License-Identifier: CC0-1.0

//! The KMS key a backup vault publishes, driven as real HTTP against Backup AND
//! KMS mounted on one engine.
//!
//! The rule under test: the `EncryptionKeyArn` a vault reports names a key KMS
//! itself holds. A vault created with no key of its own is encrypted under the
//! Amazon Web Services managed key of this service — "if you do not specify a
//! key, Backup creates a KMS key for you by default"
//! (`smithy_query(query="doc",shape="CreateBackupVaultInput",member="EncryptionKeyArn")`),
//! the key behind the predefined `alias/aws/backup` alias of that account and
//! Region — so `kms:DescribeKey` of the published ARN answers that key's
//! metadata, `KeyManager` `AWS`, and `kms:ListAliases` names the alias beside it;
//! a vault created WITH a customer managed key publishes that key, which the same
//! read resolves to `KeyManager` `CUSTOMER`.
//!
//! The rows differ in how the vault got its key (no key at all, a customer key
//! created through KMS's own wire, a second vault of the same Region, a vault of
//! another vault type), so what the rule generalises over is which key the vault
//! holds — never one run's generated key id, which is read off the response
//! rather than written here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, REGION};

/// The alias KMS holds this service's Amazon Web Services managed key under.
const MANAGED_KEY_ALIAS: &str = "alias/aws/backup";

/// `KeyManager` of a key Amazon Web Services manages, and of one the customer
/// does (`smithy_query(query="shape",shape="KeyManagerType")` in the KMS model).
const KEY_MANAGER_AWS: &str = "AWS";
const KEY_MANAGER_CUSTOMER: &str = "CUSTOMER";

/// `KeyState` of a key that can be used (the KMS model's `KeyState` enum).
const KEY_STATE_ENABLED: &str = "Enabled";

/// Backup AND KMS on ONE engine, plus that engine's router: the key a vault
/// publishes is KMS's own resource, so KMS has to be mounted for the vault's key
/// to resolve at all.
fn router_with_kms() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        engine,
    ));
    router
}

/// Call one KMS operation: `awsJson1_1`, `POST /` with
/// `X-Amz-Target: TrentService.<Operation>`, signed for [`REGION`].
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/kms/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the KMS request"),
        ))
        .expect("a well-formed KMS request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// Create a vault named `name` with the request `body`, and answer the
/// `EncryptionKeyArn` a later `DescribeBackupVault` of it reports.
async fn vault_key(router: &MultiServiceRouter, path: &str, name: &str, body: &Value) -> String {
    let (status, created) = call(
        router,
        "PUT",
        &format!("{path}/{name}"),
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: creating the vault {name:?} expected 200, got {status} {created:?}"
    );
    let (status, described) = call(router, "GET", &format!("/backup-vaults/{name}"), None).await;
    assert_eq!(
        status, 200,
        "setup: DescribeBackupVault {name:?} expected 200, got {status} {described:?}"
    );
    let key = member(&described, "EncryptionKeyArn");
    assert!(
        key.starts_with("arn:aws:kms:"),
        "setup: the vault {name:?} must publish a KMS key ARN, got {described:?}"
    );
    key
}

/// Assert `key` resolves through `kms:DescribeKey` to a key of `manager`,
/// reported under the very ARN the vault published.
async fn assert_resolves(router: &MultiServiceRouter, case: &str, key: &str, manager: &str) {
    let (status, described) = kms(router, "DescribeKey", &json!({"KeyId": key})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected kms:DescribeKey({key}) to answer 200, got {status} \
         {described:?}"
    );
    let metadata = &described["KeyMetadata"];
    assert_eq!(
        (
            member(metadata, "Arn"),
            member(metadata, "KeyManager"),
            member(metadata, "KeyState"),
        ),
        (
            key.to_string(),
            manager.to_string(),
            KEY_STATE_ENABLED.to_string(),
        ),
        "case {case:?}: expected the key the vault published to resolve as an enabled \
         {manager} key, got {described:?}"
    );
}

/// A vault created with no key of its own publishes the Amazon Web Services
/// managed key of this service, and that key resolves in KMS — behind the
/// predefined alias, enabled, and the same key for every such vault of the
/// Region.
#[tokio::test]
async fn a_vault_with_no_key_publishes_the_aws_managed_key_kms_resolves() {
    let router = router_with_kms();

    // (case, the path the vault is created under, the create body)
    let rows = vec![
        (
            "an ordinary vault with no key",
            "/backup-vaults",
            "default-one",
            json!({}),
        ),
        (
            "a second ordinary vault of the same Region",
            "/backup-vaults",
            "default-two",
            json!({}),
        ),
        (
            "a logically air-gapped vault with no key",
            "/logically-air-gapped-backup-vaults",
            "default-gapped",
            json!({"MinRetentionDays": 7, "MaxRetentionDays": 365}),
        ),
    ];

    let mut published = Vec::new();
    for (case, path, name, body) in rows {
        let key = vault_key(&router, path, name, &body).await;
        assert_resolves(&router, case, &key, KEY_MANAGER_AWS).await;
        published.push((case, key));
    }

    // One Region, one managed key: every vault that named none publishes the
    // same key, which is what makes "the account's default Backup key" a single
    // resource rather than one per vault.
    let first = published[0].1.clone();
    for (case, key) in &published {
        assert_eq!(
            (*key).clone(),
            first,
            "case {case:?}: every vault of one Region with no key of its own must publish one \
             key, got {key} beside {first}"
        );
    }

    // ...and the key the vaults published is the one the predefined alias names,
    // which `kms:ListAliases` reports beside it.
    let (status, listed) = kms(&router, "ListAliases", &json!({"KeyId": first})).await;
    let names: Vec<String> = listed["Aliases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|alias| member(alias, "AliasName"))
        .collect();
    assert!(
        status == 200 && names.iter().any(|name| name == MANAGED_KEY_ALIAS),
        "expected kms:ListAliases of the published key to report {MANAGED_KEY_ALIAS}, got \
         {status} {listed:?}"
    );
}

/// A vault created WITH a customer managed key publishes that key, and the same
/// read resolves it as the customer's — so the two kinds of key are told apart
/// by what KMS says about them rather than by the vault's own report.
#[tokio::test]
async fn a_vault_with_a_customer_key_publishes_that_key() {
    let router = router_with_kms();
    let (status, created) = kms(&router, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "setup: kms:CreateKey expected 200, got {status} {created:?}"
    );
    let customer_key = member(&created["KeyMetadata"], "Arn");

    let key = vault_key(
        &router,
        "/backup-vaults",
        "cmk-vault",
        &json!({"EncryptionKeyArn": customer_key}),
    )
    .await;
    assert_eq!(
        key, customer_key,
        "expected the vault to publish the key it was created with, got {key}"
    );
    assert_resolves(&router, "a customer managed key", &key, KEY_MANAGER_CUSTOMER).await;

    // Its twin, created with no key, publishes a DIFFERENT key — the managed one
    // — and that key resolves too.
    let managed = vault_key(&router, "/backup-vaults", "twin-vault", &json!({})).await;
    assert_ne!(
        managed, customer_key,
        "expected the no-key twin to publish another key than the CMK, got {managed}"
    );
    assert_resolves(&router, "the no-key twin", &managed, KEY_MANAGER_AWS).await;
}
