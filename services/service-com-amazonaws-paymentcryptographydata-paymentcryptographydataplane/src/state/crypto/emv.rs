// SPDX-License-Identifier: CC0-1.0

//! The EMV application-cryptogram computations: the key derivations an EMV chip card
//! and its issuer share, and the cryptograms they exchange.
//!
//! An EMV authorization cryptogram is never computed under the issuer master key
//! itself. The published chain is three steps, and each one is written out here over
//! the primitives of the parent module (the `des` cipher, the ISO 9797-1 CBC-MAC
//! chain and the decimalisation the card data computation already uses):
//!
//! 1. **the card's own master key** — the issuer master key diversified by the card's
//!    account number and sequence number, by the option the request names: EMV Option
//!    A takes the trailing sixteen digits of the account number and sequence number as
//!    the diversification value, Option B hashes them with SHA-1 and decimalises the
//!    hash to sixteen digits, and both then encrypt that value, and that value with
//!    every bit inverted, to make the two halves of a double-length key;
//! 2. **the session key** — the card's master key diversified by the transaction's own
//!    data: the common session key derivation, which encrypts the diversification
//!    value with one marker byte set two ways, over the transaction counter alone and,
//!    for the scheme whose attributes carry one, the issuer's unpredictable number;
//!    the schemes whose modelled attributes carry no transaction data at all compute
//!    under the card's master key itself;
//! 3. **the cryptogram** — the EMV MAC (ISO 9797-1 algorithm 3 over data padded by
//!    that standard's padding method 2) of the transaction data under the session key
//!    for the request cryptogram, and either the response code mixed into the request
//!    cryptogram and encrypted, or the leading four bytes of the EMV MAC over the
//!    request cryptogram and the card status update, for the response cryptogram.
//!
//! Every step is a function of the key's working material and the request alone, so a
//! cryptogram is reproducible: that is what lets `VerifyAuthRequestCryptogram`
//! recompute what `GenerateAuthRequestCryptogram` answered and compare the two, and no
//! cryptogram is stored anywhere.

use sha1::{Digest, Sha1};

use super::{cbc_mac, decimalised, single_des, BlockCipher};
use crate::ops::types::{CryptogramAuthResponse, SessionKeyDerivation};

/// Why an EMV cryptogram could not be computed.
#[derive(Debug, Clone)]
pub enum CryptogramRefusal {
    /// The request is inadmissible; the message says why. The modelled
    /// `ValidationException`.
    Invalid(String),
    /// The key's working material cannot key a derivation its algorithm admits — an
    /// invariant of this emulator's own derivation rather than anything the caller
    /// asked for, so it is a server fault and not a refusal of the request.
    Unusable(String),
}

/// The modelled `MajorKeyDerivationMode` value that takes the account number's own
/// digits as the diversification value.
const EMV_OPTION_A: &str = "EMV_OPTION_A";

/// The modelled `MajorKeyDerivationMode` value that hashes them first.
const EMV_OPTION_B: &str = "EMV_OPTION_B";

/// How many digits a master key derivation's diversification value carries: eight
/// bytes of two digits each.
const DIVERSIFICATION_DIGITS: usize = 16;

/// How many bytes each derivation here produces: a double-length Triple DES key, the
/// key both the card master key and the session key are.
const DERIVED_KEY_BYTES: usize = 16;

/// The `KeyAlgorithm` a derived double-length Triple DES key is keyed as.
const DERIVED_KEY_ALGORITHM: &str = "TDES_2KEY";

/// The byte of the diversification value the common session key derivation overwrites,
/// which is what makes the two halves of the session key differ.
const SESSION_KEY_MARKER: usize = 2;

/// The marker the left half of a common session key is derived under.
const SESSION_KEY_MARKER_LEFT: u8 = 0xF0;

/// The marker the right half is derived under.
const SESSION_KEY_MARKER_RIGHT: u8 = 0x0F;

/// How many bytes of the EMV MAC the response cryptogram of method 2 carries.
const ARPC_METHOD_2_BYTES: usize = 4;

/// How many bytes a cryptogram carries: the whole EMV MAC, which is one derivation
/// block.
const CRYPTOGRAM_BYTES: usize = DERIVATION_BLOCK_BYTES;

/// How many bytes one block of every derivation here is: the Triple DES block, which is
/// the length of a diversification value, of the halves each derivation encrypts and of
/// the MAC it chains.
const DERIVATION_BLOCK_BYTES: usize = 8;

/// How many bytes the modelled `AuthResponseCode` carries (`HexLengthEquals4`).
const AUTH_RESPONSE_CODE_BYTES: usize = 2;

/// How many bytes the modelled `CardStatusUpdate` carries (`HexLengthEquals8`).
const CARD_STATUS_UPDATE_BYTES: usize = 4;

/// How many bytes the modelled `ApplicationTransactionCounter` carries
/// (`HexLengthEquals4`).
const TRANSACTION_COUNTER_BYTES: usize = 2;

/// How many bytes the modelled `UnpredictableNumber` carries (`HexLengthEquals8`).
const UNPREDICTABLE_NUMBER_BYTES: usize = 4;

/// The `KeyAlgorithm` values an EMV issuer master key derivation runs under: the
/// derivation encrypts under the master key's own Triple DES cipher and produces a
/// double-length Triple DES key, so an algorithm that names no Triple DES cipher names
/// no such derivation. Matched exactly against the control plane's modelled
/// `SymmetricKeyAlgorithm`, so an algorithm the model gains later is refused rather
/// than run under a derivation it has no definition for.
const ISSUER_MASTER_KEY_ALGORITHMS: &[&str] = &["TDES_2KEY", "TDES_3KEY"];

/// The session key an EMV application cryptogram is computed under: the issuer master
/// key `material` diversified for the card, then for the transaction.
///
/// `major_key_derivation_mode` is the modelled `MajorKeyDerivationMode` the request
/// names, `key_algorithm` the resolved key's own `KeyAlgorithm`, and `attributes` the
/// request's `SessionKeyDerivationAttributes`.
pub fn session_key(
    major_key_derivation_mode: &str,
    key_algorithm: &str,
    material: &[u8],
    attributes: &SessionKeyDerivation,
) -> Result<Vec<u8>, CryptogramRefusal> {
    let card = diversification(attributes);
    let master = card_master_key(
        major_key_derivation_mode,
        key_algorithm,
        material,
        card.primary_account_number,
        card.pan_sequence_number,
    )?;
    // A scheme whose modelled attributes carry no transaction counter carries nothing
    // to diversify the card's key by, so the cryptogram is computed under that key
    // itself.
    let Some(counter) = card.application_transaction_counter else {
        return Ok(master);
    };
    let value = diversification_value(counter, card.unpredictable_number)?;
    let cipher = derived_cipher(&master)?;
    let halves = if card.emv2000 {
        // The EMV2000 session key derivation applies the same function the master key
        // derivation uses to the transaction's diversification value.
        inverted_pair(&cipher, &value)
    } else {
        common_session_key(&cipher, &value)
    };
    Ok(halves)
}

/// The session key an EMV DATA ENCRYPTION runs under: the issuer master key `material`
/// diversified for the card, then for the session by the derivation data the request
/// carries.
///
/// The same two-step chain [`session_key`] walks for a cryptogram, with the session step
/// diversified by `session_derivation_data` — "the derivation value used to derive the
/// ICC session key ... typically the application transaction counter value padded with
/// zeros or previous ARQC value padded with zeros as per EMV version 4.3"
/// (`EmvEncryptionAttributes$SessionDerivationData`), which is one derivation block —
/// rather than by a transaction counter the cryptogram schemes carry. The result is the
/// double-length Triple DES key the data is then encrypted under.
pub fn encryption_session_key(
    major_key_derivation_mode: &str,
    key_algorithm: &str,
    material: &[u8],
    primary_account_number: &str,
    pan_sequence_number: &str,
    session_derivation_data: &str,
) -> Result<Vec<u8>, CryptogramRefusal> {
    let master = card_master_key(
        major_key_derivation_mode,
        key_algorithm,
        material,
        primary_account_number,
        pan_sequence_number,
    )?;
    let value = decoded_exact(
        "SessionDerivationData",
        session_derivation_data,
        DERIVATION_BLOCK_BYTES,
    )?;
    let cipher = derived_cipher(&master)?;
    Ok(common_session_key(&cipher, &value))
}

/// The Authorization Request Cryptogram of `transaction_data` under `session_key`: the
/// EMV MAC, eight bytes, which is what the modelled `AuthRequestCryptogramType`
/// carries as sixteen hexadecimal characters.
pub fn auth_request_cryptogram(
    session_key: &[u8],
    transaction_data: &[u8],
) -> Result<Vec<u8>, CryptogramRefusal> {
    emv_mac(session_key, transaction_data)
}

/// The Authorization Response Cryptogram for a verified request cryptogram, by the
/// method the request's `AuthResponseAttributes` names.
///
/// * method 1 mixes the authorization response code into the request cryptogram and
///   encrypts the result under the session key, which is eight bytes;
/// * method 2 is the leading four bytes of the EMV MAC over the request cryptogram,
///   the card status update and any proprietary authentication data.
pub fn auth_response_cryptogram(
    session_key: &[u8],
    auth_request_cryptogram: &[u8],
    response: &CryptogramAuthResponse,
) -> Result<Vec<u8>, CryptogramRefusal> {
    match response {
        CryptogramAuthResponse::ArpcMethod1(attributes) => {
            let code = decoded_exact(
                "AuthResponseCode",
                &attributes.auth_response_code,
                AUTH_RESPONSE_CODE_BYTES,
            )?;
            let cipher = derived_cipher(session_key)?;
            if auth_request_cryptogram.len() != CRYPTOGRAM_BYTES {
                return Err(unusable(DERIVED_KEY_ALGORITHM));
            }
            let mut block = auth_request_cryptogram.to_vec();
            for (byte, code) in block.iter_mut().zip(code.iter()) {
                *byte ^= code;
            }
            cipher.encrypt_block(&mut block);
            Ok(block)
        }
        CryptogramAuthResponse::ArpcMethod2(attributes) => {
            let mut data = auth_request_cryptogram.to_vec();
            data.extend(decoded_exact(
                "CardStatusUpdate",
                &attributes.card_status_update,
                CARD_STATUS_UPDATE_BYTES,
            )?);
            if let Some(proprietary) = &attributes.proprietary_authentication_data {
                data.extend(decoded("ProprietaryAuthenticationData", proprietary)?);
            }
            let mut mac = emv_mac(session_key, &data)?;
            mac.truncate(ARPC_METHOD_2_BYTES);
            Ok(mac)
        }
    }
}

/// What a session key is derived from, as the modelled `SessionKeyDerivation` union
/// carries it.
#[derive(Clone, Copy)]
struct Diversification<'a> {
    primary_account_number: &'a str,
    pan_sequence_number: &'a str,
    /// The `ApplicationTransactionCounter`, for the schemes whose modelled member
    /// carries one.
    application_transaction_counter: Option<&'a str>,
    /// The `UnpredictableNumber`, for the one scheme whose member carries one.
    unpredictable_number: Option<&'a str>,
    /// Whether the scheme derives its session key by the EMV2000 function rather than
    /// by the common session key one.
    emv2000: bool,
}

impl ::std::fmt::Debug for Diversification<'_> {
    /// Names which derivation the request asked for and never the card: the account
    /// number the scheme carries is marked sensitive by the model, so no formatter of
    /// this value can put it in a log or a test failure.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("Diversification")
            .field("card", &format_args!("<redacted>"))
            .field(
                "diversified_by_transaction",
                &self.application_transaction_counter.is_some(),
            )
            .field(
                "diversified_by_unpredictable_number",
                &self.unpredictable_number.is_some(),
            )
            .field("emv2000", &self.emv2000)
            .finish()
    }
}

/// Read the diversification data off the union member the request named.
///
/// Which members carry what is the model's own statement
/// (`smithy_query(query="shape",shape="SessionKeyDerivation")`): the EMV common,
/// Mastercard, EMV2000 and UnionPay members each carry a transaction counter,
/// Mastercard's carries the issuer's unpredictable number as well, and the American
/// Express and Visa members carry the card alone.
fn diversification(attributes: &SessionKeyDerivation) -> Diversification<'_> {
    match attributes {
        SessionKeyDerivation::EmvCommon(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: Some(&member.application_transaction_counter),
            unpredictable_number: None,
            emv2000: false,
        },
        SessionKeyDerivation::Mastercard(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: Some(&member.application_transaction_counter),
            unpredictable_number: Some(&member.unpredictable_number),
            emv2000: false,
        },
        SessionKeyDerivation::Emv2000(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: Some(&member.application_transaction_counter),
            unpredictable_number: None,
            emv2000: true,
        },
        SessionKeyDerivation::UnionPay(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: Some(&member.application_transaction_counter),
            unpredictable_number: None,
            emv2000: false,
        },
        SessionKeyDerivation::Amex(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: None,
            unpredictable_number: None,
            emv2000: false,
        },
        SessionKeyDerivation::Visa(member) => Diversification {
            primary_account_number: &member.primary_account_number,
            pan_sequence_number: &member.pan_sequence_number,
            application_transaction_counter: None,
            unpredictable_number: None,
            emv2000: false,
        },
    }
}

/// The card's own master key: the issuer master key `material` diversified by the
/// card's account number and sequence number, by the option `mode` names.
fn card_master_key(
    mode: &str,
    key_algorithm: &str,
    material: &[u8],
    primary_account_number: &str,
    pan_sequence_number: &str,
) -> Result<Vec<u8>, CryptogramRefusal> {
    if !ISSUER_MASTER_KEY_ALGORITHMS.contains(&key_algorithm) {
        return Err(CryptogramRefusal::Invalid(format!(
            "KeyAlgorithm {key_algorithm} is not valid for this operation."
        )));
    }
    let digits = diversification_digits(mode, primary_account_number, pan_sequence_number)?;
    let value = hex::decode(&digits).map_err(|_| not_decimal())?;
    let Some(cipher) = BlockCipher::new(key_algorithm, material) else {
        return Err(unusable(key_algorithm));
    };
    Ok(inverted_pair(&cipher, &value))
}

/// The sixteen digits the card's master key is diversified by.
///
/// Option A takes the trailing sixteen digits of the account number followed by the
/// sequence number, padded on the left with zeros when there are fewer; Option B hashes
/// those digits with SHA-1 — padded on the right with a zero digit to a whole number of
/// bytes first — and decimalises the hash to sixteen digits.
///
/// Both read DECIMAL digits: the modelled `PrimaryAccountNumber` and
/// `PanSequenceNumber` are each `@pattern ^[0-9]+$`, and that is checked here rather
/// than assumed, so a value carrying anything else is refused naming the two members
/// instead of being sliced or coded as if it were digits.
fn diversification_digits(
    mode: &str,
    primary_account_number: &str,
    pan_sequence_number: &str,
) -> Result<String, CryptogramRefusal> {
    let digits = format!("{primary_account_number}{pan_sequence_number}");
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(not_decimal());
    }
    match mode {
        EMV_OPTION_A => {
            // Decimal digits are one ASCII byte each, which the check above establishes,
            // so the trailing sixteen of them are the trailing sixteen bytes.
            let tail = &digits[digits.len().saturating_sub(DIVERSIFICATION_DIGITS)..];
            Ok(format!("{tail:0>width$}", width = DIVERSIFICATION_DIGITS))
        }
        EMV_OPTION_B => {
            let mut padded = digits;
            if !padded.len().is_multiple_of(2) {
                padded.push('0');
            }
            let value = hex::decode(&padded).map_err(|_| not_decimal())?;
            let hashed = hex::encode_upper(Sha1::digest(&value));
            Ok(decimalised(&hashed, DIVERSIFICATION_DIGITS))
        }
        other => Err(CryptogramRefusal::Invalid(format!(
            "MajorKeyDerivationMode {other} is not valid for this operation."
        ))),
    }
}

/// The refusal of a card whose account number or sequence number is not decimal digits.
fn not_decimal() -> CryptogramRefusal {
    CryptogramRefusal::Invalid(
        "PrimaryAccountNumber and PanSequenceNumber must be decimal digits.".to_string(),
    )
}

/// The value a session key is diversified by, one derivation block long: the
/// transaction counter, then the marker byte the derivation overwrites and a zero byte,
/// then the unpredictable number of the one scheme that carries one.
fn diversification_value(
    application_transaction_counter: &str,
    unpredictable_number: Option<&str>,
) -> Result<Vec<u8>, CryptogramRefusal> {
    let counter = decoded_exact(
        "ApplicationTransactionCounter",
        application_transaction_counter,
        TRANSACTION_COUNTER_BYTES,
    )?;
    let mut value = vec![0u8; DERIVATION_BLOCK_BYTES];
    value[..counter.len()].copy_from_slice(&counter);
    if let Some(number) = unpredictable_number {
        let number =
            decoded_exact("UnpredictableNumber", number, UNPREDICTABLE_NUMBER_BYTES)?;
        let start = value.len() - number.len();
        value[start..].copy_from_slice(&number);
    }
    Ok(value)
}

/// The common session key derivation: the diversification value encrypted with its
/// marker byte set one way for the left half of the key and the other way for the
/// right.
fn common_session_key(cipher: &BlockCipher, value: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(DERIVED_KEY_BYTES);
    for marker in [SESSION_KEY_MARKER_LEFT, SESSION_KEY_MARKER_RIGHT] {
        let mut half = value.to_vec();
        half[SESSION_KEY_MARKER] = marker;
        cipher.encrypt_block(&mut half);
        key.extend(half);
    }
    odd_parity(key)
}

/// The derivation both the master key options and the EMV2000 session key use: the
/// value encrypted for the left half of the key, and the value with every bit inverted
/// encrypted for the right half.
fn inverted_pair(cipher: &BlockCipher, value: &[u8]) -> Vec<u8> {
    let mut key = value.to_vec();
    cipher.encrypt_block(&mut key);
    let mut inverted: Vec<u8> = value.iter().map(|byte| !byte).collect();
    cipher.encrypt_block(&mut inverted);
    key.extend(inverted);
    odd_parity(key)
}

/// Each byte of a derived Triple DES key adjusted to odd parity, as the derivation
/// specifies. The cipher ignores the parity bit, so this changes no computation the key
/// then keys; it is what makes the derived key the one the specification names.
fn odd_parity(mut key: Vec<u8>) -> Vec<u8> {
    for byte in &mut key {
        if byte.count_ones().is_multiple_of(2) {
            *byte ^= 0x01;
        }
    }
    key
}

/// The Triple DES cipher a derived double-length key keys.
fn derived_cipher(material: &[u8]) -> Result<BlockCipher, CryptogramRefusal> {
    if material.len() < DERIVED_KEY_BYTES {
        return Err(unusable(DERIVED_KEY_ALGORITHM));
    }
    BlockCipher::new(DERIVED_KEY_ALGORITHM, &material[..DERIVED_KEY_BYTES])
        .ok_or_else(|| unusable(DERIVED_KEY_ALGORITHM))
}

/// The EMV MAC of `data` under a derived double-length Triple DES key: ISO 9797-1
/// algorithm 3 — the single-DES CBC-MAC under the key's left half, finished by a
/// decrypt under its right half and an encrypt under the left half again — over the
/// data padded by that standard's padding method 2.
fn emv_mac(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptogramRefusal> {
    if key.len() < DERIVED_KEY_BYTES {
        return Err(unusable(DERIVED_KEY_ALGORITHM));
    }
    let (Some(first), Some(second)) = (
        single_des(&key[..DERIVATION_BLOCK_BYTES]),
        single_des(&key[DERIVATION_BLOCK_BYTES..DERIVED_KEY_BYTES]),
    ) else {
        return Err(unusable(DERIVED_KEY_ALGORITHM));
    };
    let mut mac = cbc_mac(&first, &padding_method_2(data, first.block_len()));
    second.decrypt_block(&mut mac);
    first.encrypt_block(&mut mac);
    Ok(mac)
}

/// ISO 9797-1 padding method 2: a single `80` byte, then zero bytes to the next whole
/// block — appended whatever the data's own length, which is what distinguishes this
/// method from padding method 1.
fn padding_method_2(data: &[u8], block_len: usize) -> Vec<u8> {
    let mut padded = data.to_vec();
    padded.push(0x80);
    while !padded.len().is_multiple_of(block_len) {
        padded.push(0);
    }
    padded
}

/// `value`, a hexadecimal member of the request, as bytes.
///
/// Every member decoded here is `@pattern ^[0-9a-fA-F]+$` with an even length bound, so
/// a value that does not decode is refused naming the member rather than computed over.
fn decoded(member: &str, value: &str) -> Result<Vec<u8>, CryptogramRefusal> {
    hex::decode(value).map_err(|_| {
        CryptogramRefusal::Invalid(format!(
            "{member} must be an even number of hexadecimal characters."
        ))
    })
}

/// `value`, a hexadecimal member of the request whose modelled length is EXACTLY
/// `bytes` bytes, as bytes.
///
/// The length is the model's own (`HexLengthEquals4` carries two bytes,
/// `HexLengthEquals8` four), so a value of any other length is refused naming the
/// member and the length it has to carry — never truncated into, or padded out to, the
/// slot the derivation puts it in.
fn decoded_exact(member: &str, value: &str, bytes: usize) -> Result<Vec<u8>, CryptogramRefusal> {
    let decoded = decoded(member, value)?;
    if decoded.len() != bytes {
        return Err(CryptogramRefusal::Invalid(format!(
            "{member} must be {} hexadecimal characters.",
            bytes * 2
        )));
    }
    Ok(decoded)
}

/// The key material of a key the derivation ADMITS turned out not to key it: the length
/// or the form of the working material this service derived, which is this emulator's
/// own invariant rather than the caller's request.
fn unusable(key_algorithm: &str) -> CryptogramRefusal {
    CryptogramRefusal::Unusable(format!(
        "The working material held for this {key_algorithm} key does not key the EMV \
         cryptogram derivation."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card master key to run the cryptogram steps under, derived from fixture issuer
    /// master key material.
    fn master_key() -> Vec<u8> {
        card_master_key(
            EMV_OPTION_A,
            "TDES_2KEY",
            &(0u8..16).collect::<Vec<u8>>(),
            "4111111111111111",
            "01",
        )
        .expect("a Triple DES issuer master key derives a card master key")
    }

    /// RULE: Option A's diversification value is the TRAILING sixteen digits of the
    /// account number and sequence number, padded on the left with zeros when there are
    /// fewer; Option B is the decimalised SHA-1 hash of those digits, which is sixteen
    /// digits for an account number of any length.
    #[test]
    fn the_diversification_value_is_sixteen_digits_of_the_card() {
        // (case, PAN, PSN, the Option A value)
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "a sixteen-digit account number",
                "4111111111111111",
                "01",
                "1111111111111101",
            ),
            (
                "the shortest modelled account number",
                "411111111111",
                "00",
                "0041111111111100",
            ),
            (
                "the longest modelled account number",
                "4111111111111111999",
                "02",
                "1111111111199902",
            ),
        ];
        for (case, pan, psn, expected) in rows {
            let option_a = diversification_digits(EMV_OPTION_A, pan, psn)
                .expect("Option A has a diversification value");
            assert_eq!(
                option_a, *expected,
                "case {case:?}: expected the Option A value {expected}, got {option_a}"
            );
            let option_b = diversification_digits(EMV_OPTION_B, pan, psn)
                .expect("Option B has a diversification value");
            assert!(
                option_b.len() == DIVERSIFICATION_DIGITS
                    && option_b.bytes().all(|byte| byte.is_ascii_digit()),
                "case {case:?}: expected sixteen decimal digits from Option B, got {option_b}"
            );
            assert_ne!(
                option_a, option_b,
                "case {case:?}: Option B hashes the digits Option A takes directly"
            );
        }
    }

    /// RULE: a mode the model does not name derives nothing, and neither does a key
    /// whose algorithm names no Triple DES cipher.
    #[test]
    fn a_derivation_the_specification_does_not_define_is_refused() {
        let material = (0u8..32).collect::<Vec<u8>>();
        let refused = card_master_key(
            "EMV_OPTION_C",
            "TDES_2KEY",
            &material[..16],
            "4111111111111111",
            "01",
        );
        assert!(
            matches!(refused, Err(CryptogramRefusal::Invalid(_))),
            "an unmodelled major key derivation mode must be refused, got {refused:?}"
        );
        for key_algorithm in ["AES_128", "AES_256", "HMAC_SHA256"] {
            let refused = card_master_key(
                EMV_OPTION_A,
                key_algorithm,
                &material,
                "4111111111111111",
                "01",
            );
            assert!(
                matches!(refused, Err(CryptogramRefusal::Invalid(_))),
                "case {key_algorithm}: a key that names no Triple DES cipher must be \
                 refused, got {refused:?}"
            );
        }
    }

    /// RULE: every derived key is a double-length Triple DES key of odd parity in each
    /// byte, and the halves of a session key differ because the marker byte does.
    #[test]
    fn a_derived_key_is_a_double_length_key_of_odd_parity() {
        let master = master_key();
        let cipher = derived_cipher(&master).expect("a derived key keys its cipher");
        let value = diversification_value("0001", Some("1A2B3C4D")).expect("a counter decodes");
        for (case, key) in [
            ("the card master key", master.clone()),
            ("a common session key", common_session_key(&cipher, &value)),
            ("an EMV2000 session key", inverted_pair(&cipher, &value)),
        ] {
            assert_eq!(
                key.len(),
                DERIVED_KEY_BYTES,
                "case {case}: expected {DERIVED_KEY_BYTES} bytes, got {}",
                key.len()
            );
            assert!(
                key.iter().all(|byte| !byte.count_ones().is_multiple_of(2)),
                "case {case}: every byte of a derived key carries odd parity"
            );
            assert_ne!(
                key[..8],
                key[8..],
                "case {case}: the two halves of a derived key differ"
            );
        }
    }

    /// RULE: the diversification value carries the transaction counter in its leading
    /// bytes and the unpredictable number, when the scheme has one, in its trailing
    /// ones, so a scheme that reads an unpredictable number derives a different key
    /// from one that does not.
    #[test]
    fn the_session_key_value_carries_the_transaction_data() {
        let value = diversification_value("006D", None).expect("a counter decodes");
        assert_eq!(
            value,
            vec![0x00, 0x6D, 0, 0, 0, 0, 0, 0],
            "the counter alone occupies the leading bytes, got {value:?}"
        );
        let with_number =
            diversification_value("006D", Some("1A2B3C4D")).expect("a counter decodes");
        assert_eq!(
            with_number,
            vec![0x00, 0x6D, 0, 0, 0x1A, 0x2B, 0x3C, 0x4D],
            "the unpredictable number occupies the trailing bytes, got {with_number:?}"
        );
    }

    /// RULE: the EMV MAC pads by ISO 9797-1 padding method 2, which appends the `80`
    /// byte whatever the data's own length — so data that already fills whole blocks
    /// gains one more.
    #[test]
    fn the_emv_mac_pads_by_padding_method_two() {
        // (case, the data, the padded length in bytes)
        let rows: &[(&str, &[u8], usize)] = &[
            ("one short block", &[1, 2, 3], 8),
            ("one whole block", &[1, 2, 3, 4, 5, 6, 7, 8], 16),
            ("a block and a byte", &[1, 2, 3, 4, 5, 6, 7, 8, 9], 16),
        ];
        for (case, data, expected) in rows {
            let padded = padding_method_2(data, 8);
            assert_eq!(
                padded.len(),
                *expected,
                "case {case}: expected {expected} bytes, got {}",
                padded.len()
            );
            assert_eq!(
                padded[data.len()], 0x80,
                "case {case}: the padding starts with the 80 byte"
            );
            assert!(
                padded[data.len() + 1..].iter().all(|byte| *byte == 0),
                "case {case}: the rest of the padding is zero bytes"
            );
        }
    }

    /// RULE: the card's account number and sequence number are decimal digits, and the
    /// transaction counter and unpredictable number carry exactly the length the model
    /// gives them; a value that does not is refused naming the member, never sliced,
    /// truncated or padded into the derivation.
    #[test]
    fn a_value_the_model_does_not_admit_is_refused_rather_than_coerced() {
        // (case, PAN, PSN) — including a multi-byte character, which a byte-indexed
        // reading of the digits would panic on.
        let cards: &[(&str, &str, &str)] = &[
            ("a letter among the digits", "4111111111111A11", "01"),
            ("a multi-byte character", "41111111111111π11", "01"),
            ("a multi-byte sequence number", "4111111111111111", "0π"),
            ("no digits at all", "", ""),
        ];
        for (case, pan, psn) in cards {
            for mode in [EMV_OPTION_A, EMV_OPTION_B] {
                let refused = diversification_digits(mode, pan, psn);
                assert!(
                    matches!(refused, Err(CryptogramRefusal::Invalid(_))),
                    "case {case:?}/{mode}: expected a refusal, got {refused:?}"
                );
            }
        }
        // (case, the counter, the unpredictable number)
        let transactions: &[(&str, &str, Option<&str>)] = &[
            ("a short counter", "6D", None),
            ("a long counter", "00006D", None),
            ("a short unpredictable number", "006D", Some("1A2B")),
            ("a long unpredictable number", "006D", Some("1A2B3C4D5E")),
        ];
        for (case, counter, number) in transactions {
            let refused = diversification_value(counter, *number);
            assert!(
                matches!(refused, Err(CryptogramRefusal::Invalid(_))),
                "case {case:?}: expected a refusal, got {refused:?}"
            );
        }
    }

    /// RULE: a cryptogram is a function of the session key and the data alone, so the
    /// same inputs reproduce it and a changed input changes it; the request cryptogram
    /// is eight bytes and the response cryptogram of method 2 is four.
    #[test]
    fn a_cryptogram_is_reproducible_and_data_dependent() {
        let master = master_key();
        let data = hex::decode("A0B1C2D3E4F50607").expect("the transaction data decodes");
        let cryptogram =
            auth_request_cryptogram(&master, &data).expect("a derived key computes a cryptogram");
        assert_eq!(
            cryptogram.len(),
            8,
            "the request cryptogram is eight bytes, got {}",
            cryptogram.len()
        );
        assert_eq!(
            auth_request_cryptogram(&master, &data).expect("the computation repeats"),
            cryptogram,
            "the same key and data must reproduce the cryptogram"
        );
        let other = hex::decode("A0B1C2D3E4F50608").expect("the transaction data decodes");
        assert_ne!(
            auth_request_cryptogram(&master, &other).expect("the computation repeats"),
            cryptogram,
            "a change in the transaction data must change the cryptogram"
        );

        let method_1 = auth_response_cryptogram(
            &master,
            &cryptogram,
            &CryptogramAuthResponse::ArpcMethod1(
                crate::ops::types::CryptogramVerificationArpcMethod1 {
                    auth_response_code: "3030".to_string(),
                },
            ),
        )
        .expect("method 1 computes a response cryptogram");
        assert_eq!(
            method_1.len(),
            8,
            "the response cryptogram of method 1 is eight bytes, got {}",
            method_1.len()
        );
        let method_2 = auth_response_cryptogram(
            &master,
            &cryptogram,
            &CryptogramAuthResponse::ArpcMethod2(
                crate::ops::types::CryptogramVerificationArpcMethod2 {
                    card_status_update: "00112233".to_string(),
                    proprietary_authentication_data: None,
                },
            ),
        )
        .expect("method 2 computes a response cryptogram");
        assert_eq!(
            method_2.len(),
            ARPC_METHOD_2_BYTES,
            "the response cryptogram of method 2 is {ARPC_METHOD_2_BYTES} bytes, got {}",
            method_2.len()
        );
        assert_ne!(
            method_1[..ARPC_METHOD_2_BYTES],
            method_2[..],
            "the two methods are different computations"
        );
    }
}
