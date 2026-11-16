// SPDX-License-Identifier: CC0-1.0

//! The import parameters this control plane hands out, end to end over the wire:
//! `GetParametersForImport`.
//!
//! Every call goes through the real `MultiServiceRouter` (`POST /` with
//! `X-Amz-Target: PaymentCryptographyControlPlane.<Op>`, awsJson1_0) and the
//! modelled-constraint walk, through the same harness `key_lifecycle` uses, so what these
//! tests assert is what a signed SDK request observes.
//!
//! The rules under test:
//!
//! * an import parameter set is MINTED state: the call stores one set and answers it, so
//!   its token names a wrapping key this service holds, its wrapping key certificate is a
//!   real X.509 certificate of a real RSA key pair of the algorithm that was asked for,
//!   signed by the chain it is handed with, and both expire thirty days out;
//! * a set is reused only when the caller asks for the last generated token and one with
//!   enough validity left exists for the same key material type AND wrapping key
//!   algorithm — the export side's sets are its own and are never answered here;
//! * a wrapping key certificate is issued only for an algorithm the key material type
//!   admits, and every other modelled `KeyAlgorithm` is refused with
//!   `ValidationException`.
//!
//! Each case is a row of a table spanning what those rules generalise over: every modelled
//! `KeyMaterialType`, every modelled `KeyAlgorithm` against the two material types an
//! import token is required for, and whether the caller asks to reuse.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::ops::types::{
    KEY_ALGORITHM_VALUES, KEY_MATERIAL_TYPE_VALUES,
};
use service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::state::{
    KeyExchangeDirection, IMPORT_TOKEN_PREFIX, KEY_EXCHANGE_PARAMETERS_VALIDITY,
    TR34_SIGNING_KEY_ALGORITHM,
};
use service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::ComAmazonawsPaymentcryptographyPaymentcryptographycontrolplaneState as KeyStore;

use crate::export_and_replication_parameters::assert_real_certificate_chain;
use crate::key_lifecycle::{
    call, error_code, now_secs, ok_call, router, router_with_engine, string, timestamp,
};

/// The wrapping key algorithms each modelled `KeyMaterialType` admits.
///
/// Source: `WrappingKeyAlgorithm`'s own documentation
/// (`smithy_query(query="doc",shape="GetParametersForImportInput",member="WrappingKeyAlgorithm")`),
/// which states the rule as "At this time, RSA_2048 is the allowed algorithm for TR-34
/// WrappedKeyBlock import. Additionally, RSA_2048, RSA_3072, RSA_4096 are the allowed
/// algorithms for RSA WrappedKeyCryptogram import." A wrapping key is an RSA key in either
/// case, so a material type that sentence does not name takes the RSA set rather than the
/// narrower TR-34 one. The material types are
/// `smithy_query(query="shape",shape="KeyMaterialType")`, pinned against the generated
/// value list by `every_modelled_key_material_type_is_covered` below.
const ADMITTED_WRAPPING_KEY_ALGORITHMS: &[(&str, &[&str])] = &[
    ("TR34_KEY_BLOCK", &["RSA_2048"]),
    ("KEY_CRYPTOGRAM", &["RSA_2048", "RSA_3072", "RSA_4096"]),
    ("TR31_KEY_BLOCK", &["RSA_2048", "RSA_3072", "RSA_4096"]),
    ("ROOT_PUBLIC_KEY_CERTIFICATE", &["RSA_2048", "RSA_3072", "RSA_4096"]),
    (
        "TRUSTED_PUBLIC_KEY_CERTIFICATE",
        &["RSA_2048", "RSA_3072", "RSA_4096"],
    ),
];

/// The algorithms `key_material_type` admits, from the table above.
fn admitted(key_material_type: &str) -> &'static [&'static str] {
    ADMITTED_WRAPPING_KEY_ALGORITHMS
        .iter()
        .find(|(material_type, _)| *material_type == key_material_type)
        .map(|(_, algorithms)| *algorithms)
        .unwrap_or_else(|| panic!("the table names every modelled KeyMaterialType, not {key_material_type}"))
}

/// One `GetParametersForImport` request body.
fn request(key_material_type: &str, wrapping_key_algorithm: &str, reuse: Option<bool>) -> Value {
    let mut body = json!({
        "KeyMaterialType": key_material_type,
        "WrappingKeyAlgorithm": wrapping_key_algorithm,
    });
    if let Some(reuse) = reuse {
        body["ReuseLastGeneratedToken"] = Value::Bool(reuse);
    }
    body
}

/// Assert `token` satisfies the modelled `ImportTokenId` pattern
/// `^(import-token-[0-9a-zA-Z]{16,64})?$`
/// (`smithy_query(query="operation",operation="GetParametersForImport")`), checked by hand
/// because this crate has no regex dependency.
fn assert_import_token(case: &str, token: &str) {
    let id = token.strip_prefix(IMPORT_TOKEN_PREFIX).unwrap_or_else(|| {
        panic!("case {case}: ImportToken {token:?} does not start with {IMPORT_TOKEN_PREFIX}")
    });
    assert!(
        (16..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case}: ImportToken {token:?} carries the id segment {id:?}, which is not \
         [0-9a-zA-Z]{{16,64}}"
    );
}

/// The modulus size, in bits, the `KeyAlgorithm` `algorithm` names: the digits of the
/// modelled `RSA_<bits>` values (`smithy_query(query="shape",shape="KeyAlgorithm")`).
fn rsa_modulus_bits(algorithm: &str) -> usize {
    algorithm
        .strip_prefix("RSA_")
        .and_then(|bits| bits.parse().ok())
        .unwrap_or_else(|| panic!("{algorithm} is not one of the modelled RSA algorithms"))
}

/// Assert the certificate the answer carries names a key of `algorithm`.
///
/// The public key a certificate carries is the DER `RSAPublicKey` of its modulus and
/// exponent, so its encoding is the modulus plus a small, fixed header — which is enough
/// to tell the modelled modulus sizes apart (they are 128 bytes apart at the closest) and
/// so to catch a handler that mints one size whatever was asked for.
fn assert_certificate_key_size(case: &str, algorithm: &str, certificate: &::x509_cert::Certificate) {
    let modulus_bytes = rsa_modulus_bits(algorithm) / 8;
    let key = certificate
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .raw_bytes();
    assert!(
        (modulus_bytes..modulus_bytes + 64).contains(&key.len()),
        "case {case}: expected the wrapping key certificate to carry a {algorithm} public \
         key (about {modulus_bytes} bytes of modulus), got {} bytes",
        key.len()
    );
}

/// The parameter set the engine holds under `token`, so a response is never richer than
/// the record it came from.
fn stored(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    token: &str,
) -> (KeyExchangeDirection, String, String, String) {
    let store = engine.state::<KeyStore>();
    let guard = store.lock();
    let parameters = guard
        .key_exchange_parameters
        .get(token)
        .unwrap_or_else(|| panic!("case {case}: no parameter set stored under {token}"));
    (
        parameters.direction,
        parameters.key_algorithm.clone(),
        parameters.certificate.clone(),
        parameters.certificate_chain.clone(),
    )
}

/// The table above has to name every modelled `KeyMaterialType`, so a material type the
/// model gains later cannot slip through untested.
#[test]
fn every_modelled_key_material_type_is_covered() {
    let tabled: BTreeSet<&str> = ADMITTED_WRAPPING_KEY_ALGORITHMS
        .iter()
        .map(|(material_type, _)| *material_type)
        .collect();
    let modelled: BTreeSet<&str> = KEY_MATERIAL_TYPE_VALUES.iter().copied().collect();
    assert_eq!(
        tabled, modelled,
        "the admitted-algorithm table must name exactly the modelled KeyMaterialType values, \
         got {tabled:?} against {modelled:?}"
    );
    for (material_type, algorithms) in ADMITTED_WRAPPING_KEY_ALGORITHMS {
        for algorithm in *algorithms {
            assert!(
                KEY_ALGORITHM_VALUES.contains(algorithm),
                "case {material_type}: {algorithm} is not a modelled KeyAlgorithm"
            );
        }
    }
}

#[tokio::test]
async fn get_parameters_for_import_mints_a_real_wrapping_certificate_per_token() {
    let (router, engine) = router_with_engine();
    let mut tokens: BTreeSet<String> = BTreeSet::new();

    // One row per modelled `KeyMaterialType`, each asking for the first algorithm that
    // material type admits.
    for key_material_type in KEY_MATERIAL_TYPE_VALUES {
        let algorithm = admitted(key_material_type)[0];
        let case = format!("{key_material_type} with {algorithm}");
        let answered = ok_call(
            &router,
            "GetParametersForImport",
            &request(key_material_type, algorithm, None),
        )
        .await;

        assert_import_token(&case, string(&answered, "ImportToken"));
        assert_eq!(
            string(&answered, "WrappingKeyAlgorithm"),
            algorithm,
            "case {case}: expected the wrapping key algorithm asked for, got {answered}"
        );
        // "The import token expires in 30 days" (the operation's own documentation).
        let expires = timestamp(&answered, "ParametersValidUntilTimestamp");
        let expected = now_secs() + KEY_EXCHANGE_PARAMETERS_VALIDITY.as_secs_f64();
        assert!(
            (expires - expected).abs() < 60.0,
            "case {case}: expected the parameters to expire around {expected}, got {expires}"
        );
        let certificate = assert_real_certificate_chain(
            &case,
            "WrappingKeyCertificate",
            "WrappingKeyCertificateChain",
            &answered,
        );
        assert_certificate_key_size(&case, algorithm, &certificate);

        // The answer comes from the set that was stored, under the token that names it,
        // on the import side of the exchange.
        let token = string(&answered, "ImportToken").to_string();
        let (direction, stored_algorithm, stored_certificate, stored_chain) =
            stored(&engine, &case, &token);
        assert_eq!(
            direction,
            KeyExchangeDirection::Import,
            "case {case}: the stored set must be an import set"
        );
        assert_eq!(
            stored_algorithm, algorithm,
            "case {case}: the stored set must carry the algorithm that was asked for"
        );
        assert_eq!(
            stored_certificate,
            string(&answered, "WrappingKeyCertificate"),
            "case {case}: the response must report the stored certificate"
        );
        assert_eq!(
            stored_chain,
            string(&answered, "WrappingKeyCertificateChain"),
            "case {case}: the response must report the stored certificate chain"
        );

        // Each call that does not reuse mints its own token and its own key pair.
        assert!(
            tokens.insert(token.clone()),
            "case {case}: expected a token no earlier call handed out, got {token} again"
        );
    }
}

#[tokio::test]
async fn get_parameters_for_import_reuses_the_last_token_of_the_same_type_and_algorithm() {
    let router = router();
    // The rows differ along what the reuse rule is keyed on: the key material type and
    // the wrapping key algorithm. `KEY_CRYPTOGRAM` is the material type that admits more
    // than one algorithm, so two of its sets differ in the algorithm alone.
    let rows: &[(&str, &str)] = &[
        ("TR34_KEY_BLOCK", "RSA_2048"),
        ("KEY_CRYPTOGRAM", "RSA_2048"),
        ("KEY_CRYPTOGRAM", "RSA_3072"),
    ];

    // The last token each row was handed, so a reuse request is held to its OWN row's set
    // rather than to whichever set was minted last.
    let mut last: BTreeMap<(&str, &str), Value> = BTreeMap::new();
    for (key_material_type, algorithm) in rows {
        let case = format!("{key_material_type} with {algorithm}");
        let first = ok_call(
            &router,
            "GetParametersForImport",
            &request(key_material_type, algorithm, None),
        )
        .await;
        // "The default value is false, which generates a new import token and wrapping
        // key certificate on every call" (`ReuseLastGeneratedToken`'s own documentation),
        // so a second call with the flag absent mints a second set.
        let second = ok_call(
            &router,
            "GetParametersForImport",
            &request(key_material_type, algorithm, Some(false)),
        )
        .await;
        assert_ne!(
            string(&first, "ImportToken"),
            string(&second, "ImportToken"),
            "case {case}: a call that does not reuse must mint a new token"
        );
        assert_ne!(
            string(&first, "WrappingKeyCertificate"),
            string(&second, "WrappingKeyCertificate"),
            "case {case}: a call that does not reuse must mint a new wrapping key"
        );
        last.insert((key_material_type, algorithm), second);
    }

    // …and one that asks to reuse answers that row's own last set, whole: the token, the
    // certificate, the chain and the expiry it was minted with.
    for (key_material_type, algorithm) in rows {
        let case = format!("reuse of {key_material_type} with {algorithm}");
        let expected = last
            .get(&(*key_material_type, *algorithm))
            .unwrap_or_else(|| panic!("the loop recorded a set for {case}"));
        let reused = ok_call(
            &router,
            "GetParametersForImport",
            &request(key_material_type, algorithm, Some(true)),
        )
        .await;
        for member in [
            "ImportToken",
            "WrappingKeyCertificate",
            "WrappingKeyCertificateChain",
            "WrappingKeyAlgorithm",
        ] {
            assert_eq!(
                string(&reused, member),
                string(expected, member),
                "case {case}: expected the last generated set's {member}, got {reused}"
            );
        }
        assert_eq!(
            timestamp(&reused, "ParametersValidUntilTimestamp"),
            timestamp(expected, "ParametersValidUntilTimestamp"),
            "case {case}: a reused token keeps its own expiry"
        );
    }

    // The two sides of the exchange keep their own sets: an export token is never
    // answered as an import token, and asking the export side to reuse after an import
    // set was minted for the same material type and algorithm mints an export set of its
    // own.
    let exported = ok_call(
        &router,
        "GetParametersForExport",
        &json!({
            "KeyMaterialType": "TR34_KEY_BLOCK",
            "SigningKeyAlgorithm": TR34_SIGNING_KEY_ALGORITHM,
            "ReuseLastGeneratedToken": true,
        }),
    )
    .await;
    let imported = last
        .get(&("TR34_KEY_BLOCK", "RSA_2048"))
        .expect("the loop recorded a TR-34 import set");
    assert_ne!(
        string(&exported, "ExportToken"),
        string(imported, "ImportToken"),
        "case the two directions: an export call must not answer an import set's token"
    );
    assert_ne!(
        string(&exported, "SigningKeyCertificate"),
        string(imported, "WrappingKeyCertificate"),
        "case the two directions: an export call must not answer an import set's certificate"
    );
}

#[tokio::test]
async fn get_parameters_for_import_admits_only_the_algorithms_its_material_type_names() {
    let router = router();

    // Every modelled `KeyAlgorithm` against the two material types an import token is
    // required for ("Import token is only required for TR-34 WrappedKeyBlock
    // (TR34_KEY_BLOCK) and RSA WrappedKeyCryptogram (KEY_CRYPTOGRAM)",
    // `KeyMaterialType`'s own documentation) — the whole published list, not a sample.
    for key_material_type in ["TR34_KEY_BLOCK", "KEY_CRYPTOGRAM"] {
        let admitted = admitted(key_material_type);
        for wrapping_key_algorithm in KEY_ALGORITHM_VALUES {
            let case = format!("{key_material_type} wrapped under {wrapping_key_algorithm}");
            let (status, parsed) = call(
                &router,
                "GetParametersForImport",
                &request(key_material_type, wrapping_key_algorithm, None),
            )
            .await;

            if admitted.contains(wrapping_key_algorithm) {
                assert_eq!(
                    status, 200,
                    "case {case}: an admitted algorithm must be served, got {status} {parsed}"
                );
                let certificate = assert_real_certificate_chain(
                    &case,
                    "WrappingKeyCertificate",
                    "WrappingKeyCertificateChain",
                    &parsed,
                );
                assert_certificate_key_size(&case, wrapping_key_algorithm, &certificate);
                continue;
            }

            assert_eq!(
                status, 400,
                "case {case}: expected 400, got {status} {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {case}: expected ValidationException, got {parsed}"
            );
            let expected = format!(
                "WrappingKeyAlgorithm {wrapping_key_algorithm} is not valid for the \
                 KeyMaterialType {key_material_type}."
            );
            assert_eq!(
                parsed.get("Message").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case}: expected the message {expected:?}, got {parsed}"
            );
        }
    }
}
