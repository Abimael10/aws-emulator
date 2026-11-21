// SPDX-License-Identifier: CC0-1.0

//! The one reading of a data-encryption attribute set, for every operation that takes
//! one: `EncryptData`, `DecryptData` and both legs of `ReEncryptData`.
//!
//! The modelled unions those operations take — `EncryptionDecryptionAttributes`
//! (`Symmetric | Asymmetric | Dukpt | Emv`) and `ReEncryptionAttributes` (`Symmetric |
//! Asymmetric | Dukpt`) — each name WHICH KEY the data is transformed under, and that is
//! the only thing that differs between their members:
//!
//! * `Symmetric` runs the block cipher of the key the request named, under that key's
//!   own working material, in the mode the attributes name;
//! * `Dukpt` and `Emv` run it under a key DERIVED from the key the request named — the
//!   ANSI X9.24 chain from a base derivation key and a key serial number
//!   ([`crypto::dukpt`]), or the EMV master-and-session chain from an issuer master key
//!   and the card ([`crypto::emv`]) — which is why those two need the named key's
//!   `DeriveKey` mode of use rather than `Encrypt` or `Decrypt`;
//! * `Asymmetric` runs RSA under the key pair the named key IS, sealing to its public
//!   half and opening with its private half, which is a computation the key's own store
//!   performs ([`key_store::StoredKey::rsa_encrypt`]) because that private half never
//!   leaves it.
//!
//! So every member the model admits is served here, and what a request outside a
//! scheme's own constraints gets is the modelled refusal that names what it broke —
//! never a 501, which would say nobody had written the member yet.
//!
//! The answer is the cipher's output and nothing else, so a `DecryptData` under the same
//! key and the same attributes returns the plaintext bit for bit, and so does a
//! `ReEncryptData` that re-encrypts under one key what it decrypted under another.

use ::service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::state as key_store;

use super::{
    crypto, missing_mode, resolve_key, KeyRefusal, ResolvedKey, MODE_DECRYPT, MODE_DERIVE_KEY,
    MODE_ENCRYPT,
};
use crate::ops::types::{
    AsymmetricEncryptionAttributes, DukptEncryptionAttributes, EmvEncryptionAttributes,
    EncryptionDecryptionAttributes, ReEncryptionAttributes, SymmetricEncryptionAttributes,
};

/// Which way one data-encryption computation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Plaintext to ciphertext.
    Encrypt,
    /// Ciphertext to plaintext.
    Decrypt,
}

/// The attribute set one leg of a data-encryption operation names.
///
/// Both modelled unions are read into this one set, because what each member says is the
/// same thing on either: the two unions differ only in that `ReEncryptionAttributes`
/// carries no EMV member.
#[derive(Debug, Clone, Copy)]
pub enum Attributes<'a> {
    Symmetric(&'a SymmetricEncryptionAttributes),
    Asymmetric(&'a AsymmetricEncryptionAttributes),
    Dukpt(&'a DukptEncryptionAttributes),
    Emv(&'a EmvEncryptionAttributes),
}

impl<'a> From<&'a EncryptionDecryptionAttributes> for Attributes<'a> {
    fn from(attributes: &'a EncryptionDecryptionAttributes) -> Self {
        match attributes {
            EncryptionDecryptionAttributes::Symmetric(set) => Self::Symmetric(set),
            EncryptionDecryptionAttributes::Asymmetric(set) => Self::Asymmetric(set),
            EncryptionDecryptionAttributes::Dukpt(set) => Self::Dukpt(set),
            EncryptionDecryptionAttributes::Emv(set) => Self::Emv(set),
        }
    }
}

impl<'a> From<&'a ReEncryptionAttributes> for Attributes<'a> {
    fn from(attributes: &'a ReEncryptionAttributes) -> Self {
        match attributes {
            ReEncryptionAttributes::Symmetric(set) => Self::Symmetric(set),
            ReEncryptionAttributes::Asymmetric(set) => Self::Asymmetric(set),
            ReEncryptionAttributes::Dukpt(set) => Self::Dukpt(set),
        }
    }
}

/// What one leg answered: the key it ran under, as that key reports itself, and the data
/// the cipher produced.
#[derive(Debug)]
pub struct Transformed {
    /// The canonical `KeyArn` the control plane holds the key under — what the operation
    /// answers with, whatever identifier the request named and whatever key the
    /// attributes DERIVED from it.
    pub key_arn: String,
    /// The key check value the control plane computed from that key's own secret.
    pub key_check_value: String,
    /// The cipher's output.
    pub data: Vec<u8>,
}

/// Transform `data` under the key `identifier` names, as `attributes` names it.
///
/// The refusals are [`KeyRefusal`]'s, which every operation here already lifts into its
/// own modelled error channel: a `ResourceNotFoundException` for an identifier naming no
/// key, a `ValidationException` for a request this key cannot serve as asked, and an
/// `InternalServerException` for a store that is not reachable.
pub fn transform<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identifier: &str,
    attributes: Attributes<'_>,
    direction: Direction,
    data: &[u8],
) -> Result<Transformed, KeyRefusal> {
    if let Attributes::Asymmetric(set) = attributes {
        return asymmetric(engine, identifier, set, direction, data);
    }
    let key = resolve_key(engine, identifier)?;
    let mode = required_mode(attributes, direction);
    if !holds(&key, mode) {
        return Err(KeyRefusal::NotUsable(missing_mode(&key.key_arn, mode)));
    }
    let computation = computation(&key, attributes)?;
    let request = crypto::SymmetricRequest {
        key_algorithm: &computation.key_algorithm,
        material: &computation.material,
        mode: &computation.mode,
        initialization_vector: computation.initialization_vector.as_deref(),
        data,
    };
    let output = match direction {
        Direction::Encrypt => crypto::symmetric_encrypt(&request),
        Direction::Decrypt => crypto::symmetric_decrypt(&request),
    }
    .map_err(KeyRefusal::NotUsable)?;
    Ok(Transformed {
        key_arn: key.key_arn,
        key_check_value: key.key_check_value,
        data: output,
    })
}

/// The mode of use the key a request names has to carry for that request.
///
/// A key whose material a scheme DERIVES from is used to derive a key, whichever
/// direction the data then moves in; a key the cipher runs directly under is used to
/// encrypt or to decrypt.
fn required_mode(attributes: Attributes<'_>, direction: Direction) -> &'static str {
    match attributes {
        Attributes::Dukpt(_) | Attributes::Emv(_) => MODE_DERIVE_KEY,
        Attributes::Symmetric(_) | Attributes::Asymmetric(_) => match direction {
            Direction::Encrypt => MODE_ENCRYPT,
            Direction::Decrypt => MODE_DECRYPT,
        },
    }
}

/// Whether `key` carries the mode of use `mode` names.
fn holds(key: &ResolvedKey, mode: &str) -> bool {
    match mode {
        MODE_ENCRYPT => key.modes.encrypt(),
        MODE_DECRYPT => key.modes.decrypt(),
        _ => key.modes.derive_key(),
    }
}

/// Whether the modes of use `key` carries include the mode `mode` names, read off the
/// control plane's own record of a key — the same reading [`holds`] makes of a resolved
/// one.
fn stored_key_holds(key: &key_store::StoredKey, mode: &str) -> bool {
    let modes = &key.key_attributes.key_modes_of_use;
    if modes.no_restrictions {
        return true;
    }
    match mode {
        MODE_ENCRYPT => modes.encrypt,
        MODE_DECRYPT => modes.decrypt,
        _ => modes.derive_key,
    }
}

/// The key one attribute set computes under, with the mode and initialization vector it
/// names.
#[derive(Debug)]
struct Computation {
    /// The modelled `KeyAlgorithm` of the key the cipher runs under, which is the named
    /// key's own for a symmetric request and the DERIVED key's for the others.
    key_algorithm: String,
    /// That key's bytes.
    material: Vec<u8>,
    /// The block cipher mode the data is transformed in.
    mode: String,
    /// The initialization vector, already decoded; `None` defaults it to zero, as every
    /// `InitializationVector` member of this model says the service does.
    initialization_vector: Option<Vec<u8>>,
}

/// The block cipher mode applied when a derived-key attribute set names none: "The block
/// cipher method to use for encryption. The default is CBC"
/// (`DukptEncryptionAttributes$Mode`), which is also the mode the EMV attribute set's own
/// catalog (`EmvEncryptionMode`: ECB, CBC) chains in.
const DEFAULT_MODE: &str = "CBC";

/// The key, mode and initialization vector `attributes` names under the resolved key
/// `key`, or the reason it names none.
fn computation(
    key: &ResolvedKey,
    attributes: Attributes<'_>,
) -> Result<Computation, KeyRefusal> {
    match attributes {
        Attributes::Symmetric(set) => Ok(Computation {
            key_algorithm: key.key_algorithm.clone(),
            material: key.material().to_vec(),
            mode: set.mode.clone(),
            initialization_vector: initialization_vector(set.initialization_vector.as_deref())?,
        }),
        Attributes::Dukpt(set) => {
            let derived = crypto::dukpt::data_encryption_key(&crypto::dukpt::Derivation {
                key_algorithm: &key.key_algorithm,
                material: key.material(),
                key_serial_number: &set.key_serial_number,
                derivation_type: set.dukpt_key_derivation_type.as_deref(),
                key_variant: set.dukpt_key_variant.as_deref(),
            })
            .map_err(|refusal| match refusal {
                crypto::dukpt::DerivationRefusal::Invalid(message) => {
                    KeyRefusal::NotUsable(message)
                }
                crypto::dukpt::DerivationRefusal::Unusable(message) => {
                    KeyRefusal::Unavailable(message)
                }
            })?;
            Ok(Computation {
                key_algorithm: derived.key_algorithm.to_string(),
                material: derived.material,
                mode: set.mode.clone().unwrap_or_else(|| DEFAULT_MODE.to_string()),
                initialization_vector: initialization_vector(set.initialization_vector.as_deref())?,
            })
        }
        Attributes::Emv(set) => {
            let derived = crypto::emv::encryption_session_key(
                &set.major_key_derivation_mode,
                &key.key_algorithm,
                key.material(),
                &set.primary_account_number,
                &set.pan_sequence_number,
                &set.session_derivation_data,
            )
            .map_err(|refusal| match refusal {
                crypto::emv::CryptogramRefusal::Invalid(message) => KeyRefusal::NotUsable(message),
                crypto::emv::CryptogramRefusal::Unusable(message) => {
                    KeyRefusal::Unavailable(message)
                }
            })?;
            Ok(Computation {
                key_algorithm: EMV_SESSION_KEY_ALGORITHM.to_string(),
                material: derived,
                mode: set.mode.clone().unwrap_or_else(|| DEFAULT_MODE.to_string()),
                initialization_vector: initialization_vector(set.initialization_vector.as_deref())?,
            })
        }
        // Answered by `asymmetric` above, which runs no block cipher at all.
        Attributes::Asymmetric(_) => Err(KeyRefusal::NotUsable(
            "AsymmetricEncryptionAttributes names no block cipher computation.".to_string(),
        )),
    }
}

/// The `KeyAlgorithm` an EMV session key is keyed as: the double-length Triple DES key
/// the EMV derivations produce.
const EMV_SESSION_KEY_ALGORITHM: &str = "TDES_2KEY";

/// The initialization vector a request carries, decoded, or the refusal of one that is
/// not hexadecimal.
fn initialization_vector(value: Option<&str>) -> Result<Option<Vec<u8>>, KeyRefusal> {
    match value {
        None => Ok(None),
        Some(value) => match hex::decode(value) {
            Ok(decoded) => Ok(Some(decoded)),
            Err(_) => Err(KeyRefusal::NotUsable(
                "InitializationVector must be hexadecimal.".to_string(),
            )),
        },
    }
}

/// One asymmetric leg: RSA under the key pair the named key IS.
///
/// The computation is the key store's own ([`key_store::StoredKey::rsa_encrypt`] and its
/// decrypting half), because the private half of a key pair never leaves that store;
/// what is decided here is the modelled padding the request named, the mode of use the
/// key has to carry, and how each refusal is worded.
fn asymmetric<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identifier: &str,
    set: &AsymmetricEncryptionAttributes,
    direction: Direction,
    data: &[u8],
) -> Result<Transformed, KeyRefusal> {
    // "You must specify a PaddingType" (`ReEncryptionAttributes$Asymmetric`): the
    // scheme is part of the ciphertext, so there is no padding to default to.
    let Some(padding_type) = set.padding_type.as_deref() else {
        return Err(KeyRefusal::NotUsable(
            "PaddingType must be provided for asymmetric encryption.".to_string(),
        ));
    };
    let Some(scheme) = rsa_scheme(padding_type) else {
        return Err(KeyRefusal::NotUsable(format!(
            "PaddingType {padding_type} is not valid for asymmetric encryption."
        )));
    };
    let mode = required_mode(Attributes::Asymmetric(set), direction);
    super::with_key(engine, identifier, |key| {
        // Whether an asymmetric computation is defined for this key AT ALL is a fact
        // about its own algorithm, so it is asked before the key's modes of use — the
        // precedence every operation of this service follows.
        key.rsa_longest_plaintext(scheme)
            .map_err(|refusal| rsa_refused(key, padding_type, refusal))?;
        if !stored_key_holds(key, mode) {
            return Err(KeyRefusal::NotUsable(missing_mode(&key.key_arn, mode)));
        }
        let answered = match direction {
            Direction::Encrypt => key.rsa_encrypt(scheme, data),
            Direction::Decrypt => key.rsa_decrypt(scheme, data),
        };
        let data = answered.map_err(|refusal| rsa_refused(key, padding_type, refusal))?;
        Ok(Transformed {
            key_arn: key.key_arn.clone(),
            key_check_value: key.key_check_value.clone(),
            data,
        })
    })
}

/// The RSA scheme a modelled `PaddingType` names, or `None` when it names none.
///
/// The names are the modelled values
/// (`smithy_query(query="shape",shape="PaddingType")`: PKCS1, OAEP_SHA1, OAEP_SHA256,
/// OAEP_SHA512), matched exactly, so a value the model gains later is refused rather
/// than padded by a scheme that was not asked for.
fn rsa_scheme(padding_type: &str) -> Option<key_store::RsaEncryptionScheme> {
    let scheme = match padding_type {
        "PKCS1" => key_store::RsaEncryptionScheme::Pkcs1,
        "OAEP_SHA1" => key_store::RsaEncryptionScheme::OaepSha1,
        "OAEP_SHA256" => key_store::RsaEncryptionScheme::OaepSha256,
        "OAEP_SHA512" => key_store::RsaEncryptionScheme::OaepSha512,
        _ => return None,
    };
    Some(scheme)
}

/// How this service states what the key store refused an RSA computation with.
fn rsa_refused(
    key: &key_store::StoredKey,
    padding_type: &str,
    refusal: key_store::RsaRefusal,
) -> KeyRefusal {
    match refusal {
        key_store::RsaRefusal::NotRsaKeyPair => KeyRefusal::NotUsable(format!(
            "KeyAlgorithm {} is not valid for asymmetric encryption.",
            key.key_attributes.key_algorithm
        )),
        // The ceiling is the scheme's own over this key's modulus, stated in the unit the
        // caller sent its data in: every data member of this service is hexadecimal, so
        // one byte of data is two characters of it.
        key_store::RsaRefusal::TooLong(longest) => KeyRefusal::NotUsable(format!(
            "Invalid data provided. Please provide data of length at most {} for \
             KeyAlgorithm {} with PaddingType {padding_type}.",
            longest * crypto::HEX_CHARACTERS_PER_BYTE,
            key.key_attributes.key_algorithm
        )),
        key_store::RsaRefusal::NotThisCiphertext => KeyRefusal::NotUsable(format!(
            "The data provided is not a ciphertext of key {} with PaddingType \
             {padding_type}.",
            key.key_arn
        )),
        key_store::RsaRefusal::Failed => KeyRefusal::Unavailable(format!(
            "The asymmetric computation under key {} could not be performed.",
            key.key_arn
        )),
    }
}
