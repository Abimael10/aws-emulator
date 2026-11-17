// SPDX-License-Identifier: CC0-1.0

//! The KMS key a backup vault publishes, driven as real HTTP against Backup AND
//! KMS mounted on one engine.
//!
//! The rule under test: the `EncryptionKeyArn` a vault reports names a key KMS
//! itself holds, in the account and Region the vault belongs to. A vault created
//! with no key of its own is encrypted under the Amazon Web Services managed key
//! of this service — "if you do not specify a key, Backup creates a KMS key for
//! you by default"
//! (`smithy_query(query="doc",shape="CreateBackupVaultInput",member="EncryptionKeyArn")`),
//! the key behind the predefined `alias/aws/backup` alias of that account and
//! Region — so `kms:DescribeKey` of the published ARN answers that key's
//! metadata, `KeyManager` `AWS`, and `kms:ListAliases` names the alias on it;
//! a vault created WITH a customer managed key publishes that key, which the same
//! read resolves to `KeyManager` `CUSTOMER` and which carries no alias of Amazon
//! Web Services'.
//!
//! The rows differ in how the vault got its key (no key at all, a customer key
//! created through KMS's own wire, a second vault of the same Region, a vault of
//! another vault type) and in the REGION the create was signed for, because a
//! Region is a half of the identity of both a vault and a KMS key: what the rule
//! generalises over is which key the vault holds — never one run's generated key
//! id, which is read off the response rather than written here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call_in_region, error_code, REGION};

/// A second Region, so the suite can tell a Region's own default key from the
/// one key an account-wide default would be.
const OTHER_REGION: &str = "eu-central-1";

/// The alias KMS holds this service's Amazon Web Services managed key under.
const MANAGED_KEY_ALIAS: &str = "alias/aws/backup";

/// `KeyManager` of a key Amazon Web Services manages, and of one the customer
/// does (`smithy_query(query="shape",shape="KeyManagerType")` in the KMS model).
const KEY_MANAGER_AWS: &str = "AWS";
const KEY_MANAGER_CUSTOMER: &str = "CUSTOMER";

/// `KeyState` of a key that can be used (the KMS model's `KeyState` enum).
const KEY_STATE_ENABLED: &str = "Enabled";

/// How KMS refuses a key ARN of ANOTHER Region — "the request was rejected
/// because the specified entity or resource could not be found"
/// (`smithy_query(query="shape",shape="NotFoundException")` in the KMS model),
/// which that service answers for a cross-Region ARN as an HTTP 400 beside the
/// code, the form its own suites pin (`tests/all/import_parameters.rs`,
/// `tests/all/client_error_status.rs` of the KMS crate). The shape's modelled
/// `httpError` is 404, which is the status a KMS refusal of an identifier KMS
/// holds no key for carries; the 400 here is the recorded status of the
/// cross-Region ARN form in particular, so this is a DIFFERENT refusal rather
/// than a relaxed expectation of the one the report quoted. This suite asserts
/// only that the key does not resolve there, and names the refusal rather than
/// settling for "not a success".
const NOT_FOUND: &str = "NotFoundException";
const NOT_FOUND_STATUS: u16 = 400;

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
/// `X-Amz-Target: TrentService.<Operation>`, signed for `region` — the Region
/// whose endpoint the keys it answers for live in.
async fn kms(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{region}/kms/aws4_request, \
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

/// The error shape an error body names, without the namespace an `awsJson`
/// `__type` may carry (`com.amazonaws.kms#NotFoundException` names the SAME
/// shape as `NotFoundException`), so the assertion is about the refusal rather
/// than about how the wire spelled its prefix.
fn refusal_code(body: &Value) -> String {
    let code = error_code(body);
    code.rsplit(['#', ':']).next().unwrap_or(&code).to_string()
}

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// Create a vault named `name` in `region` with the request `body`, and answer
/// the `EncryptionKeyArn` a later `DescribeBackupVault` of it reports.
async fn vault_key(
    router: &MultiServiceRouter,
    region: &str,
    path: &str,
    name: &str,
    body: &Value,
) -> String {
    let (status, created) = call_in_region(
        router,
        region,
        "PUT",
        &format!("{path}/{name}"),
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: creating the vault {name:?} in {region} expected 200, got {status} {created:?}"
    );
    let (status, described) = call_in_region(
        router,
        region,
        "GET",
        &format!("/backup-vaults/{name}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeBackupVault {name:?} in {region} expected 200, got {status} {described:?}"
    );
    let key = member(&described, "EncryptionKeyArn");
    assert!(
        key.starts_with(&format!("arn:aws:kms:{region}:")),
        "setup: the vault {name:?} must publish a KMS key ARN of {region}, got {described:?}"
    );
    key
}

/// Assert `key` resolves through `kms:DescribeKey` at `region`'s endpoint to a
/// key of `manager`, reported under the very ARN the vault published, and that
/// `kms:ListAliases` names the predefined Backup alias on that key when
/// `aliased` — and names it on no key of the customer's when it does not.
async fn assert_resolves(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    key: &str,
    manager: &str,
    aliased: bool,
) {
    let (status, described) = kms(router, region, "DescribeKey", &json!({"KeyId": key})).await;
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
    let key_id = member(metadata, "KeyId");

    let (status, listed) = kms(router, region, "ListAliases", &json!({"KeyId": key})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected kms:ListAliases({key}) to answer 200, got {status} {listed:?}"
    );
    let target = listed["Aliases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|alias| member(alias, "AliasName") == MANAGED_KEY_ALIAS)
        .map(|alias| member(&alias, "TargetKeyId"));
    if aliased {
        assert_eq!(
            target.as_deref(),
            Some(key_id.as_str()),
            "case {case:?}: expected kms:ListAliases({key}) to report {MANAGED_KEY_ALIAS} on key \
             {key_id}, got {listed:?}"
        );
    } else {
        assert_eq!(
            target, None,
            "case {case:?}: expected the customer's own key {key} to carry no \
             {MANAGED_KEY_ALIAS} alias, got {listed:?}"
        );
    }
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
        let key = vault_key(&router, REGION, path, name, &body).await;
        assert_resolves(&router, case, REGION, &key, KEY_MANAGER_AWS, true).await;
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
}

/// A vault created WITH a customer managed key publishes that key, and the same
/// read resolves it as the customer's — so the two kinds of key are told apart
/// by what KMS says about them rather than by the vault's own report.
#[tokio::test]
async fn a_vault_with_a_customer_key_publishes_that_key() {
    let router = router_with_kms();
    let (status, created) = kms(&router, REGION, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "setup: kms:CreateKey expected 200, got {status} {created:?}"
    );
    let customer_key = member(&created["KeyMetadata"], "Arn");

    let key = vault_key(
        &router,
        REGION,
        "/backup-vaults",
        "cmk-vault",
        &json!({"EncryptionKeyArn": customer_key}),
    )
    .await;
    assert_eq!(
        key, customer_key,
        "expected the vault to publish the key it was created with, got {key}"
    );
    assert_resolves(
        &router,
        "a customer managed key",
        REGION,
        &key,
        KEY_MANAGER_CUSTOMER,
        false,
    )
    .await;

    // Its twin, created with no key, publishes a DIFFERENT key — the managed one
    // — and that key resolves too.
    let managed = vault_key(&router, REGION, "/backup-vaults", "twin-vault", &json!({})).await;
    assert_ne!(
        managed, customer_key,
        "expected the no-key twin to publish another key than the CMK, got {managed}"
    );
    assert_resolves(
        &router,
        "the no-key twin",
        REGION,
        &managed,
        KEY_MANAGER_AWS,
        true,
    )
    .await;
}

/// The default key belongs to the Region the vault was created in: a vault of
/// another Region publishes THAT Region's managed key, which resolves at that
/// Region's endpoint — and one Region's key is not a key of the other's.
#[tokio::test]
async fn another_regions_vault_publishes_that_regions_own_default_key() {
    let router = router_with_kms();
    let here = vault_key(&router, REGION, "/backup-vaults", "region-local", &json!({})).await;
    assert_resolves(
        &router,
        "a vault of the suite's Region",
        REGION,
        &here,
        KEY_MANAGER_AWS,
        true,
    )
    .await;

    let elsewhere = vault_key(
        &router,
        OTHER_REGION,
        "/backup-vaults",
        "region-remote",
        &json!({}),
    )
    .await;
    assert_resolves(
        &router,
        "a vault of another Region",
        OTHER_REGION,
        &elsewhere,
        KEY_MANAGER_AWS,
        true,
    )
    .await;
    assert_ne!(
        elsewhere, here,
        "case {:?}: expected {OTHER_REGION}'s default key to be that Region's own, got {here} \
         again",
        "a vault of another Region"
    );

    // ...and that Region half is load-bearing: the other Region's endpoint holds
    // no key of this one, so a vault's published key resolves where the vault is,
    // and asking elsewhere is the refusal KMS models for a key it does not hold.
    let (status, refused) = kms(&router, OTHER_REGION, "DescribeKey", &json!({"KeyId": here})).await;
    assert_eq!(
        (status, refusal_code(&refused)),
        (NOT_FOUND_STATUS, NOT_FOUND.to_string()),
        "case {:?}: expected {OTHER_REGION}'s KMS to refuse {REGION}'s key {here} with HTTP \
         {NOT_FOUND_STATUS} {NOT_FOUND}, got {status} {refused:?}",
        "one Region's key at another Region's endpoint"
    );
}
