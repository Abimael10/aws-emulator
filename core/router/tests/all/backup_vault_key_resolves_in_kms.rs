// SPDX-License-Identifier: CC0-1.0

//! A stack-declared backup vault publishes a KMS key that KMS resolves.
//!
//! The RULE pinned here: **the `EncryptionKeyArn` a backup vault reports names a KMS key
//! that exists in the same account and Region, whoever created the vault — so a client
//! that deployed an `AWS::Backup::BackupVault` and hands that very ARN back to KMS is
//! answered the key rather than refused it. A vault declared WITHOUT `EncryptionKeyArn` is
//! encrypted under the account's Amazon Web Services managed key of this service ("if you
//! do not specify a key, Backup creates a KMS key for you by default",
//! `smithy_query(query="doc",shape="CreateBackupVaultInput",member="EncryptionKeyArn")`),
//! the key the predefined `alias/aws/backup` alias of that Region names; a vault that
//! declares a key publishes that key, which the same read tells apart by its
//! `KeyManager`.**
//!
//! THREE crates have to agree for that to hold, which is why the suite is here: the stack
//! provisions the vault in AWS Backup itself rather than recording one of its own, AWS
//! Backup decides which key the vault is encrypted under, and KMS is the service that
//! holds keys and aliases. The audit step a client makes after deploying a stack —
//! `DescribeBackupVault` into `kms:DescribeKey`/`kms:ListAliases`, to tell an Amazon Web
//! Services managed vault from a customer-key one — crosses all three.
//!
//! The rows differ in what the DECLARATION says about the key (none at all, and a customer
//! managed key made through KMS's own wire), which is what this level of the rule
//! generalises over; the vault TYPES and the Regions it generalises over are rows of the
//! Backup crate's own suite
//! (`services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/vault_default_key_in_kms.rs`),
//! which drives the two services directly. No row writes a key id: every ARN asserted on
//! is the one the vault reported, because any one run's generated id is no part of the
//! requirement.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The shared stack reader of this test crate, its awsQuery CloudFormation caller, its
// SigV4 header and its XML output reader, written ONCE rather than per suite. They sign
// for the Region below, which is why this suite works in it.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::rds_docdb_neptune_signing_name_routing::authorization;
use crate::sns_lambda_subscription_permission_boundary::{cloudformation, CREATE_COMPLETE};

/// The Region every request below is signed for — the one the shared callers sign for.
const REGION: &str = "us-east-1";

/// The predefined alias KMS holds the Amazon Web Services managed key of AWS Backup
/// under — `alias/aws/<service-name>` for this service's endpoint prefix (`backup`) —
/// spelled out here rather than imported from either crate, so a wrong constant in an
/// implementation cannot make this suite pass.
const MANAGED_KEY_ALIAS: &str = "alias/aws/backup";

/// `KeyManager` of a key Amazon Web Services manages and of one the customer does
/// (`smithy_query(query="shape",shape="KeyManagerType")` in the KMS model).
const KEY_MANAGER_AWS: &str = "AWS";
const KEY_MANAGER_CUSTOMER: &str = "CUSTOMER";

/// `KeyState` of a key that can be used (the KMS model's `KeyState` enum).
const KEY_STATE_ENABLED: &str = "Enabled";

/// CloudFormation, AWS Backup and KMS over ONE engine, as `core/router/src/lib.rs` mounts
/// them: the stack provisions the vault in AWS Backup through the in-process callback, and
/// the key that vault publishes is KMS's own resource.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
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

/// The status and parsed body of one dispatched request (`Value::Null` for an empty or
/// unparseable body).
async fn answered(router: &MultiServiceRouter, request: http::Request<Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One AWS Backup call (`restJson1`: a method and a URI of its own), which must answer 200.
async fn backup(router: &MultiServiceRouter, case: &str, method: &str, uri: &str) -> Value {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("backup.{REGION}.amazonaws.com"))
        .header("authorization", authorization("backup"))
        .body(Bytes::new())
        .expect("a well-formed test request");
    let (status, parsed) = answered(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One KMS call (`awsJson1_1`, `X-Amz-Target: TrentService.<Operation>`), with whatever
/// status it answered — a refusal is what this round trip must NOT produce, so the status
/// is the caller's to assert.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    answered(router, request).await
}

/// A body member as a string, or `"<unset>"` when the member is absent — so an assertion
/// reports a missing member rather than panicking on it.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// One stack `Output`, or the case's failure.
fn output(case: &str, described: &str, name: &str) -> String {
    outputs_of(described)
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the stack to publish {name}"))
}

/// The template one row deploys: a single `AWS::Backup::BackupVault` of the declared name,
/// carrying `EncryptionKeyArn` only where the row declares one, and publishing what `Ref`
/// returns for the type (the vault's name) so the client reads the vault back by the id the
/// stack reported.
fn template(vault: &str, declared_key: Option<&str>) -> String {
    let mut properties = json!({"BackupVaultName": vault});
    if let Some(key) = declared_key {
        properties["EncryptionKeyArn"] = json!(key);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a backup vault whose encryption key a client resolves in KMS",
        "Resources": {
            "Vault": {
                "Type": "AWS::Backup::BackupVault",
                "Properties": properties
            }
        },
        "Outputs": {
            "VaultName": { "Value": { "Ref": "Vault" } }
        }
    })
    .to_string()
}

/// RULE: the key a stack-declared vault publishes resolves in KMS — the account's Amazon
/// Web Services managed Backup key where the declaration named none, and the declared key
/// where it did.
#[tokio::test]
async fn a_stack_declared_vaults_key_resolves_in_kms() {
    // ONE emulator for the whole table: each row names its own stack and its own vault, so
    // no row can see another's.
    let router = router();

    // A customer managed key, made through KMS's own wire so a row's declaration names a
    // key that service really holds rather than an ARN this suite wrote.
    let (status, created) = kms(&router, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "setup: expected kms:CreateKey to answer 200, got {status} with {created}"
    );
    let customer_key = member(&created["KeyMetadata"], "Arn");

    // (case, stack name, vault name, the key the declaration carries, the expected
    //  KeyManager, whether the published key carries the predefined Backup alias)
    let rows = vec![
        (
            "a vault declared with no EncryptionKeyArn",
            "stack-vault-default-key",
            "stack-vault-default-key",
            None,
            KEY_MANAGER_AWS,
            true,
        ),
        (
            "a vault declaring a customer managed key",
            "stack-vault-declared-key",
            "stack-vault-declared-key",
            Some(customer_key.as_str()),
            KEY_MANAGER_CUSTOMER,
            false,
        ),
    ];

    for (case, stack, vault, declared_key, manager, aliased) in rows {
        // The shared caller asserts the create itself answered 200, so a refused
        // CreateStack names the create rather than surfacing as a wait that never ends.
        cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &template(vault, declared_key)),
            ],
        )
        .await;
        let (status, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: expected the vault stack to deploy, got {status} with {described}"
        );
        assert_eq!(
            output(case, &described, "VaultName"),
            vault,
            "case {case:?}: expected the stack to publish the vault it declared, got {described}"
        );

        // The audit step: read the vault back from AWS Backup, then hand the key it
        // published straight to KMS.
        let read = backup(&router, case, "GET", &format!("/backup-vaults/{vault}")).await;
        let key = member(&read, "EncryptionKeyArn");
        assert!(
            key.starts_with(&format!("arn:aws:kms:{REGION}:")),
            "case {case:?}: expected the deployed vault to publish a KMS key ARN of {REGION}, \
             got {read}"
        );
        match declared_key {
            Some(declared) => assert_eq!(
                key, declared,
                "case {case:?}: expected the vault to publish the key it was declared with \
                 ({declared}), got {key}"
            ),
            None => assert_ne!(
                key, customer_key,
                "case {case:?}: expected a vault that declared no key to publish another key \
                 than the customer's, got {key}"
            ),
        }

        let (status, metadata) = kms(&router, "DescribeKey", &json!({"KeyId": key})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected kms:DescribeKey of the published key {key} to answer 200, \
             got {status} with {metadata}"
        );
        let metadata = &metadata["KeyMetadata"];
        assert_eq!(
            (
                member(metadata, "Arn"),
                member(metadata, "KeyManager"),
                member(metadata, "KeyState"),
            ),
            (
                key.clone(),
                manager.to_string(),
                KEY_STATE_ENABLED.to_string(),
            ),
            "case {case:?}: expected the published key {key} to resolve as an enabled {manager} \
             key, got {metadata}"
        );
        let key_id = member(metadata, "KeyId");

        let (status, listed) = kms(&router, "ListAliases", &json!({"KeyId": key})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected kms:ListAliases of the published key {key} to answer 200, \
             got {status} with {listed}"
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
                "case {case:?}: expected kms:ListAliases of {key} to report {MANAGED_KEY_ALIAS} \
                 on key {key_id}, got {listed}"
            );
        } else {
            assert_eq!(
                target, None,
                "case {case:?}: expected the customer's own key {key} to carry no \
                 {MANAGED_KEY_ALIAS} alias, got {listed}"
            );
        }
    }
}
