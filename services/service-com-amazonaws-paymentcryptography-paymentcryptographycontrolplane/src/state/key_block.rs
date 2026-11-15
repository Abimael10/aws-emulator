// SPDX-License-Identifier: CC0-1.0

//! The ASC X9.143 (TR-31) key block a key travels in, in both directions.
//!
//! One concern: the key block itself. [`build`] turns a key's stored material, plus the
//! header fields a caller asked for, into the ASCII block `ExportKey` answers with, and
//! [`parse`] reads such a block back into the key it carries and what its header says
//! about that key — which is what `ImportKey` stores. Both are encrypted and
//! authenticated under a key encryption key this store also holds, and the key material
//! itself never leaves [`super`] in the clear: this module wraps it, unwraps it, and
//! hands back the block or the key.
//!
//! The block is the one the standard defines, built with real primitives
//! ([`super::block_cipher`]):
//!
//! * the key block encryption key and the key block MAC key are DERIVED from the
//!   wrapping key (the key block protection key) by the CMAC-based derivation the
//!   standard's key derivation binding method uses — version `B` for a TDES protection
//!   key, version `D` for an AES one;
//! * the MAC is a CMAC over the header, the optional blocks and the plaintext payload;
//! * that MAC is the initialisation vector of the payload's CBC encryption, so the
//!   authentication is bound to the ciphertext — and a block read back is authenticated
//!   by recomputing that MAC over what came out and comparing it with what the block
//!   carries.
//!
//! Each table over a header field sits beside its inverse, so the two directions cannot
//! come to disagree about what a character means.
//!
//! What is written here is the block's own layout, which no crate on the registry
//! implements; every cipher, MAC and chaining step is a crate's.

use std::collections::BTreeMap;

use crate::ops::types::KeyModesOfUse;

use super::block_cipher::{
    aes_cbc_decrypt, aes_cbc_encrypt, aes_cmac, tdes_cbc_decrypt, tdes_cbc_encrypt, tdes_cmac,
    AES_BLOCK, TDES_BLOCK,
};

/// The version id of a key block whose protection key is a TDES key: the TDES key
/// derivation binding method.
const VERSION_TDES: char = 'B';

/// The version id of a key block whose protection key is an AES key: the AES key
/// derivation binding method.
const VERSION_AES: char = 'D';

/// How many characters the key block header occupies: version id, length, key usage,
/// algorithm, mode of use, key version, exportability, optional block count and the two
/// reserved characters.
const HEADER_LEN: usize = 16;

/// The largest key block the header's 4-character length field can describe.
pub(super) const MAX_BLOCK_LEN: usize = 9_999;

/// The most optional blocks the header's 2-character count field can describe.
pub(super) const MAX_OPTIONAL_BLOCKS: usize = 99;

/// The optional block id the standard reserves for the padding block that aligns the
/// header region, so a caller cannot supply one of its own.
const PADDING_BLOCK_ID: &str = "PB";

/// The shortest optional block: its two-character id and its two-character length.
const OPTIONAL_BLOCK_OVERHEAD: usize = 4;

/// The key version number a key block carries when the key has no version of its own,
/// from the standard's "key version number is not used" value.
pub(super) const KEY_VERSION_UNUSED: &str = "00";

/// The derivation data's key usage indicator for the key block ENCRYPTION key.
const DERIVE_USAGE_ENCRYPTION: [u8; 2] = [0x00, 0x00];

/// The derivation data's key usage indicator for the key block MAC key.
const DERIVE_USAGE_MAC: [u8; 2] = [0x00, 0x01];

/// The separator the derivation data carries between the key usage indicator and the
/// algorithm.
const DERIVE_SEPARATOR: u8 = 0x00;

/// One key block to build: what is wrapped, what the header says about it, and the key
/// it is wrapped under.
pub(super) struct KeyBlock<'a> {
    /// The key material the block carries.
    pub payload: &'a [u8],
    /// The `KeyAlgorithm` of that material, which the header's algorithm comes from.
    pub payload_algorithm: &'a str,
    /// The two-character key usage the header carries.
    pub usage_code: &'a str,
    /// The modes of use the header's mode of use comes from.
    pub modes_of_use: &'a KeyModesOfUse,
    /// The header's exportability character.
    pub exportability: char,
    /// The header's two-character key version number.
    pub key_version: &'a str,
    /// The optional blocks the header carries, in one stable order.
    pub optional_blocks: &'a BTreeMap<String, String>,
    /// The key block protection key: the wrapping key's own stored material.
    pub protection_key: &'a [u8],
    /// That key's `KeyAlgorithm`, which decides the binding method.
    pub protection_key_algorithm: &'a str,
    /// Bytes the payload's trailing padding is taken from, so two exports of one key
    /// are not the same block.
    pub padding: &'a [u8],
}

/// Build `block`, or say why it cannot be built.
///
/// Every refusal names the member that cannot be rendered and what the standard admits
/// there, so a caller can fix the request from the sentence alone.
pub(super) fn build(block: &KeyBlock<'_>) -> Result<String, String> {
    let (version, block_size, mac_len) = binding_method(block.protection_key_algorithm)?;
    let algorithm = algorithm_code(block.payload_algorithm).ok_or_else(|| {
        format!(
            "a TR-31 key block carries no algorithm character for a {} key",
            block.payload_algorithm
        )
    })?;
    let mode = mode_of_use(block.modes_of_use).ok_or_else(|| {
        "KeyModesOfUse names no mode of use a TR-31 key block header can carry".to_string()
    })?;
    if block.usage_code.len() != 2 {
        return Err(format!(
            "a TR-31 key block header carries a two-character key usage, not {:?}",
            block.usage_code
        ));
    }
    if block.key_version.len() != 2 {
        return Err(format!(
            "KeyBlockHeaders.KeyVersion carries two characters, not {:?}",
            block.key_version
        ));
    }

    let (optional, optional_count) = optional_block_region(block.optional_blocks, block_size)?;
    let plaintext = payload(block.payload, block.padding, block_size)?;

    // The header's own length field covers the whole block, so it is computed BEFORE
    // the header is rendered: the header is part of what the MAC authenticates, and a
    // length filled in afterwards would not be the text that was authenticated.
    let total = HEADER_LEN
        .checked_add(optional.len())
        .and_then(|len| len.checked_add(plaintext.len().checked_mul(2)?))
        .and_then(|len| len.checked_add(mac_len.checked_mul(2)?))
        .ok_or_else(|| "the key under export does not fit a TR-31 key block".to_string())?;
    if total > MAX_BLOCK_LEN {
        return Err(format!(
            "the key block this request describes is {total} characters, more than the \
             {MAX_BLOCK_LEN} a TR-31 key block holds; name fewer or shorter optional blocks"
        ));
    }

    let header = format!(
        "{version}{total:04}{usage}{algorithm}{mode}{key_version}{exportability}\
         {optional_count:02}00",
        usage = block.usage_code,
        key_version = block.key_version,
        exportability = block.exportability,
    );

    let mut authenticated = Vec::with_capacity(header.len() + optional.len() + plaintext.len());
    authenticated.extend_from_slice(header.as_bytes());
    authenticated.extend_from_slice(optional.as_bytes());
    authenticated.extend_from_slice(&plaintext);

    let failed = || "the key under export could not be wrapped under the wrapping key".to_string();
    let encryption_key =
        derive(block.protection_key, DERIVE_USAGE_ENCRYPTION, version).ok_or_else(failed)?;
    let mac_key = derive(block.protection_key, DERIVE_USAGE_MAC, version).ok_or_else(failed)?;
    let mac = if version == VERSION_TDES {
        tdes_cmac(&mac_key, &authenticated)
    } else {
        aes_cmac(&mac_key, &authenticated)
    }
    .filter(|mac| mac.len() == mac_len)
    .ok_or_else(failed)?;
    let ciphertext = if version == VERSION_TDES {
        tdes_cbc_encrypt(&encryption_key, &mac, &plaintext)
    } else {
        aes_cbc_encrypt(&encryption_key, &mac, &plaintext)
    }
    .ok_or_else(failed)?;

    Ok(format!(
        "{header}{optional}{}{}",
        hex::encode_upper(&ciphertext),
        hex::encode_upper(&mac)
    ))
}

/// One key block read back: the key it carries, and what its header says about that
/// key.
///
/// The header fields a key RECORD does not keep are read, checked and dropped rather
/// than carried here: its key version number and its optional blocks say things about
/// the transfer (`Key` models no member for either), and its own length field and
/// padding block describe the block's layout.
#[derive(Debug, Clone)]
pub(super) struct ParsedKeyBlock {
    /// The key material the block carried, recovered from its payload.
    pub payload: Vec<u8>,
    /// The header's algorithm character, which with the payload's length is what names
    /// the key's `KeyAlgorithm`.
    pub algorithm: char,
    /// The header's two-character key usage.
    pub usage_code: String,
    /// The header's mode of use character.
    pub mode: char,
    /// The header's exportability character.
    pub exportability: char,
}

/// Read `block` back under the protection key it was wrapped with, or say why it is
/// not a key block that key authenticates.
///
/// The exact inverse of [`build`], step for step: the header is clear text, the
/// payload's encryption key and the block's MAC key are derived from the protection
/// key by the same binding method, the payload is decrypted from the MAC the block
/// carries as its initialisation vector, and the MAC is then RECOMPUTED over the
/// header, the optional blocks and the recovered plaintext and compared with the one
/// the block carries — so a block whose header, payload or MAC was altered after it
/// was built, or one built under another key, is refused rather than unwrapped into
/// whatever the ciphertext happens to decrypt to.
///
/// Every refusal says which field of the block could not be read, for the caller to
/// word the refusal from: a key block is read by a party that was sent one, so what a
/// SENDER is told about it is the importing operation's decision
/// ([`super::key_import`]), not this module's.
pub(super) fn parse(
    block: &str,
    protection_key: &[u8],
    protection_key_algorithm: &str,
) -> Result<ParsedKeyBlock, String> {
    let (version, block_size, mac_len) = binding_method(protection_key_algorithm)?;
    let malformed = |what: &str| what.to_string();
    if !block.is_ascii() || block.len() < HEADER_LEN {
        return Err(malformed(&format!(
            "a key block is at least the {HEADER_LEN} characters of its header"
        )));
    }

    let header = &block[..HEADER_LEN];
    let version_id = header[..1].chars().next().unwrap_or_default();
    if version_id != version {
        return Err(malformed(&format!(
            "it carries the key block version {version_id}, and a key block protected by a \
             {protection_key_algorithm} key carries version {version}"
        )));
    }
    let total: usize = header[1..5]
        .parse()
        .map_err(|_| malformed("its length field is not four decimal digits"))?;
    if total != block.len() {
        return Err(malformed(&format!(
            "its length field says {total} characters and it carries {}",
            block.len()
        )));
    }
    let usage_code = header[5..7].to_string();
    let algorithm = header[7..8].chars().next().unwrap_or_default();
    let mode = header[8..9].chars().next().unwrap_or_default();
    let exportability = header[11..12].chars().next().unwrap_or_default();
    let optional_count: usize = header[12..14]
        .parse()
        .map_err(|_| malformed("its optional block count is not two decimal digits"))?;

    let region_end = read_optional_blocks(block, optional_count, &malformed)?;
    let wrapped = &block[region_end..];
    let payload_len = wrapped
        .len()
        .checked_sub(mac_len * 2)
        .filter(|len| *len > 0 && len.is_multiple_of(block_size * 2))
        .ok_or_else(|| {
            malformed(&format!(
                "its wrapped payload and MAC are {} characters, and a key block protected by a \
                 {protection_key_algorithm} key carries a {mac_len}-byte MAC after a whole \
                 number of {block_size}-byte cipher blocks",
                wrapped.len()
            ))
        })?;
    let ciphertext = hex::decode(&wrapped[..payload_len])
        .map_err(|_| malformed("its wrapped payload is not hexadecimal"))?;
    let mac = hex::decode(&wrapped[payload_len..])
        .map_err(|_| malformed("its MAC is not hexadecimal"))?;

    let unusable = || "it could not be unwrapped under the wrapping key named".to_string();
    let encryption_key =
        derive(protection_key, DERIVE_USAGE_ENCRYPTION, version).ok_or_else(unusable)?;
    let mac_key = derive(protection_key, DERIVE_USAGE_MAC, version).ok_or_else(unusable)?;
    let plaintext = if version == VERSION_TDES {
        tdes_cbc_decrypt(&encryption_key, &mac, &ciphertext)
    } else {
        aes_cbc_decrypt(&encryption_key, &mac, &ciphertext)
    }
    .ok_or_else(unusable)?;

    let mut authenticated = Vec::with_capacity(region_end + plaintext.len());
    authenticated.extend_from_slice(&block.as_bytes()[..region_end]);
    authenticated.extend_from_slice(&plaintext);
    let recomputed = if version == VERSION_TDES {
        tdes_cmac(&mac_key, &authenticated)
    } else {
        aes_cmac(&mac_key, &authenticated)
    }
    .ok_or_else(unusable)?;
    if recomputed != mac {
        return Err("its MAC does not authenticate it under the wrapping key named, so the key \
                    block is not intact or was not wrapped under that key"
            .to_string());
    }

    Ok(ParsedKeyBlock {
        payload: read_payload(&plaintext, &malformed)?,
        algorithm,
        usage_code,
        mode,
        exportability,
    })
}

/// The offset the wrapped payload of `block` starts at: past its header and past the
/// `count` optional blocks that header says follow it.
///
/// Each block is framed exactly as [`optional_block_region`] writes one: its
/// two-character id, its length as two hexadecimal characters counting the id and the
/// length field themselves, then its value. The values are not kept — an optional block
/// carries data about the transfer rather than about the key — but every one of them is
/// walked, so a header region whose framing does not add up is refused instead of being
/// mistaken for the start of the payload.
fn read_optional_blocks(
    block: &str,
    count: usize,
    malformed: &dyn Fn(&str) -> String,
) -> Result<usize, String> {
    let mut at = HEADER_LEN;
    for _ in 0..count {
        let framed = block
            .get(at..at + OPTIONAL_BLOCK_OVERHEAD)
            .ok_or_else(|| malformed("it ends inside one of its optional blocks"))?;
        let len = usize::from_str_radix(&framed[2..4], 16).map_err(|_| {
            malformed("an optional block's length is not two hexadecimal characters")
        })?;
        if len < OPTIONAL_BLOCK_OVERHEAD || block.len() < at + len {
            return Err(malformed(&format!(
                "the optional block {} says it is {len} characters long",
                &framed[..2]
            )));
        }
        at += len;
    }
    Ok(at)
}

/// The key material a key block's decrypted payload carries: the length in bits the
/// payload opens with, then that many bits of key, then padding this service ignores.
fn read_payload(plaintext: &[u8], malformed: &dyn Fn(&str) -> String) -> Result<Vec<u8>, String> {
    let bits = plaintext
        .get(..2)
        .map(|field| u16::from_be_bytes([field[0], field[1]]))
        .ok_or_else(|| malformed("its payload carries no key length"))?;
    if !bits.is_multiple_of(8) {
        return Err(malformed(&format!(
            "its payload says the key is {bits} bits, which is not a whole number of bytes"
        )));
    }
    let len = usize::from(bits / 8);
    plaintext
        .get(2..2 + len)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            malformed(&format!(
                "its payload says the key is {bits} bits and carries {} bytes",
                plaintext.len().saturating_sub(2)
            ))
        })
}

/// The binding method a key block protected by `key_algorithm` uses: its version id,
/// the block size of its cipher, and the length of its MAC.
///
/// The modelled `KeyAlgorithm` values are matched exactly, the rule every table in this
/// module follows, so an algorithm the model gains later is refused rather than wrapped
/// under a method that was not defined for it.
fn binding_method(key_algorithm: &str) -> Result<(char, usize, usize), String> {
    match key_algorithm {
        "TDES_2KEY" | "TDES_3KEY" => Ok((VERSION_TDES, TDES_BLOCK, TDES_BLOCK)),
        "AES_128" | "AES_192" | "AES_256" => Ok((VERSION_AES, AES_BLOCK, AES_BLOCK)),
        other => Err(format!(
            "a TR-31 key block is protected by a TDES or an AES key encryption key, and the \
             wrapping key named is a {other} key"
        )),
    }
}

/// The optional block region of the header, and how many blocks it carries.
///
/// Each block is its two-character id, its length as two hexadecimal characters
/// (counting the id and the length field themselves), then its value. A padding block
/// closes the region when the header and the blocks together are not a multiple of the
/// cipher's block size, which the standard requires them to be.
fn optional_block_region(
    blocks: &BTreeMap<String, String>,
    block_size: usize,
) -> Result<(String, usize), String> {
    if blocks.len() > MAX_OPTIONAL_BLOCKS {
        return Err(format!(
            "KeyBlockHeaders.OptionalBlocks names {} blocks, more than the \
             {MAX_OPTIONAL_BLOCKS} a TR-31 key block header holds",
            blocks.len()
        ));
    }
    let mut region = String::new();
    let mut count = 0usize;
    for (id, value) in blocks {
        if id.len() != 2 {
            return Err(format!(
                "KeyBlockHeaders.OptionalBlocks names the block id {id:?}, and an optional \
                 block id is two characters"
            ));
        }
        if id == PADDING_BLOCK_ID {
            return Err(format!(
                "KeyBlockHeaders.OptionalBlocks names the block id {PADDING_BLOCK_ID}, which a \
                 TR-31 key block reserves for its padding block"
            ));
        }
        let len = value
            .len()
            .checked_add(OPTIONAL_BLOCK_OVERHEAD)
            .filter(|len| *len <= 0xFF)
            .ok_or_else(|| {
                format!(
                    "the optional block {id} carries {} characters, more than one TR-31 optional \
                     block holds",
                    value.len()
                )
            })?;
        region.push_str(id);
        region.push_str(&format!("{len:02X}"));
        region.push_str(value);
        count += 1;
    }

    let aligned = HEADER_LEN
        .checked_add(region.len())
        .ok_or_else(|| "the optional blocks do not fit a TR-31 key block".to_string())?;
    let mut filler = (block_size - aligned % block_size) % block_size;
    if filler > 0 {
        // A padding block cannot be shorter than its own id and length field, so a
        // deficit smaller than that takes one more whole cipher block.
        if filler < OPTIONAL_BLOCK_OVERHEAD {
            filler += block_size;
        }
        region.push_str(PADDING_BLOCK_ID);
        region.push_str(&format!("{filler:02X}"));
        for _ in 0..(filler - OPTIONAL_BLOCK_OVERHEAD) {
            region.push('0');
        }
        count += 1;
    }
    if count > MAX_OPTIONAL_BLOCKS {
        return Err(format!(
            "KeyBlockHeaders.OptionalBlocks needs a padding block, which would take the header \
             past the {MAX_OPTIONAL_BLOCKS} optional blocks a TR-31 key block holds"
        ));
    }
    Ok((region, count))
}

/// The plaintext payload of a key block: the key's length in bits, the key itself, then
/// padding up to a whole number of cipher blocks.
fn payload(key: &[u8], padding: &[u8], block_size: usize) -> Result<Vec<u8>, String> {
    let bits = key
        .len()
        .checked_mul(8)
        .and_then(|bits| u16::try_from(bits).ok())
        .ok_or_else(|| {
            "the key under export is longer than a TR-31 key block payload holds".to_string()
        })?;
    let mut plaintext = Vec::with_capacity(block_size);
    plaintext.extend_from_slice(&bits.to_be_bytes());
    plaintext.extend_from_slice(key);
    let filler = (block_size - plaintext.len() % block_size) % block_size;
    let filler = padding.get(..filler).ok_or_else(|| {
        "the key under export could not be padded to a whole TR-31 key block".to_string()
    })?;
    plaintext.extend_from_slice(filler);
    Ok(plaintext)
}

/// The key block encryption or MAC key derived from the protection key `kbpk`.
///
/// The derivation is the standard's: a CMAC under the protection key over derivation
/// data naming the counter, which of the two keys is being derived, the algorithm and
/// the key length — run once per cipher block of the key being derived, so a key longer
/// than one block is the concatenation of consecutive counters.
fn derive(kbpk: &[u8], usage: [u8; 2], version: char) -> Option<Vec<u8>> {
    let (algorithm, cipher_block) = derivation_algorithm(kbpk.len(), version)?;
    let bits = u16::try_from(kbpk.len().checked_mul(8)?).ok()?;
    let mut derived: Vec<u8> = Vec::with_capacity(kbpk.len());
    let mut counter: u8 = 1;
    while derived.len() < kbpk.len() {
        let mut data = vec![counter, usage[0], usage[1], DERIVE_SEPARATOR];
        data.extend_from_slice(&algorithm);
        data.extend_from_slice(&bits.to_be_bytes());
        // One cipher block of derivation data, zero filled.
        data.resize(cipher_block, 0);
        let block = if version == VERSION_TDES {
            tdes_cmac(kbpk, &data)?
        } else {
            aes_cmac(kbpk, &data)?
        };
        derived.extend_from_slice(&block);
        counter = counter.checked_add(1)?;
    }
    derived.truncate(kbpk.len());
    Some(derived)
}

/// The derivation data's algorithm indicator for a protection key of `key_len` bytes
/// under `version`, and the cipher block that version's derivation data fills.
fn derivation_algorithm(key_len: usize, version: char) -> Option<([u8; 2], usize)> {
    let algorithm = match (version, key_len) {
        (VERSION_TDES, 16) => [0x00, 0x00],
        (VERSION_TDES, 24) => [0x00, 0x01],
        (VERSION_AES, 16) => [0x00, 0x02],
        (VERSION_AES, 24) => [0x00, 0x03],
        (VERSION_AES, 32) => [0x00, 0x04],
        _ => return None,
    };
    let cipher_block = if version == VERSION_TDES {
        TDES_BLOCK
    } else {
        AES_BLOCK
    };
    Some((algorithm, cipher_block))
}

/// The header's algorithm character for a key of `key_algorithm`.
///
/// The characters are the standard's: `T` for Triple DES, `A` for AES, `H` for HMAC,
/// `R` for RSA and `E` for an elliptic curve key. The modelled `KeyAlgorithm` values
/// (`smithy_query(query="shape",shape="KeyAlgorithm")`) are matched exactly rather than
/// by a name prefix, the rule the rest of this module's tables follow: an algorithm the
/// model gains later answers `None` and the export is refused instead of being rendered
/// under the wrong character.
fn algorithm_code(key_algorithm: &str) -> Option<char> {
    let code = match key_algorithm {
        "TDES_2KEY" | "TDES_3KEY" => 'T',
        "AES_128" | "AES_192" | "AES_256" => 'A',
        "HMAC_SHA224" | "HMAC_SHA256" | "HMAC_SHA384" | "HMAC_SHA512" => 'H',
        "RSA_2048" | "RSA_3072" | "RSA_4096" => 'R',
        "ECC_NIST_P256" | "ECC_NIST_P384" | "ECC_NIST_P521" => 'E',
        _ => return None,
    };
    Some(code)
}

/// The modelled `KeyAlgorithm` of a key whose header gives its algorithm as `code` and
/// whose recovered material is `key_len` bytes.
///
/// The header names the key's FAMILY ([`algorithm_code`]) and the payload says how long
/// the key is, so the two together name one modelled algorithm: each row below pairs a
/// character that table renders with a length `super::material_len` gives for exactly
/// one value of that family. The two families a key block cannot determine are absent
/// and answer `None`: an HMAC key's hash is not in the header (two of the modelled HMAC
/// algorithms share a key length), and an RSA or elliptic curve key is one half of a
/// pair rather than a secret of a known length.
pub(super) fn key_algorithm_from_code(code: char, key_len: usize) -> Option<&'static str> {
    let key_algorithm = match (code, key_len) {
        ('T', 16) => "TDES_2KEY",
        ('T', 24) => "TDES_3KEY",
        ('A', 16) => "AES_128",
        ('A', 24) => "AES_192",
        ('A', 32) => "AES_256",
        _ => return None,
    };
    Some(key_algorithm)
}

/// The header's mode of use character for `modes`.
///
/// The standard gives one character per permitted use, and a key's nine modelled modes
/// of use can name a combination of them, so the rows are tried in the order below and
/// the first that holds is the mode the header carries: no restrictions, then the
/// signing combinations, then the enciphering ones, then generation, then derivation.
/// `None` for a key that permits nothing at all, which no mode of use describes.
fn mode_of_use(modes: &KeyModesOfUse) -> Option<char> {
    let enciphers = modes.encrypt || modes.wrap;
    let deciphers = modes.decrypt || modes.unwrap;
    let mode = if modes.no_restrictions {
        'N'
    } else if modes.sign && deciphers {
        'T'
    } else if modes.generate && modes.verify {
        'C'
    } else if modes.sign {
        'S'
    } else if modes.verify && !enciphers && !deciphers {
        'V'
    } else if enciphers && deciphers {
        'B'
    } else if enciphers {
        'E'
    } else if deciphers {
        'D'
    } else if modes.generate {
        'G'
    } else if modes.derive_key {
        'X'
    } else {
        return None;
    };
    Some(mode)
}

/// The modes of use a key carries whose header gives its mode of use as `mode`.
///
/// The inverse of [`mode_of_use`]: the standard's one character per permitted use, read
/// back as the modelled members that name those uses. A character names BOTH the data
/// operation and the key operation it permits — the standard's own readings are "both
/// encrypt and decrypt / wrap and unwrap" for `B`, "encrypt / wrap only" for `E` and
/// "decrypt / unwrap only" for `D` — so each of them is read back as both: a header
/// cannot say which of the two a key is restricted to, and inventing a restriction it
/// does not state would deny the imported key a use the block permits.
///
/// `None` for a character the standard does not define.
pub(super) fn modes_of_use_from_code(mode: char) -> Option<KeyModesOfUse> {
    let mut modes = KeyModesOfUse::default();
    let encipher = |modes: &mut KeyModesOfUse| {
        modes.encrypt = true;
        modes.wrap = true;
    };
    let decipher = |modes: &mut KeyModesOfUse| {
        modes.decrypt = true;
        modes.unwrap = true;
    };
    match mode {
        'N' => modes.no_restrictions = true,
        'B' => {
            encipher(&mut modes);
            decipher(&mut modes);
        }
        'C' => {
            modes.generate = true;
            modes.verify = true;
        }
        'D' => decipher(&mut modes),
        'E' => encipher(&mut modes),
        'G' => modes.generate = true,
        'S' => modes.sign = true,
        'T' => {
            modes.sign = true;
            decipher(&mut modes);
        }
        'V' => modes.verify = true,
        'X' => modes.derive_key = true,
        _ => return None,
    }
    Some(modes)
}
