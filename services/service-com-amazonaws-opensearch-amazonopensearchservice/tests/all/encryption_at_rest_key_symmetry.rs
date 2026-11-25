// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH KIND of KMS key an enabled `EncryptionAtRestOptions` may
//! name — the container `CreateDomain` and `UpdateDomainConfig` both take.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. Every key a row names is minted through the AWS KMS service's OWN `CreateKey` on
//! the same shared engine ([`Emulator::with_kms`]), so the key under test is the key that
//! service holds, of the `KeySpec` and `KeyUsage` it was created with.
//!
//! The rule under test, asserted over a table rather than one call: a configuration that
//! ENABLES encryption at rest is accepted only when the key it names is a SYMMETRIC
//! ENCRYPTION KMS key — one whose key material is a symmetric key and whose usage is
//! encryption and decryption — because the domain's storage is encrypted under that key;
//! a key of any other kind is refused with `ValidationException` (HTTP 400) and neither
//! creates a domain nor changes the encryption of one, whatever key state it is in. A
//! configuration that enables no encryption names no key the domain is encrypted under, so
//! the kind of the value it carries decides nothing.
//!
//! The rows differ along what the rule generalises over: the key's MATERIAL (a symmetric
//! key, an HMAC key, an RSA key pair, three elliptic-curve key pairs, an ML-KEM key pair)
//! and its USAGE (encryption and decryption, signing and verification, MAC generation, key
//! agreement, key encapsulation) — including an asymmetric key whose usage IS encryption
//! and decryption, which the issue behind this suite did not cite and which the rule refuses
//! all the same. Each row is driven through BOTH operations that take the container. The
//! `KeySpec` and `KeyUsage` values the rows name are the KMS model's own
//! (`smithy_query(query="doc",shape="CreateKeyRequest",member="KeySpec")` and
//! `…,member="KeyUsage"` list which usages each spec accepts).

use serde_json::{json, Value};

use super::domain_configuration_rules::{assert_no_domain, assert_refusal};
use super::domain_lifecycle::{
    baseline_create, create_baseline, settled_domain, Emulator, REGION,
};
use super::service_software_update::message;

/// The error every refusal below carries: the operation's modelled 400 for a configuration
/// the service will not provision.
const REFUSAL: &str = "ValidationException";

/// The sentence a failed key access is stated under, and what follows the colon is the
/// detail.
const ENVELOPE: &str = "Error in Accessing KmsKeyID with details:";

/// The alias a case points at a key of the wrong kind, so the reference the caller writes is
/// not a key ARN at all.
const KEY_ALIAS: &str = "alias/opensearch-signing-key";

/// One row of the table: the key to mint, and whether a configuration enabling encryption
/// under it may be served.
struct Row {
    case: &'static str,
    /// The `CreateKey` request KMS mints the key from.
    create_key: Value,
    /// Whether the key is one a domain's data can be encrypted under.
    accepted: bool,
}

/// The table: the kinds of KMS key a caller can name, each minted by KMS itself.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The default: "the default value, SYMMETRIC_DEFAULT, creates a KMS key with a
            // 256-bit AES-GCM key", used for encryption and decryption.
            case: "a symmetric encryption key created with no spec at all",
            create_key: json!({}),
            accepted: true,
        },
        Row {
            case: "a symmetric encryption key, spec and usage both named",
            create_key: json!({ "KeySpec": "SYMMETRIC_DEFAULT", "KeyUsage": "ENCRYPT_DECRYPT" }),
            accepted: true,
        },
        Row {
            case: "an RSA key pair for signing and verification",
            create_key: json!({ "KeySpec": "RSA_2048", "KeyUsage": "SIGN_VERIFY" }),
            accepted: false,
        },
        Row {
            // An asymmetric key whose usage IS encryption and decryption: it encrypts under
            // the public half outside KMS, so it is still not a key a domain's storage is
            // encrypted under.
            case: "an RSA key pair for encryption and decryption",
            create_key: json!({ "KeySpec": "RSA_2048", "KeyUsage": "ENCRYPT_DECRYPT" }),
            accepted: false,
        },
        Row {
            case: "a larger RSA key pair for encryption and decryption",
            create_key: json!({ "KeySpec": "RSA_4096", "KeyUsage": "ENCRYPT_DECRYPT" }),
            accepted: false,
        },
        Row {
            case: "an elliptic-curve key pair for signing and verification",
            create_key: json!({ "KeySpec": "ECC_NIST_P256", "KeyUsage": "SIGN_VERIFY" }),
            accepted: false,
        },
        Row {
            case: "an elliptic-curve key pair for deriving shared secrets",
            create_key: json!({ "KeySpec": "ECC_NIST_P384", "KeyUsage": "KEY_AGREEMENT" }),
            accepted: false,
        },
        Row {
            case: "an Edwards-curve key pair for signing and verification",
            create_key: json!({ "KeySpec": "ECC_NIST_EDWARDS25519", "KeyUsage": "SIGN_VERIFY" }),
            accepted: false,
        },
        Row {
            case: "an SM2 key pair for encryption and decryption",
            create_key: json!({ "KeySpec": "SM2", "KeyUsage": "ENCRYPT_DECRYPT" }),
            accepted: false,
        },
        Row {
            // An HMAC key's material is symmetric, and it still encrypts nothing: its usage
            // is generating and verifying message authentication codes.
            case: "an HMAC key for generating and verifying MACs",
            create_key: json!({ "KeySpec": "HMAC_256", "KeyUsage": "GENERATE_VERIFY_MAC" }),
            accepted: false,
        },
        Row {
            case: "a longer HMAC key for generating and verifying MACs",
            create_key: json!({ "KeySpec": "HMAC_512", "KeyUsage": "GENERATE_VERIFY_MAC" }),
            accepted: false,
        },
        Row {
            case: "an ML-KEM key pair for key encapsulation",
            create_key: json!({
                "KeySpec": "ML_KEM_768",
                "KeyUsage": "ENCAPSULATE_DECAPSULATE",
            }),
            accepted: false,
        },
    ]
}

/// Mint `create_key`'s key through KMS's own `CreateKey` at the domain's Region, answering
/// the key ARN it minted — the value an `EncryptionAtRestOptions$KmsKeyId` names it by.
async fn minted_key(emulator: &Emulator, create_key: Value, case: &str) -> String {
    let answer = emulator.kms_call(REGION, "CreateKey", create_key).await;
    assert_eq!(
        answer.status, 200,
        "case CreateKey({case}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateKey({case}): expected a key ARN, got {:?}", answer.body))
        .to_string()
}

/// An `EncryptionAtRestOptions` enabling encryption under `key`.
fn enabled(key: &str) -> Value {
    json!({ "Enabled": true, "KmsKeyId": key })
}

/// How the service states that `arn` is not a key a domain's data can be encrypted under:
/// the key, by the ARN KMS answered for it, and what a key used for encryption at rest has
/// to be.
fn not_symmetric(arn: &str) -> String {
    format!(
        "{ENVELOPE}KmsKeyID:{arn} is not a Symmetric CMK. Only Symmetric CMKs may be used \
         for encryption at rest."
    )
}

/// What `DescribeDomain` reports `name` encrypting at rest with — absent when the domain
/// holds no container at all.
async fn encryption(emulator: &Emulator, name: &str) -> Option<Value> {
    let answer = emulator.describe(name).await;
    assert_eq!(
        answer.status, 200,
        "case DescribeDomain({name:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainStatus"]
        .get("EncryptionAtRestOptions")
        .cloned()
}

#[tokio::test]
async fn a_created_domain_may_only_be_encrypted_at_rest_under_a_symmetric_encryption_key() {
    let emulator = Emulator::with_kms();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            create_key,
            accepted,
        } = row;
        let key = minted_key(&emulator, create_key, case).await;

        let name = format!("cmk-kind-create-{index}");
        let mut body = baseline_create(&name);
        body["EncryptionAtRestOptions"] = enabled(&key);
        let answer = emulator.create(body).await;

        if accepted {
            assert_eq!(
                answer.status, 200,
                "case CreateDomain({case}): expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let reported = encryption(&emulator, &name).await;
            assert_eq!(
                reported,
                Some(enabled(&key)),
                "case DescribeDomain({case}): expected the domain to report the key it is \
                 encrypted under, got {reported:?}"
            );
        } else {
            assert_refusal(&answer, REFUSAL, case);
            let expected = not_symmetric(&key);
            let stated = message(&answer, case);
            assert_eq!(
                stated, expected,
                "case CreateDomain({case}): expected {expected:?}, got {stated:?}"
            );
            // A refused create leaves no domain behind, so the name it asked for is free.
            assert_no_domain(&emulator, &name).await;
        }
    }
}

#[tokio::test]
async fn the_same_key_kind_rule_governs_a_change_to_a_domains_encryption() {
    let emulator = Emulator::with_kms();

    // One settled domain the rows try to change: a domain with work in flight refuses a
    // further change, so the create's own change lands first.
    let name = "cmk-kind-update";
    create_baseline(&emulator, name).await;
    settled_domain(&emulator, name).await;
    let held = encryption(&emulator, name).await;

    for row in rows() {
        let Row {
            case,
            create_key,
            accepted,
        } = row;
        if accepted {
            continue;
        }
        let key = minted_key(&emulator, create_key, case).await;

        let answer = emulator
            .update_config(name, json!({ "EncryptionAtRestOptions": enabled(&key) }))
            .await;
        assert_refusal(&answer, REFUSAL, case);
        let expected = not_symmetric(&key);
        let stated = message(&answer, case);
        assert_eq!(
            stated, expected,
            "case UpdateDomainConfig({case}): expected {expected:?}, got {stated:?}"
        );
        // A refused change stores nothing: the domain keeps the encryption it had.
        let reported = encryption(&emulator, name).await;
        assert_eq!(
            reported, held,
            "case DescribeDomain({case}): a refused change must leave the domain encrypting \
             with {held:?}, got {reported:?}"
        );
    }

    // The same domain takes a change to a key of the one kind it can be encrypted under, so
    // what the rows above were refused for is the KIND of their key and nothing else.
    let symmetric = minted_key(&emulator, json!({}), "a symmetric encryption key").await;
    let answer = emulator
        .update_config(
            name,
            json!({ "EncryptionAtRestOptions": enabled(&symmetric) }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case UpdateDomainConfig(a symmetric encryption key): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let reported = encryption(&emulator, name).await;
    assert_eq!(
        reported,
        Some(enabled(&symmetric)),
        "case DescribeDomain(a symmetric encryption key): expected the changed domain to \
         encrypt with the key, got {reported:?}"
    );
}

#[tokio::test]
async fn the_kind_of_the_key_is_weighed_however_the_caller_names_it_and_whatever_state_it_is_in() {
    let emulator = Emulator::with_kms();
    let key = minted_key(
        &emulator,
        json!({ "KeySpec": "RSA_2048", "KeyUsage": "SIGN_VERIFY" }),
        "an RSA key pair for signing and verification",
    )
    .await;
    let bare_id = key
        .rsplit_once("key/")
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| panic!("expected a key ARN, got {key:?}"));
    emulator
        .kms_key_operation(
            REGION,
            "CreateAlias",
            json!({ "AliasName": KEY_ALIAS, "TargetKeyId": key.clone() }),
        )
        .await;

    // However the caller spells the reference, the key it resolves to is the same key of the
    // wrong kind — and the refusal names it by the ARN KMS answered for it.
    for (index, (case, named)) in [
        ("by its key ARN", key.clone()),
        ("by its bare key id", bare_id),
        ("by an alias pointing at it", KEY_ALIAS.to_string()),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("cmk-kind-named-{index}");
        let mut body = baseline_create(&name);
        body["EncryptionAtRestOptions"] = enabled(&named);
        let answer = emulator.create(body).await;
        assert_refusal(&answer, REFUSAL, case);
        let expected = not_symmetric(&key);
        let stated = message(&answer, case);
        assert_eq!(
            stated, expected,
            "case CreateDomain({case}): expected {expected:?}, got {stated:?}"
        );
        assert_no_domain(&emulator, &name).await;
    }

    // A key of the wrong kind could never encrypt the domain, so the kind is what the caller
    // is told even once the key's owner has disabled it: the key state is weighed only for a
    // key that is of the right kind in the first place.
    emulator
        .kms_key_operation(REGION, "DisableKey", json!({ "KeyId": key.clone() }))
        .await;
    let disabled_case = "the same key once its owner disabled it";
    let mut body = baseline_create("cmk-kind-disabled");
    body["EncryptionAtRestOptions"] = enabled(&key);
    let answer = emulator.create(body).await;
    assert_refusal(&answer, REFUSAL, disabled_case);
    let expected = not_symmetric(&key);
    let stated = message(&answer, disabled_case);
    assert_eq!(
        stated, expected,
        "case CreateDomain({disabled_case}): expected {expected:?}, got {stated:?}"
    );
    assert_no_domain(&emulator, "cmk-kind-disabled").await;
}

#[tokio::test]
async fn a_configuration_that_enables_no_encryption_names_no_key_to_weigh() {
    let emulator = Emulator::with_kms();
    let key = minted_key(
        &emulator,
        json!({ "KeySpec": "RSA_2048", "KeyUsage": "SIGN_VERIFY" }),
        "an RSA key pair for signing and verification",
    )
    .await;

    // Encryption is switched off, so no key encrypts the domain's data and the value the
    // container carries is stored as written rather than resolved and weighed.
    let case = "encryption disabled against a key of the wrong kind";
    let name = "cmk-kind-off";
    let options = json!({ "Enabled": false, "KmsKeyId": key });
    let mut body = baseline_create(name);
    body["EncryptionAtRestOptions"] = options.clone();
    let answer = emulator.create(body).await;
    assert_eq!(
        answer.status, 200,
        "case CreateDomain({case}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let reported = encryption(&emulator, name).await;
    assert_eq!(
        reported,
        Some(options),
        "case DescribeDomain({case}): expected the container as written, got {reported:?}"
    );
}
