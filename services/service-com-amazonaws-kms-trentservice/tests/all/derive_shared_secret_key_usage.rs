// SPDX-License-Identifier: CC0-1.0

//! The KEY-USAGE gate of `DeriveSharedSecret`: which keys may derive a shared secret at
//! all, and how a key that may not is refused.
//!
//! The RULE asserted here: **`DeriveSharedSecret` resolves the key its `KeyId` names and
//! then judges that key's `KeyUsage` BEFORE running any agreement — a key whose usage is
//! not `KEY_AGREEMENT` is refused with HTTP 400 and the modelled client error
//! `InvalidKeyUsageException`
//! (`smithy_query(query="operation",operation="DeriveSharedSecret")`), by a sentence that
//! names the resolved key ARN and the usage that key carries, while a key of the SAME key
//! spec that does carry `KEY_AGREEMENT` answers the agreement.** The refusal is about the
//! key, so it does not depend on the identifier form the request used, on the peer public
//! key it submitted, or on anything else the request carries.
//!
//! The tables span what the rule generalises over: every other `KeyUsageType` value
//! (`smithy_query(shape="KeyUsageType")`) with a key spec that carries it — including the
//! elliptic curve specs that CAN agree but were minted for another usage, since the usage
//! and not the curve is what decides — every identifier form a `KeyId` member accepts, and
//! every key spec that can carry `KEY_AGREEMENT`, whose curve decides how many bytes the
//! raw secret is.
//!
//! The expected refusal message is built per row from the key ARN and the `KeyUsage` the
//! service itself reports for that key through `DescribeKey`, so no row carries an ARN, a
//! key id or a usage written here: the variable halves of the sentence are read back out
//! of the emulator's own state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{b64, describe, ok, refused, router, text, unb64, REGION};

/// The operation under test.
const OPERATION: &str = "DeriveSharedSecret";

/// The one `KeyAgreementAlgorithmSpec` the model declares
/// (`smithy_query(shape="KeyAgreementAlgorithmSpec")`).
const ECDH: &str = "ECDH";

/// The `KeyUsage` a key that may derive a shared secret carries
/// (`smithy_query(shape="KeyUsageType")`).
const KEY_AGREEMENT: &str = "KEY_AGREEMENT";

/// The wire error code of the modelled refusal, which the model binds to HTTP 400
/// (`smithy_query(query="operation",operation="DeriveSharedSecret")`).
const INVALID_KEY_USAGE: &str = "InvalidKeyUsageException";

/// One row of the usage table: a `KeySpec` and a `KeyUsage` that spec can carry, for every
/// `KeyUsageType` value other than [`KEY_AGREEMENT`]
/// (`smithy_query(shape="KeyUsageType")` — `SIGN_VERIFY`, `ENCRYPT_DECRYPT`,
/// `GENERATE_VERIFY_MAC`, `ENCAPSULATE_DECAPSULATE`).
///
/// The first four rows are key pairs on a curve that CAN derive a shared secret, minted for
/// another usage instead; the rest are specs that cannot agree on any usage.
const WRONG_USAGE_ROWS: &[(&str, &str)] = &[
    ("ECC_NIST_P256", "SIGN_VERIFY"),
    ("ECC_NIST_P384", "SIGN_VERIFY"),
    ("ECC_NIST_P521", "SIGN_VERIFY"),
    ("SM2", "ENCRYPT_DECRYPT"),
    ("ECC_NIST_EDWARDS25519", "SIGN_VERIFY"),
    ("ECC_SECG_P256K1", "SIGN_VERIFY"),
    ("ML_DSA_44", "SIGN_VERIFY"),
    ("RSA_3072", "SIGN_VERIFY"),
    ("RSA_2048", "ENCRYPT_DECRYPT"),
    ("SYMMETRIC_DEFAULT", "ENCRYPT_DECRYPT"),
    ("HMAC_384", "GENERATE_VERIFY_MAC"),
    ("ML_KEM_768", "ENCAPSULATE_DECAPSULATE"),
];

/// Every `KeySpec` that can carry [`KEY_AGREEMENT`], with the length of the raw secret its
/// curve yields: one field element, so ceil(field bits / 8) — 32 bytes for the 256-bit
/// fields of P-256 and the SM2 curve, 48 for P-384, 66 for the 521-bit field of P-521
/// (`smithy_query(query="doc",shape="DeriveSharedSecret")` names the NIST-standard elliptic
/// curve and SM2 key pairs as the ones that derive a shared secret).
const AGREEING_SPECS: &[(&str, usize)] = &[
    ("ECC_NIST_P256", 32),
    ("ECC_NIST_P384", 48),
    ("ECC_NIST_P521", 66),
    ("SM2", 32),
];

/// Create a key of `key_spec` under `key_usage` and answer its (key id, key ARN, reported
/// `KeyUsage`) as the service reports them — the two variable halves of the refusal
/// sentence come from here, never from this file.
async fn key_of(
    router: &MultiServiceRouter,
    case: &str,
    key_spec: &str,
    key_usage: &str,
) -> (String, String, String) {
    let created = super::harness::create_key(
        router,
        &json!({"Description": case, "KeySpec": key_spec, "KeyUsage": key_usage}),
    )
    .await;
    let key_id = text(case, &created, "KeyId");
    let reported = describe(router, &key_id).await;
    (
        key_id,
        text(case, &reported, "Arn"),
        text(case, &reported, "KeyUsage"),
    )
}

/// How this service words the refusal of a key whose usage does not permit the operation:
/// the fixed text, with the key ARN and the key's own `KeyUsage` filled in.
fn wrong_usage_message(key_arn: &str, key_usage: &str) -> String {
    format!("{key_arn} key usage is {key_usage} which is not valid for {OPERATION}.")
}

/// A `DeriveSharedSecret` request body.
fn request(identifier: &str, public_key: &[u8]) -> Value {
    json!({
        "KeyId": identifier,
        "KeyAgreementAlgorithm": ECDH,
        "PublicKey": b64(public_key),
    })
}

/// A peer public key on the curve `key_spec` names: the DER-encoded X.509
/// `SubjectPublicKeyInfo` the service publishes for a key-agreement key of that spec, which
/// is the public half the documented workflow submits.
async fn peer_public_key(router: &MultiServiceRouter, case: &str, key_spec: &str) -> Vec<u8> {
    let (peer, _, _) = key_of(router, case, key_spec, KEY_AGREEMENT).await;
    let published = ok(router, "GetPublicKey", &json!({"KeyId": peer})).await;
    unb64(case, &published, "PublicKey")
}

/// The rule's refusal half: a key of any other `KeyUsage` is refused HTTP 400
/// `InvalidKeyUsageException`, by the sentence naming that key's ARN and its own usage.
#[tokio::test]
async fn a_key_of_another_usage_is_refused_by_arn_and_usage() {
    let router = router();
    let peer = peer_public_key(&router, "peer", "ECC_NIST_P256").await;

    for (key_spec, key_usage) in WRONG_USAGE_ROWS {
        let case = format!("{key_spec} under {key_usage}");
        let (key_id, key_arn, reported_usage) =
            key_of(&router, &case, key_spec, key_usage).await;
        assert_eq!(
            reported_usage, *key_usage,
            "case {case}: expected the key to carry KeyUsage {key_usage}, got \
             {reported_usage}"
        );

        let (status, code, message) =
            refused(&router, REGION, OPERATION, &request(&key_id, &peer)).await;
        let expected = wrong_usage_message(&key_arn, &reported_usage);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, INVALID_KEY_USAGE, expected.as_str()),
            "case {case}: expected 400 {INVALID_KEY_USAGE} {expected:?}, got {status} \
             {code} {message:?}"
        );
    }
}

/// The refusal is about the KEY, so every identifier form a `KeyId` member accepts reaches
/// the same key and the same sentence — which names the resolved key ARN, not the
/// identifier the request used.
#[tokio::test]
async fn every_identifier_form_is_refused_by_the_resolved_key_arn() {
    let router = router();
    let peer = peer_public_key(&router, "peer", "ECC_NIST_P256").await;
    let (key_id, key_arn, key_usage) =
        key_of(&router, "identifier forms", "ECC_NIST_P256", "SIGN_VERIFY").await;

    let alias_name = "alias/signing-not-agreement";
    ok(
        &router,
        "CreateAlias",
        &json!({"AliasName": alias_name, "TargetKeyId": &key_id}),
    )
    .await;
    let aliases = ok(&router, "ListAliases", &json!({})).await;
    let alias_arn = aliases["Aliases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find(|entry| entry["AliasName"] == json!(alias_name))
        .map(|entry| text("alias arn", entry, "AliasArn"))
        .unwrap_or_else(|| panic!("case alias arn: ListAliases reports the alias, got {aliases}"));

    // The four forms a `KeyId` accepts: "key ID, key ARN, alias name, or alias ARN"
    // (`smithy_query(query="doc",shape="DeriveSharedSecretRequest",member="KeyId")`).
    let expected = wrong_usage_message(&key_arn, &key_usage);
    for identifier in [
        key_id.as_str(),
        key_arn.as_str(),
        alias_name,
        alias_arn.as_str(),
    ] {
        let (status, code, message) =
            refused(&router, REGION, OPERATION, &request(identifier, &peer)).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, INVALID_KEY_USAGE, expected.as_str()),
            "case {identifier}: expected 400 {INVALID_KEY_USAGE} {expected:?}, got {status} \
             {code} {message:?}"
        );
    }
}

/// The usage is judged BEFORE any agreement, so nothing about the public key the request
/// submits changes the answer: a peer public key on another curve, bytes that are no public
/// key at all and a dry run all still meet the usage refusal.
#[tokio::test]
async fn the_usage_is_judged_before_anything_the_request_carries() {
    let router = router();
    let (key_id, key_arn, key_usage) =
        key_of(&router, "precedence", "ECC_NIST_P256", "SIGN_VERIFY").await;
    let expected = wrong_usage_message(&key_arn, &key_usage);

    // A public key on the key's OWN curve, one on another curve, and bytes that decode as
    // no public key at all — the three ways the agreement half could answer instead.
    let same_curve = peer_public_key(&router, "same curve", "ECC_NIST_P256").await;
    let other_curve = peer_public_key(&router, "other curve", "ECC_NIST_P384").await;
    let not_a_key = vec![0x5au8; 64];

    let mut rows: Vec<(String, Value)> = Vec::new();
    for (case, public_key) in [
        ("a public key on the key's curve", &same_curve),
        ("a public key on another curve", &other_curve),
        ("bytes that are no public key", &not_a_key),
    ] {
        rows.push((case.to_string(), request(&key_id, public_key)));
    }
    // A dry run checks the request and then always fails, so it is answered after every
    // other check — and is not reached, because this key cannot agree at all.
    rows.push((
        "a dry run".to_string(),
        json!({
            "KeyId": &key_id,
            "KeyAgreementAlgorithm": ECDH,
            "PublicKey": b64(&same_curve),
            "DryRun": true,
        }),
    ));

    for (case, body) in rows {
        let (status, code, message) = refused(&router, REGION, OPERATION, &body).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, INVALID_KEY_USAGE, expected.as_str()),
            "case {case}: expected 400 {INVALID_KEY_USAGE} {expected:?}, got {status} \
             {code} {message:?}"
        );
    }
}

/// The rule's positive half: the SAME key spec, the SAME algorithm and the SAME peer public
/// key against a key that carries `KEY_AGREEMENT` succeed — the usage is the only
/// difference between the refusal and the agreement.
#[tokio::test]
async fn the_same_request_against_a_key_agreement_key_succeeds() {
    let router = router();

    for (key_spec, secret_len) in AGREEING_SPECS {
        let case = format!("{key_spec} peer");
        let peer = peer_public_key(&router, &case, key_spec).await;

        // The same curve under a usage that cannot agree: refused.
        let (signing, signing_arn, signing_usage) =
            key_of(&router, &case, key_spec, "SIGN_VERIFY").await;
        let expected = wrong_usage_message(&signing_arn, &signing_usage);
        let (status, code, message) =
            refused(&router, REGION, OPERATION, &request(&signing, &peer)).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, INVALID_KEY_USAGE, expected.as_str()),
            "case {case}: expected 400 {INVALID_KEY_USAGE} {expected:?}, got {status} \
             {code} {message:?}"
        );

        // The same curve under KEY_AGREEMENT, with the identical algorithm and peer public
        // key: the agreement runs and answers the raw secret of that curve's field size.
        let (agreeing, agreeing_arn, _) = key_of(&router, &case, key_spec, KEY_AGREEMENT).await;
        let answer = ok(&router, OPERATION, &request(&agreeing, &peer)).await;
        let secret = unb64(&case, &answer, "SharedSecret");
        assert_eq!(
            secret.len(),
            *secret_len,
            "case {case}: expected a {secret_len}-byte shared secret, got {} bytes",
            secret.len()
        );
        assert_eq!(
            answer["KeyAgreementAlgorithm"],
            json!(ECDH),
            "case {case}: expected KeyAgreementAlgorithm {ECDH}, got {answer}"
        );
        assert_eq!(
            answer["KeyId"],
            json!(agreeing_arn),
            "case {case}: expected the key ARN {agreeing_arn}, got {answer}"
        );
    }
}
