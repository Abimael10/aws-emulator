// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KEY-AGREEMENT surface of this service: a KMS key whose
//! `KeyUsage` is `KEY_AGREEMENT`, the public half a peer downloads for it, and the shared
//! secret `DeriveSharedSecret` derives from that key's private half and the peer's own
//! public key.
//!
//! The RULE asserted here: **`DeriveSharedSecret` runs the Elliptic Curve Cryptography
//! Cofactor Diffie-Hellman Primitive between the private half of the named key's key pair
//! and the submitted peer public key, and answers the RAW agreed secret** — the x
//! coordinate of the agreed point, one field element of the curve both key pairs are on.
//! Everything else follows from the primitive being a function of the two key pairs alone:
//! the peer running it from the opposite two halves reaches the same bytes, two KMS keys
//! agreeing with each other reach the same bytes from either side, deriving twice answers
//! the same bytes, and a different peer key pair reaches different bytes. A public key that
//! is not a DER-encoded X.509 `SubjectPublicKeyInfo` on that same curve is refused rather
//! than agreed with.
//!
//! Every test is a TABLE over the key specs that can carry `KEY_AGREEMENT` — which is what
//! the rule generalises over: the curve decides the size of the agreed secret and nothing
//! else about the surface changes with it. Rows the issue did not cite (P-384, P-521 and
//! the SM2 curve) are asserted on exactly like the one it did.
//!
//! The peer's key pair is one the SERVICE hands out in the clear
//! (`GenerateDataKeyPair`), so each row holds a real private half on its curve without any
//! key generation written here, and the agreement a peer computes is recomputed from that
//! private half and the public key `GetPublicKey` published, with the library that
//! implements the curve. No row carries a key id, a public key or a secret written here:
//! every one of those is a value the service produced for that run.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    b64, enabled_key, ok, refused, router, text, tracked_last_usage, unb64, REGION,
};

/// The operation under test.
const OPERATION: &str = "DeriveSharedSecret";

/// The `KeyUsage` a key-agreement key carries (`smithy_query(shape="KeyUsageType")`).
const KEY_AGREEMENT: &str = "KEY_AGREEMENT";

/// The only `KeyAgreementAlgorithmSpec` the model declares
/// (`smithy_query(shape="KeyAgreementAlgorithmSpec")`).
const ECDH: &str = "ECDH";

/// The `OriginType` of a key whose material KMS generated
/// (`smithy_query(shape="OriginType")`), which is what every key this suite creates has.
const AWS_KMS: &str = "AWS_KMS";

/// One row: a `KeySpec` that can derive a shared secret, the length of the secret its
/// curve yields, and the agreement over that curve as the peer runs it.
struct CurveCase {
    /// The `KeySpec` value (`smithy_query(shape="KeySpec")`), which is also the
    /// `DataKeyPairSpec` value naming a peer key pair on the same curve
    /// (`smithy_query(shape="DataKeyPairSpec")`).
    key_spec: &'static str,
    /// How many bytes the raw agreed secret is: one field element of the curve, so
    /// ceil(field bits / 8) — 32 for the 256-bit fields of P-256 and the SM2 curve of
    /// GM/T 0003-2012, 48 for P-384, 66 for the 521-bit field of P-521.
    shared_secret_len: usize,
    /// The agreement over this row's curve, run from a peer's PKCS#8 private key and a
    /// DER-encoded X.509 public key — what the peer in the documented workflow computes
    /// for itself ("Bob uses the Elliptic Curve Cryptography Cofactor Diffie-Hellman
    /// Primitive to calculate the same raw secret using his private key and Alice's
    /// public key", `smithy_query(query="doc",shape="DeriveSharedSecret")`).
    agree: fn(&str, &[u8], &[u8]) -> Vec<u8>,
}

/// Every `KeySpec` whose key pair can derive a shared secret: "you must use an asymmetric
/// NIST-standard elliptic curve (ECC) or SM2 (China Regions only) KMS key pair with a
/// KeyUsage value of KEY_AGREEMENT"
/// (`smithy_query(query="doc",shape="DeriveSharedSecret")`).
const CURVE_CASES: &[CurveCase] = &[
    CurveCase {
        key_spec: "ECC_NIST_P256",
        shared_secret_len: 32,
        agree: agreed_locally::<::p256::NistP256>,
    },
    CurveCase {
        key_spec: "ECC_NIST_P384",
        shared_secret_len: 48,
        agree: agreed_locally::<::p384::NistP384>,
    },
    CurveCase {
        key_spec: "ECC_NIST_P521",
        shared_secret_len: 66,
        agree: agreed_locally::<::p521::NistP521>,
    },
    CurveCase {
        key_spec: "SM2",
        shared_secret_len: 32,
        agree: agreed_locally::<::sm2::Sm2>,
    },
];

/// The agreement on the curve `C` between the private key `private_key_pkcs8` (a
/// DER-encoded PKCS#8 `PrivateKeyInfo`) and the public key `public_key_spki` (a
/// DER-encoded X.509 `SubjectPublicKeyInfo`), as the curve's own library runs it.
fn agreed_locally<C>(case: &str, private_key_pkcs8: &[u8], public_key_spki: &[u8]) -> Vec<u8>
where
    C: ::elliptic_curve::CurveArithmetic
        + ::elliptic_curve::point::PointCompression
        + ::elliptic_curve::pkcs8::AssociatedOid,
    C::AffinePoint:
        ::elliptic_curve::sec1::FromSec1Point<C> + ::elliptic_curve::sec1::ToSec1Point<C>,
    C::FieldBytesSize: ::elliptic_curve::sec1::ModulusSize,
{
    use ::elliptic_curve::pkcs8::DecodePrivateKey;
    use ::spki::DecodePublicKey;

    let secret = ::elliptic_curve::SecretKey::<C>::from_pkcs8_der(private_key_pkcs8)
        .unwrap_or_else(|error| panic!("case {case}: the peer private key is PKCS#8: {error}"));
    let public = ::elliptic_curve::PublicKey::<C>::from_public_key_der(public_key_spki)
        .unwrap_or_else(|error| {
            panic!("case {case}: the public key is a SubjectPublicKeyInfo: {error}")
        });
    ::elliptic_curve::ecdh::diffie_hellman(secret.to_nonzero_scalar(), public.as_affine())
        .raw_secret_bytes()
        .to_vec()
}

/// How this service words the refusal of a public key the named key pair cannot agree
/// with: fixed text, with the key's own spec read from the key.
fn invalid_public_key(key_spec: &str) -> String {
    format!(
        "The public key must be a DER-encoded X.509 public key (SubjectPublicKeyInfo) on \
         the same elliptic curve as the KMS key, whose KeySpec is {key_spec}."
    )
}

/// A key of `key_spec` under [`KEY_AGREEMENT`], as (key id, key ARN).
async fn agreement_key(
    router: &MultiServiceRouter,
    key_spec: &str,
    description: &str,
) -> (String, String) {
    let metadata = super::harness::create_key(
        router,
        &json!({"Description": description, "KeySpec": key_spec, "KeyUsage": KEY_AGREEMENT}),
    )
    .await;
    (
        text(key_spec, &metadata, "KeyId"),
        text(key_spec, &metadata, "Arn"),
    )
}

/// The DER-encoded X.509 public key `GetPublicKey` publishes for `identifier`.
async fn published_public_key(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<u8> {
    let answer = ok(router, "GetPublicKey", &json!({"KeyId": identifier})).await;
    unb64(case, &answer, "PublicKey")
}

/// A peer key pair on the curve `key_pair_spec` names, as (PKCS#8 private key, DER X.509
/// public key) — a pair the service itself generated and returned both halves of, which
/// is the position a peer holding its own key pair is in.
async fn peer_key_pair(
    router: &MultiServiceRouter,
    case: &str,
    wrapping_key_id: &str,
    key_pair_spec: &str,
) -> (Vec<u8>, Vec<u8>) {
    let answer = ok(
        router,
        "GenerateDataKeyPair",
        &json!({"KeyId": wrapping_key_id, "KeyPairSpec": key_pair_spec}),
    )
    .await;
    (
        unb64(case, &answer, "PrivateKeyPlaintext"),
        unb64(case, &answer, "PublicKey"),
    )
}

/// Derive a shared secret for `identifier` from `public_key`, requiring success, and
/// answer the whole response.
async fn derive(
    router: &MultiServiceRouter,
    identifier: &str,
    public_key: &[u8],
) -> Value {
    ok(
        router,
        OPERATION,
        &json!({
            "KeyId": identifier,
            "KeyAgreementAlgorithm": ECDH,
            "PublicKey": b64(public_key),
        }),
    )
    .await
}

/// The rule itself: for every curve a key-agreement key can be on, the secret the service
/// derives from its private half and a peer's public key is exactly the secret that peer
/// computes from its own private half and the key's published public key — and the answer
/// names the key by its ARN, the primitive that ran and the origin of the key material.
#[tokio::test]
async fn derive_shared_secret_answers_the_agreement_the_peer_computes() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");

    for row in CURVE_CASES {
        let case = row.key_spec;
        let (key_id, key_arn) = agreement_key(&router, row.key_spec, "agreement").await;

        // The key reports the one algorithm list its usage selects, so what the service
        // says the key can do and what it then does cannot disagree.
        let metadata = super::harness::describe(&router, &key_id).await;
        assert_eq!(
            metadata["KeyUsage"],
            json!(KEY_AGREEMENT),
            "case {case}: expected KeyUsage {KEY_AGREEMENT}, got {metadata}"
        );
        assert_eq!(
            metadata["KeyAgreementAlgorithms"],
            json!([ECDH]),
            "case {case}: expected KeyAgreementAlgorithms [{ECDH}], got {metadata}"
        );

        let kms_public_key = published_public_key(&router, case, &key_id).await;
        let (peer_private_key, peer_public_key) =
            peer_key_pair(&router, case, &wrapping_key, row.key_spec).await;

        let answer = derive(&router, &key_id, &peer_public_key).await;
        let shared_secret = unb64(case, &answer, "SharedSecret");
        let expected = (row.agree)(case, &peer_private_key, &kms_public_key);

        assert_eq!(
            shared_secret.len(),
            row.shared_secret_len,
            "case {case}: expected a {}-byte shared secret, got {} bytes",
            row.shared_secret_len,
            shared_secret.len()
        );
        assert_eq!(
            shared_secret,
            expected,
            "case {case}: the shared secret must be the agreement the peer computes, \
             expected {expected:?}, got {shared_secret:?}"
        );
        assert_eq!(
            answer["KeyId"],
            json!(key_arn),
            "case {case}: expected the key ARN {key_arn}, got {answer}"
        );
        assert_eq!(
            answer["KeyAgreementAlgorithm"],
            json!(ECDH),
            "case {case}: expected KeyAgreementAlgorithm {ECDH}, got {answer}"
        );
        assert_eq!(
            answer["KeyOrigin"],
            json!(AWS_KMS),
            "case {case}: expected KeyOrigin {AWS_KMS}, got {answer}"
        );
        // "If the response includes the CiphertextForRecipient field, the SharedSecret
        // field is null or empty" — no attestation document took part, so the secret is
        // in the clear and that member is absent
        // (`smithy_query(query="operation",operation="DeriveSharedSecret")`).
        assert!(
            answer["CiphertextForRecipient"].is_null(),
            "case {case}: no enclave took part, so CiphertextForRecipient must be absent, \
             got {answer}"
        );
    }
}

/// The agreement is a FUNCTION of the two key pairs: the same pair of keys yields the same
/// secret however often it is derived, and a different peer key pair on the same curve
/// yields a different one.
#[tokio::test]
async fn the_shared_secret_depends_only_on_the_two_key_pairs() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");

    for row in CURVE_CASES {
        let case = row.key_spec;
        let (key_id, _) = agreement_key(&router, row.key_spec, "determinism").await;
        let (_, peer_public_key) =
            peer_key_pair(&router, case, &wrapping_key, row.key_spec).await;
        let (_, other_public_key) =
            peer_key_pair(&router, case, &wrapping_key, row.key_spec).await;

        let first = unb64(case, &derive(&router, &key_id, &peer_public_key).await, "SharedSecret");
        let again = unb64(case, &derive(&router, &key_id, &peer_public_key).await, "SharedSecret");
        let other = unb64(
            case,
            &derive(&router, &key_id, &other_public_key).await,
            "SharedSecret",
        );

        assert_eq!(
            first, again,
            "case {case}: the same two key pairs must agree on the same secret, got \
             {first:?} then {again:?}"
        );
        assert_ne!(
            first, other,
            "case {case}: another peer key pair must reach another secret, got {other:?}"
        );
    }
}

/// Two KMS keys on one curve reach the same secret from opposite halves — each one's
/// private half with the other's published public key — which is what makes the answer a
/// SHARED secret.
#[tokio::test]
async fn the_shared_secret_is_symmetric_between_two_kms_keys() {
    let router = router();

    for row in CURVE_CASES {
        let case = row.key_spec;
        let (alice, _) = agreement_key(&router, row.key_spec, "alice").await;
        let (bob, _) = agreement_key(&router, row.key_spec, "bob").await;
        let alice_public_key = published_public_key(&router, case, &alice).await;
        let bob_public_key = published_public_key(&router, case, &bob).await;

        let from_alice = unb64(
            case,
            &derive(&router, &alice, &bob_public_key).await,
            "SharedSecret",
        );
        let from_bob = unb64(
            case,
            &derive(&router, &bob, &alice_public_key).await,
            "SharedSecret",
        );

        assert_eq!(
            from_alice, from_bob,
            "case {case}: both peers must reach one secret, got {from_alice:?} from one \
             side and {from_bob:?} from the other"
        );
        assert_eq!(
            from_alice.len(),
            row.shared_secret_len,
            "case {case}: expected a {}-byte shared secret, got {} bytes",
            row.shared_secret_len,
            from_alice.len()
        );
    }
}

/// A public key the named key pair cannot agree with is refused: bytes that are no
/// DER-encoded public key at all, a public key of another kind of key pair, and an
/// elliptic curve public key on a curve other than the key's own — "both key pairs must be
/// on the same elliptic curve".
#[tokio::test]
async fn a_public_key_on_another_curve_is_refused() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");

    // Each row names the key's own spec and the spec of the key pair whose public key is
    // submitted to it: a NIST curve against another NIST curve, the SM2 curve against a
    // NIST curve, and a curve against an RSA key pair, which is no elliptic curve at all.
    let mismatches = [
        ("ECC_NIST_P256", "ECC_NIST_P384"),
        ("ECC_NIST_P384", "ECC_NIST_P521"),
        ("ECC_NIST_P521", "ECC_NIST_P256"),
        ("SM2", "ECC_NIST_P256"),
        ("ECC_NIST_P256", "RSA_2048"),
    ];
    for (key_spec, peer_spec) in mismatches {
        let case = format!("{key_spec} with a {peer_spec} public key");
        let (key_id, _) = agreement_key(&router, key_spec, "mismatch").await;
        let (_, peer_public_key) = peer_key_pair(&router, &case, &wrapping_key, peer_spec).await;
        let (status, code, message) = refused(
            &router,
            REGION,
            OPERATION,
            &json!({
                "KeyId": key_id,
                "KeyAgreementAlgorithm": ECDH,
                "PublicKey": b64(&peer_public_key),
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationException", invalid_public_key(key_spec).as_str()),
            "case {case}: expected the invalid-public-key refusal, got {status} {code} \
             {message}"
        );
    }

    // Bytes that are no DER structure at all, asserted on the same key: the refusal is
    // about the public key, so what is wrong with it does not change the answer.
    let (key_id, _) = agreement_key(&router, "ECC_NIST_P256", "not a public key").await;
    for len in [1usize, 32, 8192] {
        let case = format!("{len} bytes that are no public key");
        let bytes = service_com_amazonaws_kms_trentservice::state::random_bytes(len)
            .unwrap_or_else(|| panic!("case {case}: the platform produces entropy"));
        let (status, code, message) = refused(
            &router,
            REGION,
            OPERATION,
            &json!({
                "KeyId": key_id,
                "KeyAgreementAlgorithm": ECDH,
                "PublicKey": b64(&bytes),
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "ValidationException",
                invalid_public_key("ECC_NIST_P256").as_str()
            ),
            "case {case}: expected the invalid-public-key refusal, got {status} {code} \
             {message}"
        );
    }
}

/// A key that cannot derive a shared secret at all is refused `InvalidKeyUsageException`:
/// "the key usage of the KMS key must be KEY_AGREEMENT"
/// (`smithy_query(query="doc",shape="DeriveSharedSecretRequest",member="KeyId")`).
#[tokio::test]
async fn a_key_that_cannot_agree_is_refused() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");
    let (_, peer_public_key) =
        peer_key_pair(&router, "usage", &wrapping_key, "ECC_NIST_P256").await;

    // One key of every OTHER `KeyUsage` the model declares
    // (`smithy_query(shape="KeyUsageType")`), including a key pair on a curve that CAN
    // agree but was minted for signing instead — the usage, not the curve, is what decides.
    let wrong_usage = [
        ("ECC_NIST_P256", "SIGN_VERIFY"),
        ("ECC_NIST_EDWARDS25519", "SIGN_VERIFY"),
        ("RSA_2048", "ENCRYPT_DECRYPT"),
        ("SYMMETRIC_DEFAULT", "ENCRYPT_DECRYPT"),
        ("HMAC_256", "GENERATE_VERIFY_MAC"),
        ("ML_KEM_512", "ENCAPSULATE_DECAPSULATE"),
    ];
    for (key_spec, key_usage) in wrong_usage {
        let case = format!("{key_spec} under {key_usage}");
        let metadata = super::harness::create_key(
            &router,
            &json!({"Description": &case, "KeySpec": key_spec, "KeyUsage": key_usage}),
        )
        .await;
        let key_id = text(&case, &metadata, "KeyId");
        let (status, code, message) = refused(
            &router,
            REGION,
            OPERATION,
            &json!({
                "KeyId": key_id,
                "KeyAgreementAlgorithm": ECDH,
                "PublicKey": b64(&peer_public_key),
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidKeyUsageException"),
            "case {case}: expected the key-usage refusal, got {status} {code} {message}"
        );
    }
}

/// A key that is not in a usable key state, an identifier that names no key, and a dry run
/// are each refused — and none of them derives anything.
#[tokio::test]
async fn an_unusable_key_and_a_dry_run_are_refused() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");
    let (_, peer_public_key) =
        peer_key_pair(&router, "state", &wrapping_key, "ECC_NIST_P256").await;
    let request = |identifier: &str, dry_run: bool| {
        json!({
            "KeyId": identifier,
            "KeyAgreementAlgorithm": ECDH,
            "PublicKey": b64(&peer_public_key),
            "DryRun": dry_run,
        })
    };

    // A disabled key: "the request was rejected because the specified KMS key is not
    // enabled" (`smithy_query(query="doc",shape="DisabledException")`).
    let (disabled, _) = agreement_key(&router, "ECC_NIST_P256", "disabled").await;
    ok(&router, "DisableKey", &json!({"KeyId": disabled})).await;
    let (status, code, message) =
        refused(&router, REGION, OPERATION, &request(&disabled, false)).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "DisabledException"),
        "case disabled: expected the disabled refusal, got {status} {code} {message}"
    );

    // A key scheduled for deletion is in a key state no cryptographic operation is valid
    // for, which names the state the key is in.
    let (pending, _) = agreement_key(&router, "ECC_NIST_P256", "pending deletion").await;
    ok(
        &router,
        "ScheduleKeyDeletion",
        &json!({"KeyId": pending, "PendingWindowInDays": 7}),
    )
    .await;
    let (status, code, message) =
        refused(&router, REGION, OPERATION, &request(&pending, false)).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "KMSInvalidStateException"),
        "case pending deletion: expected the key-state refusal, got {status} {code} {message}"
    );
    assert!(
        message.contains(&pending),
        "case pending deletion: the refusal must name the key, got {message}"
    );

    // An identifier that names no key of this account.
    let (status, code, message) = refused(
        &router,
        REGION,
        OPERATION,
        &request("00000000-1111-2222-3333-444444444444", false),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "NotFoundException"),
        "case unknown key: expected the not-found refusal, got {status} {code} {message}"
    );

    // "Checks if your request will succeed. DryRun is an optional parameter" — a request
    // that would otherwise succeed is still refused.
    let (usable, _) = agreement_key(&router, "ECC_NIST_P256", "dry run").await;
    let (status, code, message) =
        refused(&router, REGION, OPERATION, &request(&usable, true)).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "DryRunOperationException"),
        "case dry run: expected the dry-run refusal, got {status} {code} {message}"
    );
    // The same request without the dry run succeeds, so the refusal was the dry run and
    // nothing else about the request.
    let answer = ok(&router, OPERATION, &request(&usable, false)).await;
    assert_eq!(
        unb64("dry run", &answer, "SharedSecret").len(),
        32,
        "case dry run: the request succeeds without DryRun, got {answer}"
    );
}

/// Deriving a shared secret is a USE of the key, which KMS's usage tracking records:
/// `DeriveSharedSecret` is one of the tracked operations
/// (`smithy_query(query="shape",shape="KeyLastUsageTrackingOperation")`).
#[tokio::test]
async fn deriving_a_shared_secret_is_a_recorded_use_of_the_key() {
    let router = router();
    let wrapping_key = text("peer pairs", &enabled_key(&router).await, "KeyId");
    let (key_id, _) = agreement_key(&router, "ECC_NIST_P256", "last usage").await;
    let (_, peer_public_key) =
        peer_key_pair(&router, "last usage", &wrapping_key, "ECC_NIST_P256").await;

    derive(&router, &key_id, &peer_public_key).await;

    let answer = tracked_last_usage(&router, "last usage", &key_id, OPERATION, &Value::Null).await;
    assert_eq!(
        answer["KeyId"],
        json!(key_id),
        "case last usage: expected the key id {key_id}, got {answer}"
    );
    assert_eq!(
        answer["KeyLastUsage"]["Operation"],
        json!(OPERATION),
        "case last usage: expected the {OPERATION} record, got {answer}"
    );
}
