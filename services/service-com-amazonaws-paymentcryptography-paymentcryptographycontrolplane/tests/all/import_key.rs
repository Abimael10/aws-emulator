// SPDX-License-Identifier: CC0-1.0

//! `ImportKey`, end to end over the wire: the key a wrapped key block or a key
//! cryptogram brings into this service, and what is refused instead of stored.
//!
//! Every call goes through the real `MultiServiceRouter` (`POST /` with
//! `X-Amz-Target: PaymentCryptographyControlPlane.<Op>`, awsJson1_0) and the
//! modelled-constraint walk, through the same harness `key_lifecycle` and `export_key`
//! use, so what these tests assert is what a signed SDK request observes.
//!
//! The rule under test: a key arrives in this service as the key its wrapped material
//! carries — the material is recovered under the key encryption key the request names,
//! the imported key's own attributes are the ones that material itself describes (a key
//! block's header for a key block, the declaration the cryptogram's own mechanism
//! carries for a cryptogram), its origin is `EXTERNAL`, and the key the store ends up
//! holding is byte for byte the key that was wrapped. Material that is not intact under
//! the key named, and a key encryption key that cannot unwrap, are refused rather than
//! stored.
//!
//! Each case is one row of a table spanning what the rule generalises over — both key
//! block binding methods, each symmetric key algorithm, each mode of use a header can
//! carry, every exportability the header can carry, each AS 2805 key variant, and each
//! way wrapped material can fail to be intact — not the one algorithm an issue happened
//! to name. That the recovered key is the key that was wrapped is asserted through the
//! check value both ends report, which is a function of the key's own material.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::ops::types::AS2805_KEY_VARIANT_VALUES;

use crate::key_lifecycle::{call, error_code, error_message, ok_call, router, string};

/// The `KeyUsage` of a key encryption key, the usage every key below is wrapped and
/// unwrapped under.
const KEK_USAGE: &str = "TR31_K0_KEY_ENCRYPTION_KEY";

/// The `KeyOrigin` of an imported key, from `Key.KeyOrigin`'s own documentation: "For
/// keys imported into Amazon Web Services Payment Cryptography, the value is
/// `EXTERNAL`."
const ORIGIN_IMPORTED: &str = "EXTERNAL";

/// The sentence an import of a key block that is not valid key block data is refused
/// with.
const KEY_BLOCK_INVALID: &str = "KeyBlock data in the imported payload is invalid.";

/// A `CreateKey` body for one symmetric key.
fn create_body(key_usage: &str, key_algorithm: &str, modes: &Value, exportable: bool) -> Value {
    json!({
        "KeyAttributes": {
            "KeyUsage": key_usage,
            // Every algorithm below is symmetric, which is the class the model gives
            // those algorithms.
            "KeyClass": "SYMMETRIC_KEY",
            "KeyAlgorithm": key_algorithm,
            "KeyModesOfUse": modes,
        },
        "Exportable": exportable,
    })
}

/// Create one key and answer the `Key` the service stored.
async fn create_key(
    router: &MultiServiceRouter,
    key_usage: &str,
    key_algorithm: &str,
    modes: &Value,
    exportable: bool,
) -> Value {
    let body = create_body(key_usage, key_algorithm, modes, exportable);
    let created = ok_call(router, "CreateKey", &body).await;
    created
        .get("Key")
        .cloned()
        .unwrap_or_else(|| panic!("no Key in {created}"))
}

/// Create a key encryption key of `key_algorithm` that may both wrap and unwrap.
async fn create_kek(router: &MultiServiceRouter, key_algorithm: &str) -> String {
    let key = create_key(
        router,
        KEK_USAGE,
        key_algorithm,
        &json!({"Wrap": true, "Unwrap": true}),
        false,
    )
    .await;
    string(&key, "KeyArn").to_string()
}

/// Export `key_identifier` in a TR-31 key block under `wrapping`, with the header
/// fields `headers` asks for, and answer the `WrappedKey`.
async fn export_tr31(
    router: &MultiServiceRouter,
    key_identifier: &str,
    wrapping: &str,
    headers: Option<Value>,
) -> Value {
    let mut block = json!({"WrappingKeyIdentifier": wrapping});
    if let Some(headers) = headers {
        block["KeyBlockHeaders"] = headers;
    }
    let body = json!({
        "ExportKeyIdentifier": key_identifier,
        "KeyMaterial": {"Tr31KeyBlock": block},
    });
    let exported = ok_call(router, "ExportKey", &body).await;
    exported
        .get("WrappedKey")
        .cloned()
        .unwrap_or_else(|| panic!("no WrappedKey in {exported}"))
}

/// An `ImportKey` body for a TR-31 key block.
fn import_tr31_body(wrapping: &str, block: &str) -> Value {
    json!({
        "KeyMaterial": {
            "Tr31KeyBlock": {
                "WrappingKeyIdentifier": wrapping,
                "WrappedKeyBlock": block,
            }
        }
    })
}

/// Import a TR-31 key block and answer the `Key` the service stored.
async fn import_tr31(router: &MultiServiceRouter, wrapping: &str, block: &str) -> Value {
    let imported = ok_call(router, "ImportKey", &import_tr31_body(wrapping, block)).await;
    imported
        .get("Key")
        .cloned()
        .unwrap_or_else(|| panic!("no Key in {imported}"))
}

/// The `Key` a `GetKey` reports for `key_identifier`.
async fn get_key(router: &MultiServiceRouter, key_identifier: &str) -> Value {
    let read = ok_call(router, "GetKey", &json!({"KeyIdentifier": key_identifier})).await;
    read.get("Key")
        .cloned()
        .unwrap_or_else(|| panic!("no Key in {read}"))
}

/// One row of the round-trip table: a key to wrap, the key encryption key to wrap it
/// under, and the modes of use the key comes back with.
struct RoundTrip {
    case: &'static str,
    /// The `KeyUsage` of the key under export, which the block's header carries.
    key_usage: &'static str,
    /// Its `KeyAlgorithm`, which the header's algorithm character and the payload's
    /// length together name on the way back.
    key_algorithm: &'static str,
    /// Its `KeyModesOfUse`, which the header's mode of use character carries.
    modes: Value,
    /// The modelled `KeyModesOfUse` members the imported key permits: the ones the mode
    /// of use character the header carried names. A character names both the data
    /// operation and the key operation it permits — the standard reads `B` as "both
    /// encrypt and decrypt / wrap and unwrap" — so a key exported permitting one of a
    /// pair comes back permitting both, which the first row below is the case of.
    expected_modes: &'static [&'static str],
    /// The `KeyAlgorithm` of the key encryption key: a TDES one binds the block by the
    /// standard's TDES method and an AES one by its AES method.
    wrapping_key_algorithm: &'static str,
}

/// Every row spans one thing the rule generalises over: both binding methods, each
/// symmetric key algorithm a key block's header and payload length name, and every mode
/// of use character a header can carry.
fn round_trips() -> Vec<RoundTrip> {
    vec![
        RoundTrip {
            case: "a key encryption key, wrap and unwrap, under an AES-256 key",
            key_usage: KEK_USAGE,
            key_algorithm: "TDES_2KEY",
            modes: json!({"Wrap": true, "Unwrap": true}),
            expected_modes: &["Encrypt", "Decrypt", "Wrap", "Unwrap"],
            wrapping_key_algorithm: "AES_256",
        },
        RoundTrip {
            case: "a data key, encrypt, decrypt, wrap and unwrap, under a TDES-3KEY key",
            key_usage: "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            key_algorithm: "AES_128",
            modes: json!({"Encrypt": true, "Decrypt": true, "Wrap": true, "Unwrap": true}),
            expected_modes: &["Encrypt", "Decrypt", "Wrap", "Unwrap"],
            wrapping_key_algorithm: "TDES_3KEY",
        },
        RoundTrip {
            case: "a base derivation key, derive only, under an AES-128 key",
            key_usage: "TR31_B0_BASE_DERIVATION_KEY",
            key_algorithm: "TDES_3KEY",
            modes: json!({"DeriveKey": true}),
            expected_modes: &["DeriveKey"],
            wrapping_key_algorithm: "AES_128",
        },
        RoundTrip {
            case: "a PIN encryption key, encrypt and wrap, under a TDES-2KEY key",
            key_usage: "TR31_P0_PIN_ENCRYPTION_KEY",
            key_algorithm: "AES_192",
            modes: json!({"Encrypt": true, "Wrap": true}),
            expected_modes: &["Encrypt", "Wrap"],
            wrapping_key_algorithm: "TDES_2KEY",
        },
        RoundTrip {
            case: "a data key, decrypt and unwrap, under an AES-128 key",
            key_usage: "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            key_algorithm: "AES_128",
            modes: json!({"Decrypt": true, "Unwrap": true}),
            expected_modes: &["Decrypt", "Unwrap"],
            wrapping_key_algorithm: "AES_128",
        },
        RoundTrip {
            case: "a MAC key, generate and verify, under an AES-192 key",
            key_usage: "TR31_M7_HMAC_KEY",
            key_algorithm: "AES_256",
            modes: json!({"Generate": true, "Verify": true}),
            expected_modes: &["Generate", "Verify"],
            wrapping_key_algorithm: "AES_192",
        },
        RoundTrip {
            case: "a MAC key, generate only, under a TDES-2KEY key",
            key_usage: "TR31_M6_ISO_9797_5_CMAC_KEY",
            key_algorithm: "AES_128",
            modes: json!({"Generate": true}),
            expected_modes: &["Generate"],
            wrapping_key_algorithm: "TDES_2KEY",
        },
        RoundTrip {
            case: "a MAC key, sign only, under an AES-256 key",
            key_usage: "TR31_M1_ISO_9797_1_MAC_KEY",
            key_algorithm: "AES_192",
            modes: json!({"Sign": true}),
            expected_modes: &["Sign"],
            wrapping_key_algorithm: "AES_256",
        },
        RoundTrip {
            case: "a MAC key, sign and decipher, under a TDES-3KEY key",
            key_usage: "TR31_M1_ISO_9797_1_MAC_KEY",
            key_algorithm: "TDES_3KEY",
            modes: json!({"Sign": true, "Decrypt": true, "Unwrap": true}),
            expected_modes: &["Sign", "Decrypt", "Unwrap"],
            wrapping_key_algorithm: "TDES_3KEY",
        },
        RoundTrip {
            case: "a PIN verification key, verify only, under an AES-128 key",
            key_usage: "TR31_V2_VISA_PIN_VERIFICATION_KEY",
            key_algorithm: "TDES_2KEY",
            modes: json!({"Verify": true}),
            expected_modes: &["Verify"],
            wrapping_key_algorithm: "AES_128",
        },
        RoundTrip {
            case: "a key with no restrictions on its use, under a TDES-3KEY key",
            key_usage: "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            key_algorithm: "AES_256",
            modes: json!({"NoRestrictions": true}),
            expected_modes: &["NoRestrictions"],
            wrapping_key_algorithm: "TDES_3KEY",
        },
    ]
}

/// The nine modelled `KeyModesOfUse` members, from
/// `smithy_query(query="shape",shape="KeyModesOfUse")`.
const MODE_MEMBERS: &[&str] = &[
    "Encrypt",
    "Decrypt",
    "Wrap",
    "Unwrap",
    "Generate",
    "Sign",
    "Verify",
    "DeriveKey",
    "NoRestrictions",
];

/// Assert `modes` permits exactly the members `permitted` names and nothing else.
fn assert_modes(case: &str, modes: &Value, permitted: &[&str]) {
    for member in MODE_MEMBERS {
        let expected = permitted.contains(member);
        assert_eq!(
            modes.get(*member).and_then(Value::as_bool),
            Some(expected),
            "case {case}: expected {member} to be {expected}, got {modes}"
        );
    }
}

#[tokio::test]
async fn import_key_stores_the_key_the_tr31_key_block_carries() {
    let router = router();

    for row in round_trips() {
        let case = row.case;
        let wrapping = create_kek(&router, row.wrapping_key_algorithm).await;
        let created = create_key(
            &router,
            row.key_usage,
            row.key_algorithm,
            &row.modes,
            true,
        )
        .await;
        let exported = export_tr31(
            &router,
            string(&created, "KeyArn"),
            &wrapping,
            None,
        )
        .await;

        let imported = import_tr31(&router, &wrapping, string(&exported, "KeyMaterial")).await;

        // A fresh record of this store, not the key that was exported.
        assert_ne!(
            string(&imported, "KeyArn"),
            string(&created, "KeyArn"),
            "case {case}: an import stores a key of its own, got {imported}"
        );
        // The key inside the block is the key that comes out: the check value is a
        // function of the key's own material, and the one the export reported for the
        // material it wrapped is the one the imported record reports.
        assert_eq!(
            imported.get("KeyCheckValue"),
            exported.get("KeyCheckValue"),
            "case {case}: expected the check value of the wrapped key {:?}, got {imported}",
            exported.get("KeyCheckValue")
        );
        assert_eq!(
            imported.get("KeyCheckValue"),
            created.get("KeyCheckValue"),
            "case {case}: expected the check value of the key that was exported {:?}, got \
             {imported}",
            created.get("KeyCheckValue")
        );
        // Every attribute is the one the block's header described, which is what the
        // exported key carried.
        for member in ["KeyUsage", "KeyClass", "KeyAlgorithm"] {
            assert_eq!(
                imported["KeyAttributes"].get(member),
                created["KeyAttributes"].get(member),
                "case {case}: expected the {member} of the wrapped key, got {imported}"
            );
        }
        // …and what may be done with the imported key is what the header's mode of use
        // character permits.
        assert_modes(
            case,
            &imported["KeyAttributes"]["KeyModesOfUse"],
            row.expected_modes,
        );
        assert_eq!(
            string(&imported, "KeyOrigin"),
            ORIGIN_IMPORTED,
            "case {case}: expected the origin {ORIGIN_IMPORTED}, got {imported}"
        );
        assert_eq!(
            imported.get("Enabled"),
            Some(&Value::Bool(true)),
            "case {case}: expected an imported key to be enabled, got {imported}"
        );

        // …and the record a later reader sees is that same record.
        let read = get_key(&router, string(&imported, "KeyArn")).await;
        assert_eq!(
            read, imported,
            "case {case}: expected GetKey to report the imported key, got {read}"
        );
    }
}

#[tokio::test]
async fn import_key_takes_exportability_from_the_key_block_header() {
    let router = router();
    // `KeyExportability`'s three modelled values
    // (`smithy_query(query="shape",shape="KeyExportability")`), each with whether a key
    // marked that way in a key block header may leave this service again.
    let cases: [(&str, bool); 3] = [
        ("EXPORTABLE", true),
        ("SENSITIVE", true),
        ("NON_EXPORTABLE", false),
    ];

    for (exportability, exportable) in cases {
        let wrapping = create_kek(&router, "AES_256").await;
        let created = create_key(
            &router,
            "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            "AES_128",
            &json!({"Encrypt": true, "Decrypt": true}),
            true,
        )
        .await;
        let exported = export_tr31(
            &router,
            string(&created, "KeyArn"),
            &wrapping,
            Some(json!({"KeyExportability": exportability})),
        )
        .await;
        let imported = import_tr31(&router, &wrapping, string(&exported, "KeyMaterial")).await;

        assert_eq!(
            imported.get("Exportable"),
            Some(&Value::Bool(exportable)),
            "case {exportability}: expected Exportable {exportable}, got {imported}"
        );

        // …and that is what a later export of the imported key is decided by.
        let arn = string(&imported, "KeyArn").to_string();
        let export_again = json!({
            "ExportKeyIdentifier": arn,
            "KeyMaterial": {"Tr31KeyBlock": {"WrappingKeyIdentifier": wrapping}},
        });
        let (status, parsed) = call(&router, "ExportKey", &export_again).await;
        if exportable {
            assert_eq!(
                status, 200,
                "case {exportability}: expected the imported key to export again, got {status} \
                 {parsed}"
            );
            let wrapped = parsed
                .get("WrappedKey")
                .cloned()
                .unwrap_or_else(|| panic!("case {exportability}: no WrappedKey in {parsed}"));
            assert_eq!(
                wrapped.get("KeyCheckValue"),
                imported.get("KeyCheckValue"),
                "case {exportability}: expected the exported material to be the imported key, \
                 got {wrapped}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {exportability}: expected 400, got {status} {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "ValidationException",
                "case {exportability}: expected ValidationException, got {parsed}"
            );
            let expected = format!("Export is not enabled for the key {arn}.");
            assert_eq!(
                error_message(&parsed),
                expected,
                "case {exportability}: expected the message {expected:?}, got {parsed}"
            );
        }
    }
}

/// `block` with the character at `at` changed to another the modelled
/// `Tr31WrappedKeyBlock` pattern `^[0-9a-zA-Z]+$` admits, so the mutation is refused
/// for what it is rather than for the shape of the string.
fn altered(block: &str, at: usize) -> String {
    block
        .chars()
        .enumerate()
        .map(|(index, character)| {
            if index != at {
                return character;
            }
            if character == '0' {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

#[tokio::test]
async fn import_key_refuses_wrapped_material_that_is_not_intact() {
    let router = router();
    let wrapping = create_kek(&router, "AES_256").await;
    let other_wrapping = create_kek(&router, "AES_256").await;
    let created = create_key(
        &router,
        "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
        "AES_192",
        &json!({"Encrypt": true, "Decrypt": true}),
        true,
    )
    .await;
    let exported = export_tr31(
        &router,
        string(&created, "KeyArn"),
        &wrapping,
        // Two optional blocks, so the header region a mutation can fall in is as wide
        // as a caller can make it.
        Some(json!({"OptionalBlocks": {"KS": "00112233445566", "TS": "2026"}})),
    )
    .await;
    let block = string(&exported, "KeyMaterial").to_string();
    let last = block.len() - 1;
    // An AES-bound key block closes with the 32 hexadecimal characters of its 16-byte
    // MAC, so the character before them is the last one of the wrapped payload.
    let payload_end = block.len() - 33;

    // Each row alters the block somewhere a party could alter it, or names a key
    // encryption key that did not wrap it: in each case the block is no longer the one
    // that key authenticated.
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a character of the wrapped payload",
            altered(&block, payload_end),
            wrapping.as_str(),
        ),
        ("a character of the MAC", altered(&block, last), wrapping.as_str()),
        (
            "the header's mode of use, which the MAC authenticates",
            altered(&block, 8),
            wrapping.as_str(),
        ),
        (
            "an optional block's value, which the MAC authenticates",
            altered(&block, 20),
            wrapping.as_str(),
        ),
        (
            "the length field, which no longer counts the block",
            altered(&block, 4),
            wrapping.as_str(),
        ),
        (
            "the whole block, under a key encryption key that did not wrap it",
            block.clone(),
            other_wrapping.as_str(),
        ),
        (
            "the block with its last two characters dropped",
            block[..block.len() - 2].to_string(),
            wrapping.as_str(),
        ),
    ];

    for (case, altered_block, key) in cases {
        let (status, parsed) = call(&router, "ImportKey", &import_tr31_body(key, &altered_block))
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
        // One sentence for the whole class: what a sending party is told is that the
        // payload's key block data is not valid, and nothing about which field of it
        // this service could not read.
        assert_eq!(
            error_message(&parsed),
            KEY_BLOCK_INVALID,
            "case {case}: expected the message {KEY_BLOCK_INVALID:?}, got {parsed}"
        );
    }

    // Nothing of any of them was stored: the one key this store holds beside the two
    // key encryption keys is the key that was created.
    let listed = ok_call(&router, "ListKeys", &json!({})).await;
    let keys = listed
        .get("Keys")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no Keys in {listed}"));
    assert_eq!(
        keys.len(),
        3,
        "expected no key to have been stored for a refused import, got {listed}"
    );
}

#[tokio::test]
async fn import_key_refuses_a_block_outside_the_published_length_bounds() {
    let router = router();
    let wrapping = create_kek(&router, "AES_256").await;
    // The published bounds of `Tr31WrappedKeyBlock`
    // (`smithy_query(query="operation",operation="ImportKey")` gives length 56..9984),
    // each bound and one character past it. A block past a bound is refused before any
    // of it is read, so no parse ever walks an oversized or a truncated one; a block of
    // exactly a bound's length reaches the handler and is refused for what it carries.
    let cases: [(&str, usize); 4] = [
        ("one character below the minimum", 55),
        ("exactly the minimum", 56),
        ("exactly the maximum", 9984),
        ("one character above the maximum", 9985),
    ];

    for (case, len) in cases {
        // Every character satisfies the modelled `^[0-9a-zA-Z]+$`, so the length is the
        // only thing about the block under test.
        let block = "0".repeat(len);
        let (status, parsed) = call(&router, "ImportKey", &import_tr31_body(&wrapping, &block))
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
    }
}

#[tokio::test]
async fn import_key_refuses_a_key_that_cannot_unwrap_the_material() {
    let router = router();
    let wrapping = create_kek(&router, "AES_256").await;
    let created = create_key(
        &router,
        "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
        "AES_128",
        &json!({"Encrypt": true, "Decrypt": true}),
        true,
    )
    .await;
    let exported = export_tr31(&router, string(&created, "KeyArn"), &wrapping, None).await;
    let block = string(&exported, "KeyMaterial").to_string();

    // A key encryption key that may wrap but not unwrap: the direction this call needs
    // is the one its modes of use do not permit.
    let wrap_only = create_key(&router, KEK_USAGE, "AES_256", &json!({"Wrap": true}), false).await;
    // A data encryption key is not a key keys are sent under at all.
    let data_key = create_key(
        &router,
        "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
        "AES_256",
        &json!({"Encrypt": true, "Decrypt": true, "Wrap": true, "Unwrap": true}),
        false,
    )
    .await;
    // A key encryption key whose use has been stopped.
    let stopped = create_kek(&router, "AES_256").await;
    ok_call(&router, "StopKeyUsage", &json!({"KeyIdentifier": stopped})).await;
    // An ARN of this account and region no key was ever minted under.
    let unknown = format!(
        "arn:aws:payment-cryptography:{}:000000000000:key/neverminted00000",
        crate::key_lifecycle::REGION
    );

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a key encryption key that may wrap but not unwrap",
            string(&wrap_only, "KeyArn").to_string(),
            "ValidationException",
        ),
        (
            "a data encryption key rather than a key encryption key",
            string(&data_key, "KeyArn").to_string(),
            "ValidationException",
        ),
        (
            "a key encryption key whose use has been stopped",
            stopped.clone(),
            "ValidationException",
        ),
        (
            "an identifier naming no key this account holds",
            unknown.clone(),
            "ResourceNotFoundException",
        ),
    ];

    for (case, identifier, expected_code) in cases {
        let (status, parsed) =
            call(&router, "ImportKey", &import_tr31_body(&identifier, &block)).await;
        // This service answers every client error under one status, whatever
        // `@httpError` the class's own shape declares (see `client_error_status`).
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            expected_code,
            "case {case}: expected {expected_code}, got {parsed}"
        );
        let named = parsed
            .get("Message")
            .or_else(|| parsed.get("ResourceId"))
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(
            named.contains(&identifier),
            "case {case}: expected the refusal to name {identifier}, got {parsed}"
        );
    }
}

/// The `KeyUsage` the key an AS 2805 key cryptogram carries under `variant` is held
/// under: each modelled `As2805KeyVariant` names the kind of key the standard forms
/// that variant for, and this is the modelled usage of that kind. A variant the model
/// gains later has no row here and fails this suite rather than passing silently.
fn usage_of(variant: &str) -> &'static str {
    match variant {
        "TERMINAL_MAJOR_KEY_VARIANT_00" => "TR31_K0_KEY_ENCRYPTION_KEY",
        "PIN_ENCRYPTION_KEY_VARIANT_28" => "TR31_P0_PIN_ENCRYPTION_KEY",
        "MESSAGE_AUTHENTICATION_KEY_VARIANT_24" => "TR31_M1_ISO_9797_1_MAC_KEY",
        "DATA_ENCRYPTION_KEY_VARIANT_22" => "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
        other => panic!("no key usage is expected for the key variant {other}"),
    }
}

#[tokio::test]
async fn import_key_stores_the_key_an_as2805_key_cryptogram_carries() {
    let router = router();
    // Each modelled `As2805KeyVariant`, from the generated value list of
    // `smithy_query(query="shape",shape="As2805KeyVariant")`, paired with a key
    // algorithm whose material is a whole number of TDES blocks.
    let key_algorithms = ["TDES_2KEY", "TDES_3KEY", "AES_128", "AES_192"];

    for (index, variant) in AS2805_KEY_VARIANT_VALUES.iter().enumerate() {
        let key_algorithm = key_algorithms[index % key_algorithms.len()];
        let wrapping = create_kek(&router, "TDES_3KEY").await;
        let created = create_key(
            &router,
            "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
            key_algorithm,
            &json!({"Encrypt": true, "Decrypt": true}),
            true,
        )
        .await;
        let exported = ok_call(
            &router,
            "ExportKey",
            &json!({
                "ExportKeyIdentifier": string(&created, "KeyArn"),
                "KeyMaterial": {"As2805KeyCryptogram": {
                    "WrappingKeyIdentifier": wrapping,
                    "As2805KeyVariant": variant,
                }},
            }),
        )
        .await;
        let wrapped = exported
            .get("WrappedKey")
            .cloned()
            .unwrap_or_else(|| panic!("case {variant}: no WrappedKey in {exported}"));

        let body = json!({
            "KeyMaterial": {"As2805KeyCryptogram": {
                "As2805KeyVariant": variant,
                "KeyAlgorithm": key_algorithm,
                "KeyModesOfUse": {"Encrypt": true, "Decrypt": true},
                "Exportable": true,
                "WrappingKeyIdentifier": wrapping,
                "WrappedKeyCryptogram": string(&wrapped, "KeyMaterial"),
            }},
        });
        let answered = ok_call(&router, "ImportKey", &body).await;
        let imported = answered
            .get("Key")
            .cloned()
            .unwrap_or_else(|| panic!("case {variant}: no Key in {answered}"));

        // The cryptogram carried the key, so the key that comes out has the material
        // that went in: both ends report the check value of those same bytes.
        assert_eq!(
            imported.get("KeyCheckValue"),
            wrapped.get("KeyCheckValue"),
            "case {variant}: expected the check value of the wrapped key {:?}, got {imported}",
            wrapped.get("KeyCheckValue")
        );
        assert_eq!(
            imported["KeyAttributes"].get("KeyAlgorithm"),
            Some(&Value::String(key_algorithm.to_string())),
            "case {variant}: expected the declared algorithm, got {imported}"
        );
        assert_eq!(
            string(&imported, "KeyOrigin"),
            ORIGIN_IMPORTED,
            "case {variant}: expected the origin {ORIGIN_IMPORTED}, got {imported}"
        );
        // The variant a cryptogram is formed under is what says what the key is for, so
        // the imported key's usage is the one that variant names.
        assert_eq!(
            string(&imported["KeyAttributes"], "KeyUsage"),
            usage_of(variant),
            "case {variant}: expected the key usage that variant names, got {imported}"
        );

        // A cryptogram that does not yield a key of the declared algorithm's length is
        // refused rather than stored under a declaration it does not support. No key
        // algorithm above carries the 32 bytes an AES-256 key does.
        let mut mismatched = body.clone();
        mismatched["KeyMaterial"]["As2805KeyCryptogram"]["KeyAlgorithm"] = json!("AES_256");
        let (status, parsed) = call(&router, "ImportKey", &mismatched).await;
        assert_eq!(
            status, 400,
            "case {variant}: expected 400 for a declared algorithm the cryptogram does not \
             carry, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {variant}: expected ValidationException, got {parsed}"
        );
    }
}

#[tokio::test]
async fn import_key_refuses_a_cryptogram_outside_the_published_length_bounds() {
    let router = router();
    let wrapping = create_kek(&router, "TDES_3KEY").await;
    // The published bounds of `WrappedKeyCryptogram`
    // (`smithy_query(query="closure",operation="ImportKey")` gives length 16..4096),
    // each bound and one character past it. A cryptogram past a bound is refused before
    // any of it is read, so nothing is decoded or decrypted for it; one of exactly a
    // bound's length reaches the handler, which judges its length against the key the
    // request declares before decoding it.
    let cases: [(&str, usize); 4] = [
        ("one character below the minimum", 15),
        ("exactly the minimum", 16),
        ("exactly the maximum", 4096),
        ("one character above the maximum", 4097),
    ];

    for (case, len) in cases {
        let body = json!({
            "KeyMaterial": {"As2805KeyCryptogram": {
                "As2805KeyVariant": "DATA_ENCRYPTION_KEY_VARIANT_22",
                "KeyAlgorithm": "TDES_2KEY",
                "KeyModesOfUse": {"Encrypt": true, "Decrypt": true},
                "Exportable": true,
                "WrappingKeyIdentifier": wrapping,
                // Every character satisfies the modelled `^[0-9A-F]+$`, so the length is
                // the only thing about the cryptogram under test.
                "WrappedKeyCryptogram": "0".repeat(len),
            }},
        });
        let (status, parsed) = call(&router, "ImportKey", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: expected ValidationException, got {parsed}"
        );
    }
}
