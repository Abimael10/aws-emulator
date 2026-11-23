// SPDX-License-Identifier: CC0-1.0

//! WHICH KMS key a repository is encrypted under, as the repository reports it.
//!
//! RULE: a repository encrypted under a KMS encryption type reports the KEY its contents are
//! protected with — `encryptionConfiguration.kmsKey` is the ARN KMS resolves the create's
//! reference to, and for a configuration that names no reference it is the ARN of this account's
//! and Region's Amazon Web Services managed key for Amazon ECR, the key the predefined
//! `alias/aws/ecr` alias stands for: "the alias, key ID, or full ARN of the KMS key can be
//! specified. The key must exist in the same Region as the repository. If no key is specified,
//! the default Amazon Web Services managed KMS key for Amazon ECR will be used"
//! (`smithy_query(query="shape",shape="EncryptionConfiguration")`, whose `encryptionType` doc
//! repeats it for both KMS types). A repository encrypted with an Amazon S3-managed key
//! (`AES256`, and the default when no configuration is named) is under no KMS key and reports
//! none. Every reader of the repository reports what the create resolved, so
//! `DescribeRepositories` names the same key.
//!
//! The key is resolved through the in-process callback, so a single-service router has no KMS to
//! resolve it against: that is why this suite lives here rather than in the service's own crate.
//!
//! The rows span what the rule generalises over: the encryption type (`KMS`, `KMS_DSSE`,
//! `AES256`, and no configuration at all) crossed with the FORM the key is referenced by — its
//! full ARN, its key id, an alias of it, and no reference at all, the forms
//! `EncryptionConfiguration$kmsKey` admits.
//!
//! No row writes a key id or an ARN: the account, the Region and the key id of an Amazon Web
//! Services managed key are environment values minted per run, so every expectation is read from
//! what KMS itself answered about the key.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for. A KMS key is regional and ECR requires the key
/// to be in the repository's own Region, so it is also where every key is resolved.
const REGION: &str = "eu-west-1";

/// The `X-Amz-Target` prefix ECR routes on.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The `X-Amz-Target` prefix KMS routes on (its service shape is `TrentService`).
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The predefined Amazon Web Services alias of the Amazon Web Services managed key for Amazon
/// ECR, written out here rather than imported so a service resolving another alias fails this
/// suite. It has the `aws/<service-name>` form KMS publishes for an Amazon Web Services managed
/// key, with this service's own endpoint name.
const AWS_MANAGED_KEY_ALIAS: &str = "alias/aws/ecr";

/// The `KeyManager` KMS reports for a key AWS manages on the account's behalf, as that enum
/// names it (`smithy_query(query="shape",shape="KeyManagerType")` in the KMS model lists `AWS`
/// and `CUSTOMER`).
const AWS_KEY_MANAGER: &str = "AWS";

/// ECR and KMS over ONE shared engine, as `router/src/lib.rs` mounts them, so ECR's in-process
/// callback reaches this very KMS.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call, signed for [`REGION`], answered with the status and the JSON body.
async fn call(
    router: &MultiServiceRouter,
    host: &str,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One ECR call, answered or a panic naming the case and what came back instead.
async fn ecr(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(
        router,
        &format!("api.ecr.{REGION}.amazonaws.com"),
        "ecr",
        &format!("{ECR_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ecr:{operation}, got {status} with {answer}"
    );
    answer
}

/// One KMS call, answered or a panic naming the case and what came back instead.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(
        router,
        &format!("kms.{REGION}.amazonaws.com"),
        "kms",
        &format!("{KMS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from kms:{operation}, got {status} with {answer}"
    );
    answer
}

/// A member of `value` as a string, or `"<unset>"` when it is absent — so an assertion reports
/// the missing member rather than panicking on it.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The `KeyMetadata` KMS reports for the key `reference` names.
async fn described_key(router: &MultiServiceRouter, case: &str, reference: &str) -> Value {
    let described = kms(router, case, "DescribeKey", &json!({"KeyId": reference})).await;
    described["KeyMetadata"].clone()
}

/// The ARN KMS resolves `reference` to.
async fn key_arn(router: &MultiServiceRouter, case: &str, reference: &str) -> String {
    member(&described_key(router, case, reference).await, "Arn")
}

/// The `encryptionConfiguration` of the repository `name`, as `DescribeRepositories` reports it.
async fn described_encryption(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let described = ecr(
        router,
        case,
        "DescribeRepositories",
        &json!({"repositoryNames": [name]}),
    )
    .await;
    described["repositories"][0]["encryptionConfiguration"].clone()
}

/// How a row references the key it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    /// The row names no `kmsKey` at all, so the repository is encrypted under the account's
    /// Amazon Web Services managed key for Amazon ECR.
    None,
    /// The row names the customer managed key by its full ARN.
    KeyArn,
    /// The row names the customer managed key by its key id.
    KeyId,
    /// The row names the customer managed key by an alias of it.
    Alias,
}

/// What a row expects the repository to report as the key it is encrypted under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The ARN of the account's Amazon Web Services managed key for Amazon ECR.
    Managed,
    /// The ARN of the customer managed key the row referenced.
    Customer,
    /// No `kmsKey` member at all: the repository is under no KMS key.
    Absent,
}

/// One row: an encryption configuration, and the key the repository it creates must report.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    name: &'static str,
    /// `None` for a request that names no `encryptionConfiguration` at all.
    encryption_type: Option<&'static str>,
    reference: Reference,
    expected: Expected,
}

/// RULE: every KMS-encrypted repository reports the KEY it is encrypted under — the ARN KMS
/// resolves the reference to, and the account's Amazon Web Services managed key for Amazon ECR
/// when the configuration names none — and a repository under an Amazon S3-managed key reports
/// no key; `DescribeRepositories` reports the same as the create.
#[tokio::test]
async fn every_repository_reports_the_key_its_contents_are_encrypted_under() {
    let router = emulator();

    // The customer managed key the rows reference, made through KMS's own wire — so every row
    // names a key that service really holds rather than an ARN this suite wrote — reachable by
    // each of the three forms `kmsKey` admits.
    let created = kms(&router, "setup", "CreateKey", &json!({})).await;
    let customer_key_arn = member(&created["KeyMetadata"], "Arn");
    let customer_key_id = member(&created["KeyMetadata"], "KeyId");
    let customer_alias = "alias/ecr-encryption-suite";
    kms(
        &router,
        "setup",
        "CreateAlias",
        &json!({"AliasName": customer_alias, "TargetKeyId": customer_key_id}),
    )
    .await;

    let rows = [
        Row {
            case: "KMS naming no key",
            name: "kms-managed-default",
            encryption_type: Some("KMS"),
            reference: Reference::None,
            expected: Expected::Managed,
        },
        Row {
            case: "KMS_DSSE naming no key",
            name: "kms-dsse-managed-default",
            encryption_type: Some("KMS_DSSE"),
            reference: Reference::None,
            expected: Expected::Managed,
        },
        Row {
            case: "KMS naming the key by its ARN",
            name: "kms-by-arn",
            encryption_type: Some("KMS"),
            reference: Reference::KeyArn,
            expected: Expected::Customer,
        },
        Row {
            case: "KMS naming the key by its key id",
            name: "kms-by-key-id",
            encryption_type: Some("KMS"),
            reference: Reference::KeyId,
            expected: Expected::Customer,
        },
        Row {
            case: "KMS naming the key by an alias of it",
            name: "kms-by-alias",
            encryption_type: Some("KMS"),
            reference: Reference::Alias,
            expected: Expected::Customer,
        },
        Row {
            case: "KMS_DSSE naming the key by an alias of it",
            name: "kms-dsse-by-alias",
            encryption_type: Some("KMS_DSSE"),
            reference: Reference::Alias,
            expected: Expected::Customer,
        },
        Row {
            case: "the Amazon S3-managed encryption type",
            name: "s3-managed",
            encryption_type: Some("AES256"),
            reference: Reference::None,
            expected: Expected::Absent,
        },
        Row {
            case: "no encryption configuration at all",
            name: "no-configuration",
            encryption_type: None,
            reference: Reference::None,
            expected: Expected::Absent,
        },
    ];

    // Read AFTER the rows have run, so the first create naming no key is what associates the
    // predefined alias with the account's managed key: the ARN below is the one KMS reports for
    // that alias, whoever asked for it first.
    let mut reported: Vec<(Row, Value, Value)> = Vec::new();
    for row in rows {
        let mut request = json!({"repositoryName": row.name});
        if let Some(encryption_type) = row.encryption_type {
            let mut configuration = json!({"encryptionType": encryption_type});
            let reference = match row.reference {
                Reference::None => None,
                Reference::KeyArn => Some(customer_key_arn.clone()),
                Reference::KeyId => Some(customer_key_id.clone()),
                Reference::Alias => Some(customer_alias.to_string()),
            };
            if let Some(reference) = reference {
                configuration["kmsKey"] = json!(reference);
            }
            request["encryptionConfiguration"] = configuration;
        }
        let created = ecr(&router, row.case, "CreateRepository", &request).await;
        let on_create = created["repository"]["encryptionConfiguration"].clone();
        let on_read = described_encryption(&router, row.case, row.name).await;
        reported.push((row, on_create, on_read));
    }

    let managed_key_arn = key_arn(&router, "the managed key", AWS_MANAGED_KEY_ALIAS).await;
    assert_ne!(
        managed_key_arn, customer_key_arn,
        "the Amazon Web Services managed key for Amazon ECR must be a different key from the \
         customer managed key, got {managed_key_arn}"
    );

    for (row, on_create, on_read) in reported {
        let case = row.case;
        // The default encryption type when the request names no configuration: "by default,
        // when no encryption configuration is set … Amazon ECR uses server-side encryption with
        // Amazon S3-managed encryption keys".
        let encryption_type = row.encryption_type.unwrap_or("AES256");
        let expected = match row.expected {
            Expected::Managed => {
                json!({"encryptionType": encryption_type, "kmsKey": managed_key_arn})
            }
            Expected::Customer => {
                json!({"encryptionType": encryption_type, "kmsKey": customer_key_arn})
            }
            Expected::Absent => json!({"encryptionType": encryption_type}),
        };
        assert_eq!(
            on_create, expected,
            "case {case:?}: expected the create to report {expected}, got {on_create}"
        );
        assert_eq!(
            on_read, expected,
            "case {case:?}: expected DescribeRepositories to report {expected}, got {on_read}"
        );
    }

    // The key the two rows that named none report is one KMS itself manages, which is what
    // "the default Amazon Web Services managed KMS key for Amazon ECR" is: read from KMS's own
    // answer about the key the repositories name.
    let metadata = described_key(&router, "the managed key", &managed_key_arn).await;
    assert_eq!(
        member(&metadata, "KeyManager"),
        AWS_KEY_MANAGER,
        "the key a repository naming no key is encrypted under must be an Amazon Web Services \
         managed key, got {metadata}"
    );
}

/// RULE, the half that is about authorizations: the Amazon Web Services managed key for Amazon
/// ECR is the account's own rather than a key of the caller's, so a repository that is encrypted
/// under it takes no grant on it — the authorizations a repository holds are the ones taken on a
/// key the request NAMES.
#[tokio::test]
async fn the_default_managed_key_carries_no_grant_for_the_repositories_it_encrypts() {
    let router = emulator();

    for (case, name, encryption_type) in [
        ("KMS naming no key", "managed-grantless-kms", "KMS"),
        (
            "KMS_DSSE naming no key",
            "managed-grantless-kms-dsse",
            "KMS_DSSE",
        ),
    ] {
        let created = ecr(
            &router,
            case,
            "CreateRepository",
            &json!({
                "repositoryName": name,
                "encryptionConfiguration": {"encryptionType": encryption_type},
            }),
        )
        .await;
        let reported = member(
            &created["repository"]["encryptionConfiguration"],
            "kmsKey",
        );
        let managed_key_arn = key_arn(&router, case, AWS_MANAGED_KEY_ALIAS).await;
        assert_eq!(
            reported, managed_key_arn,
            "case {case:?}: expected the repository to report the managed key \
             {managed_key_arn}, got {reported}"
        );

        let listed = kms(
            &router,
            case,
            "ListGrants",
            &json!({"KeyId": managed_key_arn}),
        )
        .await;
        let grants = listed["Grants"].as_array().cloned().unwrap_or_default();
        assert!(
            grants.is_empty(),
            "case {case:?}: expected the Amazon Web Services managed key to carry no grant, \
             got {grants:?}"
        );
    }
}
