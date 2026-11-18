// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW this service refuses a `CreateKey` whose `KeyUsage` and
//! `KeySpec` cannot hold together, and how `GetPublicKey` refuses a key that has no
//! public half.
//!
//! The RULE asserted here: **a refusal carries the text KMS words that error class with
//! and nothing else** — the incompatibility refusal is the fixed `KeyUsage ` /
//! ` is not compatible with KeySpec ` sentence with the two values the request supplied
//! and no trailing clause, and the unsupported-key-spec refusal of a public-key download
//! carries NO message at all, so a client matching, logging or forwarding the text reads
//! what the service states rather than wording added here.
//!
//! Each rule is a TABLE over what it generalises over: every family of key spec, under
//! every `KeyUsage` the model declares that the spec does not accept, and — for the
//! public-key download — every key spec that has no public half.

use serde_json::{json, Value};

use super::harness::{create_key, failure, ok, refused, router, text, REGION};

/// Every `KeyUsage` the model declares (`smithy_query(shape="KeyUsageType")`), which is
/// the set a row's usage is drawn from.
const KEY_USAGES: [&str; 5] = [
    "SIGN_VERIFY",
    "ENCRYPT_DECRYPT",
    "GENERATE_VERIFY_MAC",
    "KEY_AGREEMENT",
    "ENCAPSULATE_DECAPSULATE",
];

/// The usages each key spec DOES accept, from the matrix
/// `smithy_query(query="doc",shape="CreateKeyRequest",member="KeyUsage")` states: every
/// other usage of [`KEY_USAGES`] is one that spec cannot carry. One row per family of
/// spec, including a spec of each family the issue did not cite.
const SUPPORTED_USAGES: &[(&str, &[&str])] = &[
    ("SYMMETRIC_DEFAULT", &["ENCRYPT_DECRYPT"]),
    ("HMAC_256", &["GENERATE_VERIFY_MAC"]),
    ("HMAC_512", &["GENERATE_VERIFY_MAC"]),
    ("RSA_2048", &["ENCRYPT_DECRYPT", "SIGN_VERIFY"]),
    ("RSA_4096", &["ENCRYPT_DECRYPT", "SIGN_VERIFY"]),
    ("ECC_NIST_P256", &["SIGN_VERIFY", "KEY_AGREEMENT"]),
    ("ECC_NIST_P521", &["SIGN_VERIFY", "KEY_AGREEMENT"]),
    ("ECC_SECG_P256K1", &["SIGN_VERIFY"]),
    ("ECC_NIST_EDWARDS25519", &["SIGN_VERIFY"]),
    ("ML_DSA_44", &["SIGN_VERIFY"]),
    ("ML_DSA_87", &["SIGN_VERIFY"]),
    ("SM2", &["ENCRYPT_DECRYPT", "SIGN_VERIFY", "KEY_AGREEMENT"]),
    ("ML_KEM_512", &["ENCAPSULATE_DECAPSULATE"]),
    ("ML_KEM_768", &["ENCAPSULATE_DECAPSULATE"]),
    ("ML_KEM_1024", &["ENCAPSULATE_DECAPSULATE"]),
];

/// How KMS words an incompatible pair: fixed text, the two values from the request, and
/// nothing after the key spec.
fn incompatible(key_usage: &str, key_spec: &str) -> String {
    format!("KeyUsage {key_usage} is not compatible with KeySpec {key_spec}")
}

/// Every pair of a key spec and a `KeyUsage` that spec does not accept is refused 400
/// `ValidationException` with exactly that sentence — and nothing is minted.
#[tokio::test]
async fn create_key_words_an_incompatible_key_usage_as_kms_does() {
    let router = router();

    // One key that IS created, so "nothing was minted" has something to compare to.
    let kept = create_key(&router, &json!({"Description": "kept"})).await;

    for (key_spec, supported) in SUPPORTED_USAGES {
        for key_usage in KEY_USAGES {
            if supported.contains(&key_usage) {
                continue;
            }
            let case = format!("{key_usage} with {key_spec}");
            let (status, code, message) = refused(
                &router,
                REGION,
                "CreateKey",
                &json!({"KeySpec": key_spec, "KeyUsage": key_usage}),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationException"),
                "case {case}: expected HTTP 400 ValidationException, got {status} {code}"
            );
            assert_eq!(
                message,
                incompatible(key_usage, key_spec),
                "case {case}: expected {:?}, got {message:?}",
                incompatible(key_usage, key_spec)
            );
        }
    }

    // The DEPRECATED spec member names the same property, so a pair named through it is
    // refused by the same sentence about the same two values.
    let (status, code, message) = refused(
        &router,
        REGION,
        "CreateKey",
        &json!({"CustomerMasterKeySpec": "HMAC_256", "KeyUsage": "ENCRYPT_DECRYPT"}),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case the deprecated spec member: expected HTTP 400 ValidationException, got \
         {status} {code}"
    );
    assert_eq!(
        message,
        incompatible("ENCRYPT_DECRYPT", "HMAC_256"),
        "case the deprecated spec member: expected {:?}, got {message:?}",
        incompatible("ENCRYPT_DECRYPT", "HMAC_256")
    );

    let listed = ok(&router, "ListKeys", &json!({})).await;
    assert_eq!(
        listed["Keys"],
        json!([{"KeyId": kept["KeyId"], "KeyArn": kept["Arn"]}]),
        "a refused create must leave no key behind, got {listed}"
    );
}

/// Every pair a key spec DOES accept is served, so the sentence above is the answer to a
/// pair that cannot hold together rather than to a usage this service simply refuses.
#[tokio::test]
async fn create_key_serves_every_pair_its_key_spec_accepts() {
    let router = router();

    for (key_spec, supported) in SUPPORTED_USAGES {
        for key_usage in *supported {
            let case = format!("{key_usage} with {key_spec}");
            let metadata = create_key(
                &router,
                &json!({"Description": &case, "KeySpec": key_spec, "KeyUsage": key_usage}),
            )
            .await;
            assert_eq!(
                (
                    text(&case, &metadata, "KeySpec"),
                    text(&case, &metadata, "KeyUsage")
                ),
                ((*key_spec).to_string(), (*key_usage).to_string()),
                "case {case}: expected the requested spec and usage, got {metadata}"
            );
        }
    }
}

/// `GetPublicKey` on a key with no public half is refused 400
/// `UnsupportedOperationException` with an EMPTY message: the code is the whole of the
/// answer for this class.
///
/// The rows are the ways a caller asks for a SYMMETRIC encryption key — the key type
/// that carries a usage a public key could belong to and has no public half — with the
/// spec stated, left to its default, named through the deprecated member, and on a
/// multi-Region key, each asked for by key id and by key ARN.
///
/// A key whose USAGE has no public half at all (an HMAC key) is a different class, and
/// the second half of this test pins that it stays the one that names the key, its usage
/// and the operation.
#[tokio::test]
async fn get_public_key_refuses_a_key_with_no_public_half_without_a_message() {
    let router = router();

    let rows: Vec<(&str, Value)> = vec![
        (
            "the spec stated",
            json!({"KeySpec": "SYMMETRIC_DEFAULT", "KeyUsage": "ENCRYPT_DECRYPT"}),
        ),
        ("the spec left to its default", json!({})),
        (
            "the deprecated spec member",
            json!({"CustomerMasterKeySpec": "SYMMETRIC_DEFAULT"}),
        ),
        (
            "a multi-Region symmetric key",
            json!({"KeySpec": "SYMMETRIC_DEFAULT", "MultiRegion": true}),
        ),
    ];

    for (case, mut request) in rows {
        request["Description"] = json!(case);
        let metadata = create_key(&router, &request).await;
        let key_id = text(case, &metadata, "KeyId");
        let arn = text(case, &metadata, "Arn");

        // Both identifier forms name the same key, and both are refused the same way.
        for identifier in [key_id.as_str(), arn.as_str()] {
            let (status, code, message) = refused(
                &router,
                REGION,
                "GetPublicKey",
                &json!({"KeyId": identifier}),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "UnsupportedOperationException"),
                "case {case} by {identifier}: expected HTTP 400 \
                 UnsupportedOperationException, got {status} {code}"
            );
            assert_eq!(
                message, "",
                "case {case} by {identifier}: this refusal carries no message, got \
                 {message:?}"
            );
        }
    }

    // An HMAC key carries a usage no public key belongs to, which is the OTHER refusal
    // class: it keeps its own sentence, so the empty message above is not this
    // operation's answer to every key without a public half.
    for key_spec in ["HMAC_224", "HMAC_256", "HMAC_384", "HMAC_512"] {
        let case = format!("{key_spec} GENERATE_VERIFY_MAC");
        let metadata = create_key(
            &router,
            &json!({"Description": &case, "KeySpec": key_spec,
                    "KeyUsage": "GENERATE_VERIFY_MAC"}),
        )
        .await;
        let key_id = text(&case, &metadata, "KeyId");
        let arn = text(&case, &metadata, "Arn");
        let (status, code, message) =
            refused(&router, REGION, "GetPublicKey", &json!({"KeyId": key_id})).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidKeyUsageException"),
            "case {case}: expected HTTP 400 InvalidKeyUsageException, got {status} {code}"
        );
        assert!(
            message.contains(&arn)
                && message.contains("GENERATE_VERIFY_MAC")
                && message.contains("GetPublicKey"),
            "case {case}: the refusal must name the key, its usage and the operation, got \
             {message:?}"
        );
    }

    // The key is untouched by the refusal: the record still reports what it was created
    // with, and a key that DOES have a public half still serves one.
    let asymmetric = create_key(
        &router,
        &json!({"Description": "asymmetric", "KeySpec": "RSA_2048",
                "KeyUsage": "SIGN_VERIFY"}),
    )
    .await;
    let served: Value = ok(
        &router,
        "GetPublicKey",
        &json!({"KeyId": text("asymmetric", &asymmetric, "KeyId")}),
    )
    .await;
    assert!(
        served["PublicKey"].is_string(),
        "case an asymmetric key: expected a public key, got {served}"
    );

    // A key the request names by an identifier that resolves to nothing is a different
    // refusal, so the empty message above is not this service's answer to everything.
    let (status, code) = failure(
        &router,
        "GetPublicKey",
        &json!({"KeyId": "11111111-2222-3333-4444-555555555555"}),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "NotFoundException"),
        "case an identifier naming no key: expected HTTP 400 NotFoundException, got \
         {status} {code}"
    );
}
