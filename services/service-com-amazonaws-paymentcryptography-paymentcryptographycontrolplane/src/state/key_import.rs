// SPDX-License-Identifier: CC0-1.0

//! What `ImportKey` asks of this store: the key a caller's wrapped material carries,
//! and the record this service holds for it afterwards.
//!
//! One concern: recovering a key from material wrapped under a key encryption key this
//! store already holds, and the record the recovered key becomes. It is the exact
//! inverse of [`super::key_export`] and shares every precondition with it — the same
//! lookup decides which key may protect a key block or a key cryptogram, in the
//! direction the call needs — so a key this service wrapped for a party comes back as
//! the same key, and material that was altered on the way, or was never wrapped under
//! the key the request names, is refused instead of being stored as a key.
//!
//! The recovered key's own attributes are NOT taken from the request: a key block
//! describes the key it carries, so every attribute of the imported key is read out of
//! the block's header ([`super::key_block`]) and the recovered material's length. A key
//! cryptogram carries no header, so for that mechanism the attributes a caller declares
//! are checked against the material the cryptogram actually yields.
//!
//! Every computation here is bounded by the length the MODEL gives the member it reads,
//! which the centralized request walk has already refused a longer value of before a
//! handler runs — `Tr31WrappedKeyBlock` is 56..9984 characters and
//! `WrappedKeyCryptogram` 16..4096 (`smithy_query(query="closure",operation=
//! "ImportKey")`), so a few kilobytes at most reach a cipher. Inside that, the
//! cryptogram's own length is judged against the declared key's before it is decoded,
//! and a key block's framing is walked once with every index checked.

use std::time::SystemTime;

use crate::ops::types::{
    ImportAs2805KeyCryptogram, ImportTr31KeyBlock, KeyAttributes, Tag, KEY_USAGE_VALUES,
};

use super::block_cipher::tdes_decrypt_blocks;
use super::key_block::{self, ParsedKeyBlock};
use super::key_export::{
    as2805_variant, as2805_varianted_key, tr31_key_usage, KeyProtectionUse,
};
use super::{
    is_symmetric_algorithm, key_usage_class, material_len, tag_map,
    ComAmazonawsPaymentcryptographyPaymentcryptographycontrolplaneState as KeyStore,
    ExportRejection, KeyMaterial, KeyRecord, MintRejection, StoredKey, TagRejection,
    KEY_CLASS_SYMMETRIC, KEY_ORIGIN_EXTERNAL,
};

/// The `KeyAlgorithm` values of a key encryption key an AS 2805 key cryptogram is
/// encrypted under: the standard's key variants are formed from a TDES key, which is
/// the family the export side wraps under too.
const AS2805_WRAPPING_KEY_ALGORITHMS: &[&str] = &["TDES_2KEY", "TDES_3KEY"];

/// The sentence this service refuses an import whose wrapped key block is not valid
/// key block data with.
///
/// Fixed text with no variable part: a block that cannot be read, that no longer counts
/// its own characters, whose header or payload was altered after it was built, or that
/// was never wrapped under the key the request names are one refusal — the party that
/// sent it learns that the payload's key block data is not valid, and nothing about
/// which field of it this service could not read.
const KEY_BLOCK_INVALID: &str = "KeyBlock data in the imported payload is invalid.";

/// Why a key could not be imported.
#[derive(Debug, Clone)]
pub enum ImportRejection {
    /// An identifier names no key this store holds, so there is nothing to unwrap the
    /// material with. Carries the identifier AS THE REQUEST GAVE IT, which is what the
    /// modelled `ResourceNotFoundException.ResourceId` reports.
    NotFound(String),
    /// The request is inadmissible; the message says why, naming the member.
    Invalid(String),
    /// Storing the imported key would take this account past a bound; the message says
    /// which.
    QuotaExceeded(String),
    /// The request is admissible and this service could not store the key: nothing the
    /// caller named is wrong, so the answer is a server error rather than a refusal.
    Internal(String),
}

impl From<ExportRejection> for ImportRejection {
    /// The lookup of the key encryption key is shared with the export path, which is
    /// why it answers that type; its two refusal classes are the same two here.
    fn from(rejection: ExportRejection) -> Self {
        match rejection {
            ExportRejection::NotFound(identifier) => Self::NotFound(identifier),
            ExportRejection::Invalid(why) => Self::Invalid(why),
        }
    }
}

impl From<MintRejection> for ImportRejection {
    fn from(rejection: MintRejection) -> Self {
        match rejection {
            MintRejection::Invalid(why) => Self::Invalid(why),
            MintRejection::QuotaExceeded(why) => Self::QuotaExceeded(why),
            MintRejection::Internal(why) => Self::Internal(why),
        }
    }
}

impl From<TagRejection> for ImportRejection {
    fn from(rejection: TagRejection) -> Self {
        match rejection {
            TagRejection::Invalid(why) => Self::Invalid(why),
            TagRejection::QuotaExceeded(why) => Self::QuotaExceeded(why),
        }
    }
}

/// What one `ImportKey` call asks for beside the mechanism's own parameters: everything
/// about the record the imported key becomes rather than about the material itself.
#[derive(Debug, Clone, Copy)]
pub struct KeyImport<'a> {
    /// The region the imported key's `KeyArn` is minted in.
    pub region: &'a str,
    /// `KeyCheckValueAlgorithm`: the algorithm the imported key's check value is
    /// computed by, or that key's own default when the request names none.
    pub key_check_value_algorithm: Option<&'a str>,
    /// `Enabled`, or the documented default when the request names none.
    pub enabled: Option<bool>,
    /// `Tags`, assigned to the key as it is imported.
    pub tags: &'a [Tag],
    /// `ReplicationRegions`, the configuration the imported key replicates under;
    /// empty takes the account's defaults, exactly as a create does.
    pub replication_regions: &'a [String],
}

impl KeyStore {
    /// Import the key the TR-31 key block `request` carries, wrapped under the key
    /// encryption key it names, and answer the stored record.
    ///
    /// "For symmetric key exchange, Amazon Web Services Payment Cryptography uses the
    /// ANSI X9 TR-31 norm" (the operation's own documentation): the block is unwrapped
    /// under a key encryption key this store holds, its header is what says which key
    /// it carries, and the key that is stored is the one the block's payload actually
    /// yielded.
    pub fn import_tr31_key_block(
        &mut self,
        import: &KeyImport<'_>,
        request: &ImportTr31KeyBlock,
        now: SystemTime,
    ) -> Result<StoredKey, ImportRejection> {
        let tags = tag_map(import.tags)?;
        let wrapping = self.key_encryption_key(
            &request.wrapping_key_identifier,
            KeyProtectionUse::Unwrap,
            now,
        )?;
        let parsed = key_block::parse(
            &request.wrapped_key_block,
            wrapping.material.bytes(),
            &wrapping.key_attributes.key_algorithm,
        )
        .map_err(invalid_key_block)?;

        let key_attributes = imported_attributes(&parsed)?;
        // `KeyExportability` "can be used to further restrict exportability of the key
        // after export" (`ExportKey`'s own documentation), so the character the block
        // carries is what decides whether the imported key may leave this service
        // again.
        let exportable =
            exportable_under(parsed.exportability).ok_or_else(|| {
                invalid_key_block(format!(
                    "its exportability is {}, which the standard does not define",
                    parsed.exportability
                ))
            })?;
        self.store_key(
            import.region,
            KeyRecord {
                key_attributes,
                material: KeyMaterial(parsed.payload),
                // A TR-31 key block carries one secret, never half of a pair, so the
                // imported key is no key pair and nothing about one is invented for it.
                key_pair: None,
                key_origin: KEY_ORIGIN_EXTERNAL,
                key_check_value_algorithm: import
                    .key_check_value_algorithm
                    .map(str::to_string),
                exportable,
                enabled: import.enabled,
                // `DeriveKeyUsage` is a member of `CreateKey` and of no import
                // mechanism, and a key block header carries nothing like it.
                derive_key_usage: None,
                tags,
                replication_regions: import.replication_regions.to_vec(),
            },
            now,
        )
        .map_err(Into::into)
    }

    /// Import the key the AS 2805 key cryptogram `request` carries, encrypted under the
    /// variant of the key encryption key it names, and answer the stored record.
    ///
    /// The inverse of the export of a cryptogram: the same variant of the same key
    /// encryption key decrypts it. A cryptogram carries no header, so what the caller
    /// declares about the key is checked against the material that comes out — a
    /// cryptogram that does not yield a key of the declared algorithm's length is
    /// refused rather than stored under a declaration it does not support.
    pub fn import_as2805_key_cryptogram(
        &mut self,
        import: &KeyImport<'_>,
        request: &ImportAs2805KeyCryptogram,
        now: SystemTime,
    ) -> Result<StoredKey, ImportRejection> {
        let tags = tag_map(import.tags)?;
        let key_algorithm = request.key_algorithm.clone();
        if !is_symmetric_algorithm(&key_algorithm) {
            return Err(ImportRejection::Invalid(format!(
                "an AS 2805 key cryptogram carries a symmetric key, and KeyAlgorithm names a \
                 {key_algorithm} key"
            )));
        }
        let expected_len = material_len(&key_algorithm).ok_or_else(|| {
            ImportRejection::Invalid(format!(
                "KeyAlgorithm {key_algorithm} is not an algorithm this service holds key \
                 material for"
            ))
        })?;
        let key_usage = as2805_key_usage(&request.as2805_key_variant).ok_or_else(|| {
            ImportRejection::Invalid(format!(
                "As2805KeyVariant {} names no key this service imports",
                request.as2805_key_variant
            ))
        })?;
        let variant = as2805_variant(&request.as2805_key_variant).ok_or_else(|| {
            ImportRejection::Invalid(format!(
                "As2805KeyVariant {} names no key variant this service forms",
                request.as2805_key_variant
            ))
        })?;
        // The cryptogram is the key encrypted under the key variant and nothing else, so
        // it is exactly as long as the key it carries: the REQUEST'S OWN LENGTH says
        // whether it can carry a key of the declared algorithm, and it is judged here —
        // before anything is decoded or decrypted — so neither the decode nor the cipher
        // ever runs over more bytes than one key of that algorithm.
        let expected_characters = expected_len.saturating_mul(2);
        if request.wrapped_key_cryptogram.len() != expected_characters {
            return Err(ImportRejection::Invalid(format!(
                "WrappedKeyCryptogram carries {} characters, and a {key_algorithm} key \
                 encrypted under a key variant is {expected_characters}",
                request.wrapped_key_cryptogram.len()
            )));
        }

        let wrapping = self.key_encryption_key(
            &request.wrapping_key_identifier,
            KeyProtectionUse::Unwrap,
            now,
        )?;
        let wrapping_algorithm = &wrapping.key_attributes.key_algorithm;
        if !AS2805_WRAPPING_KEY_ALGORITHMS.contains(&wrapping_algorithm.as_str()) {
            return Err(ImportRejection::Invalid(format!(
                "an AS 2805 key cryptogram is encrypted under a TDES key encryption key, and \
                 the wrapping key named is a {wrapping_algorithm} key"
            )));
        }
        let cryptogram = hex::decode(&request.wrapped_key_cryptogram)
            .map_err(|_| ImportRejection::Invalid(hex_refusal("WrappedKeyCryptogram")))?;
        let varianted = as2805_varianted_key(wrapping.material.bytes(), variant);
        let material = tdes_decrypt_blocks(&varianted, &cryptogram).ok_or_else(|| {
            ImportRejection::Invalid(
                "WrappedKeyCryptogram is not a whole number of TDES blocks, so it is no key \
                 cryptogram this key encryption key encrypted"
                    .to_string(),
            )
        })?;

        self.store_key(
            import.region,
            KeyRecord {
                key_attributes: KeyAttributes {
                    key_usage: key_usage.to_string(),
                    key_class: KEY_CLASS_SYMMETRIC.to_string(),
                    key_algorithm,
                    key_modes_of_use: request.key_modes_of_use.clone(),
                },
                material: KeyMaterial(material),
                key_pair: None,
                key_origin: KEY_ORIGIN_EXTERNAL,
                key_check_value_algorithm: import
                    .key_check_value_algorithm
                    .map(str::to_string),
                // The only import mechanism whose own member says whether the imported
                // key may leave again.
                exportable: request.exportable,
                enabled: import.enabled,
                derive_key_usage: None,
                tags,
                replication_regions: import.replication_regions.to_vec(),
            },
            now,
        )
        .map_err(Into::into)
    }
}

/// The attributes of the key a parsed key block carries, every one of them read from
/// the block itself.
///
/// A TR-31 key block describes the key inside it: the header's key usage names what the
/// key is for, its algorithm character together with the recovered key's own length
/// names the key's `KeyAlgorithm`, and its mode of use names what may be done with the
/// key. Nothing here comes from the request, so a caller cannot import a key under
/// attributes the wrapped material does not support.
fn imported_attributes(block: &ParsedKeyBlock) -> Result<KeyAttributes, ImportRejection> {
    let key_algorithm = imported_key_algorithm(block.algorithm, block.payload.len())?;
    let key_usage = imported_key_usage(&block.usage_code)?;
    // The three header fields have to describe ONE key: a key block this service
    // imports carries a single secret, so a usage naming one half of an asymmetric pair
    // is refused rather than stored against symmetric material.
    let class_by_usage = key_usage_class(key_usage).unwrap_or(KEY_CLASS_SYMMETRIC);
    if class_by_usage != KEY_CLASS_SYMMETRIC {
        return Err(invalid_key_block(format!(
            "its key usage {} names a {class_by_usage}, and a key block this service imports \
             carries a {KEY_CLASS_SYMMETRIC}",
            block.usage_code
        )));
    }
    let key_modes_of_use = key_block::modes_of_use_from_code(block.mode).ok_or_else(|| {
        invalid_key_block(format!(
            "its mode of use is {}, which the standard does not define",
            block.mode
        ))
    })?;
    Ok(KeyAttributes {
        key_usage: key_usage.to_string(),
        key_class: KEY_CLASS_SYMMETRIC.to_string(),
        key_algorithm: key_algorithm.to_string(),
        key_modes_of_use,
    })
}

/// The modelled `KeyUsage` the key block key usage `usage_code` names.
///
/// Answered by searching the modelled values for the one whose own X9.143 key usage is
/// this code, so the two vocabularies are related by the single table the export side
/// renders a header from ([`tr31_key_usage`]) and a code neither direction knows is
/// refused in both.
fn imported_key_usage(usage_code: &str) -> Result<&'static str, ImportRejection> {
    KEY_USAGE_VALUES
        .iter()
        .copied()
        .find(|usage| tr31_key_usage(usage) == Some(usage_code))
        .ok_or_else(|| {
            invalid_key_block(format!(
                "its key usage {usage_code} is not a KeyUsage this service holds a key under"
            ))
        })
}

/// The modelled `KeyAlgorithm` of the key a block carries, from the header's algorithm
/// character and the recovered key's length.
///
/// A block's header names the key's FAMILY and the payload says how long the key is, so
/// the two together name the algorithm ([`key_block::key_algorithm_from_code`]). A
/// family whose modelled algorithm the block does not determine — an HMAC key, whose
/// hash is not in the header, or an asymmetric key, which is half a pair rather than a
/// secret — is refused naming what the header said, rather than stored as a guess.
fn imported_key_algorithm(
    algorithm: char,
    key_len: usize,
) -> Result<&'static str, ImportRejection> {
    key_block::key_algorithm_from_code(algorithm, key_len).ok_or_else(|| {
        invalid_key_block(format!(
            "it carries {key_len} bytes of a key whose algorithm its header gives as \
             {algorithm}, which names no KeyAlgorithm this service imports a key under"
        ))
    })
}

/// Whether a key whose key block header carries the exportability `code` may be
/// exported from this service again.
///
/// The three characters X9.143 defines: a key marked exportable may be exported under a
/// key encryption key, a sensitive one may be exported too (under a key encryption key,
/// in a form that need not meet the X9.24 requirements), and a non-exportable one may
/// not be exported at all. `None` for a character the standard does not define, which
/// the import is refused for.
fn exportable_under(code: char) -> Option<bool> {
    let exportable = match code {
        'E' | 'S' => true,
        'N' => false,
        _ => return None,
    };
    Some(exportable)
}

/// The modelled `KeyUsage` of the key an AS 2805 key cryptogram carries under
/// `variant`.
///
/// `ImportAs2805KeyCryptogram` carries no `KeyUsage`, and the variant it names is what
/// says what the key is for: each modelled `As2805KeyVariant` value
/// (`smithy_query(query="shape",shape="As2805KeyVariant")`) names the kind of key the
/// standard forms that variant for, and each row below is the modelled `KeyUsage` of
/// that kind — the terminal major key is the key other keys are sent to a terminal
/// under, so it is a key encryption key, and AS 2805's message authentication is the
/// ISO 9797-1 MAC. Matched exactly, like every table over a modelled enum in this
/// crate, so a variant the model gains later is refused instead of being stored under a
/// usage guessed from its name.
fn as2805_key_usage(variant: &str) -> Option<&'static str> {
    let usage = match variant {
        "TERMINAL_MAJOR_KEY_VARIANT_00" => "TR31_K0_KEY_ENCRYPTION_KEY",
        "PIN_ENCRYPTION_KEY_VARIANT_28" => "TR31_P0_PIN_ENCRYPTION_KEY",
        "MESSAGE_AUTHENTICATION_KEY_VARIANT_24" => "TR31_M1_ISO_9797_1_MAC_KEY",
        "DATA_ENCRYPTION_KEY_VARIANT_22" => "TR31_D0_SYMMETRIC_DATA_ENCRYPTION_KEY",
        _ => return None,
    };
    Some(usage)
}

/// How this service words a member that should have carried hexadecimal and did not.
fn hex_refusal(member: &str) -> String {
    format!("{member} is not hexadecimal, so it carries no wrapped key material")
}

/// One refused import of a key block whose own data could not be read, as the caller is
/// answered and as this emulator records it.
///
/// The caller is answered [`KEY_BLOCK_INVALID`], the one sentence this class of refusal
/// carries; `why` says which field of the block could not be read and is kept local, so
/// this service can be debugged without the refusal telling a sending party anything
/// about the block or the key. It carries no key material: the key inside a block that
/// does not authenticate is never recovered at all.
fn invalid_key_block(why: impl AsRef<str>) -> ImportRejection {
    ::tracing::debug!(
        target: "emulator::audit",
        reason = why.as_ref(),
        "a wrapped key block under import is not valid key block data"
    );
    ImportRejection::Invalid(KEY_BLOCK_INVALID.to_string())
}
