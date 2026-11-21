// SPDX-License-Identifier: CC0-1.0

//! The DUKPT key derivations: the data-encryption working key a terminal and the host
//! each compute, on their own, from a base derivation key and a key serial number.
//!
//! DUKPT (derive unique key per transaction) never encrypts under the base derivation
//! key itself. The published chain, ANSI X9.24, is three steps, and which of its two
//! parts is walked follows the BASE DERIVATION KEY's own algorithm — a Triple DES base
//! derivation key names the Triple DES scheme of X9.24-1, an AES one the AES scheme of
//! X9.24-3:
//!
//! 1. **the initial key** — the key the device was loaded with, which the host
//!    recomputes from the base derivation key and the serial number's own initial half.
//!    That step is already written for the other plane of this service, where
//!    `ExportKey` hands a caller an initial key of its own
//!    ([`key_store::dukpt_initial_key`]), and it is a function of those two inputs
//!    alone — so it is CALLED here rather than written a second time; only the refusals
//!    this operation words for itself are decided before the call;
//! 2. **the current transaction key** — the initial key walked forward over the SET
//!    BITS of the transaction counter the serial number carries, one derivation per
//!    bit: the non-reversible key generation process of X9.24-1 over the serial number
//!    register, or the derive-key function of X9.24-3 over the counter reached so far.
//!    That is what makes the key unique per transaction and the walk one-way;
//! 3. **the working key** — the data-encryption key the transaction key names for the
//!    direction the request asks for: in X9.24-1, the transaction key XORed with the
//!    standard's data-encryption variant and then encrypted under itself, half by half;
//!    in X9.24-3, one more derive-key with the data-encryption key usage.
//!
//! Every step is a function of the base derivation key's working material and the
//! request alone, so the same serial number and the same attributes always derive the
//! same key: that is what lets a `DecryptData` under one key serial number recover what
//! an `EncryptData` under it produced, and no derived key is stored anywhere.

use std::fmt;

use ::service_com_amazonaws_paymentcryptography_paymentcryptographycontrolplane::state as key_store;

use super::{single_des, BlockCipher, HEX_CHARACTERS_PER_BYTE};

/// Why no DUKPT working key could be derived.
#[derive(Debug, Clone)]
pub enum DerivationRefusal {
    /// The request is inadmissible; the message says why, naming the member. The
    /// modelled `ValidationException`.
    Invalid(String),
    /// The working material held for a key the derivation ADMITS turned out not to key
    /// it — an invariant of this emulator's own material rather than anything the
    /// caller asked for, so it is a server fault and not a refusal of the request.
    Unusable(String),
}

/// The modelled `DukptKeyVariant` value for a key used to encrypt data the terminal
/// sends.
const VARIANT_REQUEST: &str = "REQUEST";

/// The modelled `DukptKeyVariant` value for a key used to decrypt data the terminal
/// receives.
const VARIANT_RESPONSE: &str = "RESPONSE";

/// The modelled `DukptKeyVariant` value for a key used in both directions.
const VARIANT_BIDIRECTIONAL: &str = "BIDIRECTIONAL";

/// The `DukptKeyVariant` applied when the request names none: the variant that is used
/// in both directions ("incoming data decryption, outgoing data encryption, or both" —
/// `DukptEncryptionAttributes$DukptKeyVariant`), so an encrypt and the decrypt that
/// inverts it derive one key when neither names a variant.
const DEFAULT_KEY_VARIANT: &str = VARIANT_BIDIRECTIONAL;

/// What one DUKPT derivation runs over: the base derivation key, and the attributes the
/// request named.
pub struct Derivation<'a> {
    /// The base derivation key's modelled `KeyAlgorithm`, which names the scheme.
    pub key_algorithm: &'a str,
    /// That key's working material.
    pub material: &'a [u8],
    /// The `KeySerialNumber` the request carries, as the hex the model admits.
    pub key_serial_number: &'a str,
    /// The `DukptKeyDerivationType` the request names, if any.
    pub derivation_type: Option<&'a str>,
    /// The `DukptKeyVariant` the request names, if any.
    pub key_variant: Option<&'a str>,
}

impl fmt::Debug for Derivation<'_> {
    /// Names the scheme and the attributes, and never the key material.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Derivation")
            .field("key_algorithm", &self.key_algorithm)
            .field("material", &format_args!("<redacted>"))
            .field("key_serial_number", &self.key_serial_number)
            .field("derivation_type", &self.derivation_type)
            .field("key_variant", &self.key_variant)
            .finish()
    }
}

/// The key a DUKPT derivation produced: the algorithm it is keyed as, and its bytes.
pub struct WorkingKey {
    /// The modelled `KeyAlgorithm` the derived key is keyed under, which is what names
    /// the cipher and the block length the data is then transformed in.
    pub key_algorithm: &'static str,
    /// The derived key's bytes.
    pub material: Vec<u8>,
}

impl fmt::Debug for WorkingKey {
    /// Names the algorithm and the length, and never the derived key's bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkingKey")
            .field("key_algorithm", &self.key_algorithm)
            .field("material", &format_args!("<{} bytes>", self.material.len()))
            .finish()
    }
}

/// The DATA-ENCRYPTION working key `request` names, or the reason it names none.
///
/// Which scheme is walked is the base derivation key's own algorithm: "Amazon Web
/// Services Payment Cryptography supports the following encryption key schemes: DUKPT:
/// TDES and AES algorithms" (`ReEncryptData`'s own documentation), and the two
/// algorithms are two different standards rather than one with a wider key.
pub fn data_encryption_key(request: &Derivation<'_>) -> Result<WorkingKey, DerivationRefusal> {
    let Ok(serial_number) = hex::decode(request.key_serial_number) else {
        return Err(DerivationRefusal::Invalid(
            "KeySerialNumber must be hexadecimal.".to_string(),
        ));
    };
    let variant = request.key_variant.unwrap_or(DEFAULT_KEY_VARIANT);
    let Some(scheme) = scheme(request.key_algorithm) else {
        return Err(DerivationRefusal::Invalid(format!(
            "KeyAlgorithm {} is not valid for DUKPT key derivation.",
            request.key_algorithm
        )));
    };
    let serial_number_bytes = match scheme {
        Scheme::TripleDes => TRIPLE_DES_SERIAL_NUMBER_BYTES,
        Scheme::Aes => AES_SERIAL_NUMBER_BYTES,
    };
    if serial_number.len() != serial_number_bytes {
        return Err(DerivationRefusal::Invalid(format!(
            "KeySerialNumber must be {} hexadecimal characters for KeyAlgorithm {}.",
            serial_number_bytes * HEX_CHARACTERS_PER_BYTE,
            request.key_algorithm
        )));
    }
    let derived = derived_key_type(request, scheme)?;
    // The initial key the device was loaded with: the other plane's derivation, over
    // this plane's own working material for the base derivation key.
    let initial = key_store::dukpt_initial_key(
        request.key_algorithm,
        request.material,
        request.key_serial_number,
    )
    .map_err(|_| unusable_material(request.key_algorithm))?;
    match scheme {
        Scheme::TripleDes => triple_des_working_key(&initial.material, &serial_number, variant),
        Scheme::Aes => aes_working_key(request, &initial.material, &serial_number, &derived, variant),
    }
}

/// Which of the two DUKPT standards a base derivation key of one algorithm names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    /// The Triple DES scheme of ANSI X9.24-1.
    TripleDes,
    /// The AES scheme of ANSI X9.24-3.
    Aes,
}

/// The scheme a base derivation key of `key_algorithm` names, or `None` when that
/// algorithm names no DUKPT derivation at all.
///
/// The names are matched exactly against the control plane's modelled
/// `SymmetricKeyAlgorithm`, the rule every table over that enum in this crate follows,
/// so an algorithm the model gains later is refused rather than derived under a
/// standard that was not written for it. X9.24-1 defines its derivation under a
/// DOUBLE-LENGTH Triple DES key, which is the same set the other plane's initial key
/// derivation admits; the HMAC algorithms name a hash rather than a block cipher, and
/// neither standard derives under one.
fn scheme(key_algorithm: &str) -> Option<Scheme> {
    match key_algorithm {
        "TDES_2KEY" => Some(Scheme::TripleDes),
        "AES_128" | "AES_192" | "AES_256" => Some(Scheme::Aes),
        _ => None,
    }
}

/// One `DukptDerivationType`: the algorithm the derived key is keyed as, its length in
/// bytes, and the algorithm indicator the X9.24-3 derivation data carries for it.
#[derive(Debug, Clone, Copy)]
struct DerivedType {
    key_algorithm: &'static str,
    key_bytes: usize,
    indicator: [u8; 2],
}

/// The derived key type `name` names, or `None` when it names none.
///
/// The names are the modelled `DukptDerivationType` values
/// (`smithy_query(query="shape",shape="DukptDerivationType")`: TDES_2KEY, TDES_3KEY,
/// AES_128, AES_192, AES_256), each with the algorithm indicator X9.24-3's derivation
/// data carries for a key of that algorithm.
fn derived_type(name: &str) -> Option<DerivedType> {
    let (key_algorithm, key_bytes, indicator) = match name {
        "TDES_2KEY" => ("TDES_2KEY", 16, [0x00, 0x00]),
        "TDES_3KEY" => ("TDES_3KEY", 24, [0x00, 0x01]),
        "AES_128" => ("AES_128", 16, [0x00, 0x02]),
        "AES_192" => ("AES_192", 24, [0x00, 0x03]),
        "AES_256" => ("AES_256", 32, [0x00, 0x04]),
        _ => return None,
    };
    Some(DerivedType {
        key_algorithm,
        key_bytes,
        indicator,
    })
}

/// The type of key `request` derives, or the reason it names none.
///
/// "This must be less than or equal to the strength of the BDK"
/// (`DukptEncryptionAttributes$DukptKeyDerivationType`): a derived key is never longer
/// than the key it is derived from. With no type named it is the strongest that rule
/// admits — the base derivation key's own algorithm for the AES scheme, and the
/// double-length Triple DES key that is the only key X9.24-1 derives for the other.
fn derived_key_type(
    request: &Derivation<'_>,
    scheme: Scheme,
) -> Result<DerivedType, DerivationRefusal> {
    let default = match scheme {
        Scheme::TripleDes => TRIPLE_DES_DERIVED_TYPE,
        Scheme::Aes => request.key_algorithm,
    };
    let named = request.derivation_type.unwrap_or(default);
    let (Some(derived), Some(base)) = (derived_type(named), derived_type(request.key_algorithm))
    else {
        return Err(DerivationRefusal::Invalid(format!(
            "DukptKeyDerivationType {named} is not valid for DUKPT key derivation."
        )));
    };
    let admitted = match scheme {
        // X9.24-1 derives a double-length Triple DES key at every step, so that is the
        // one type a Triple DES base derivation key derives.
        Scheme::TripleDes => named == TRIPLE_DES_DERIVED_TYPE,
        Scheme::Aes => derived.key_bytes <= base.key_bytes,
    };
    if !admitted {
        return Err(DerivationRefusal::Invalid(format!(
            "DukptKeyDerivationType {named} is not valid for KeyAlgorithm {}.",
            request.key_algorithm
        )));
    }
    Ok(derived)
}

/// How many bytes one derivation block is in the Triple DES scheme: the cipher's block,
/// which is the length of the serial number register and of each half of a key.
const BLOCK_BYTES: usize = 8;

/// How many bytes the double-length Triple DES key every X9.24-1 derivation produces
/// carries.
const TRIPLE_DES_KEY_BYTES: usize = 2 * BLOCK_BYTES;

/// The `KeyAlgorithm` a key the Triple DES scheme derives is keyed as: that scheme
/// derives double-length Triple DES keys and no others.
const TRIPLE_DES_DERIVED_TYPE: &str = "TDES_2KEY";

/// How many bits of the Triple DES scheme's key serial number are its transaction
/// counter: the rightmost 21, the remainder being the initial serial number.
const TRIPLE_DES_COUNTER_BITS: u32 = 21;

/// How many bytes that serial number carries (X9.24-1). The modelled
/// `HexLength16Or20Or24` admits other lengths because one member carries the serial
/// numbers of BOTH standards.
const TRIPLE_DES_SERIAL_NUMBER_BYTES: usize = 10;

/// The mask the non-reversible key generation process applies to the key register to
/// derive the other half of the next key (X9.24-1).
const NON_REVERSIBLE_MASK: [u8; TRIPLE_DES_KEY_BYTES] = [
    0xC0, 0xC0, 0xC0, 0xC0, 0x00, 0x00, 0x00, 0x00, 0xC0, 0xC0, 0xC0, 0xC0, 0x00, 0x00, 0x00, 0x00,
];

/// The variant of the current transaction key that names a key encrypting data the
/// terminal SENDS (X9.24-1's data-encryption request variant).
const DATA_ENCRYPTION_REQUEST_VARIANT: [u8; TRIPLE_DES_KEY_BYTES] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00,
];

/// The variant that names a key encrypting data the terminal RECEIVES (X9.24-1's
/// data-encryption response variant).
const DATA_ENCRYPTION_RESPONSE_VARIANT: [u8; TRIPLE_DES_KEY_BYTES] = [
    0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00,
];

/// The Triple DES scheme's data-encryption working key, from the initial key on.
fn triple_des_working_key(
    initial: &[u8],
    serial_number: &[u8],
    variant: &str,
) -> Result<WorkingKey, DerivationRefusal> {
    let variant_mask = match variant {
        VARIANT_REQUEST | VARIANT_BIDIRECTIONAL => &DATA_ENCRYPTION_REQUEST_VARIANT,
        VARIANT_RESPONSE => &DATA_ENCRYPTION_RESPONSE_VARIANT,
        other => return Err(unknown_variant(other)),
    };
    let transaction = transaction_key(initial, serial_number)?;
    let material = data_encryption_variant(&transaction, variant_mask)?;
    Ok(WorkingKey {
        key_algorithm: TRIPLE_DES_DERIVED_TYPE,
        material,
    })
}

/// The current transaction key: the initial key walked forward over the set bits of the
/// serial number's transaction counter, one non-reversible derivation per bit.
fn transaction_key(initial: &[u8], serial_number: &[u8]) -> Result<Vec<u8>, DerivationRefusal> {
    let mut trailing = [0u8; BLOCK_BYTES];
    trailing.copy_from_slice(&serial_number[serial_number.len() - BLOCK_BYTES..]);
    let trailing = u64::from_be_bytes(trailing);
    let counter_mask = (1u64 << TRIPLE_DES_COUNTER_BITS) - 1;
    let counter = trailing & counter_mask;
    let mut register = trailing & !counter_mask;
    let mut key = initial.to_vec();
    for bit in (0..TRIPLE_DES_COUNTER_BITS).rev() {
        let reached = 1u64 << bit;
        if counter & reached != 0 {
            register |= reached;
            key = non_reversible_key(&key, &register.to_be_bytes())?;
        }
    }
    Ok(key)
}

/// The non-reversible key generation process of X9.24-1: the key register's right half
/// is derived from the register as it stands and its left half from the register with
/// [`NON_REVERSIBLE_MASK`] applied, each by XORing `data` with that register's right
/// half, encrypting under its left half, and XORing the right half back in.
fn non_reversible_key(key: &[u8], data: &[u8; BLOCK_BYTES]) -> Result<Vec<u8>, DerivationRefusal> {
    let right = non_reversible_half(key, data)?;
    let mut derived = non_reversible_half(&masked(key), data)?;
    derived.extend(right);
    Ok(derived)
}

/// One half of a non-reversibly generated key, from the key register `key`.
fn non_reversible_half(
    key: &[u8],
    data: &[u8; BLOCK_BYTES],
) -> Result<Vec<u8>, DerivationRefusal> {
    if key.len() < TRIPLE_DES_KEY_BYTES {
        return Err(unusable_material(TRIPLE_DES_DERIVED_TYPE));
    }
    let (left, right) = (&key[..BLOCK_BYTES], &key[BLOCK_BYTES..TRIPLE_DES_KEY_BYTES]);
    let Some(cipher) = single_des(left) else {
        return Err(unusable_material(TRIPLE_DES_DERIVED_TYPE));
    };
    let mut half: Vec<u8> = data
        .iter()
        .zip(right.iter())
        .map(|(data, key)| data ^ key)
        .collect();
    cipher.encrypt_block(&mut half);
    for (byte, key) in half.iter_mut().zip(right.iter()) {
        *byte ^= key;
    }
    Ok(half)
}

/// The data-encryption key the current transaction key names: the transaction key XORed
/// with the direction's variant, then encrypted under itself half by half, which is what
/// makes a data key rather than one of the PIN or MAC variants (X9.24-1).
fn data_encryption_variant(
    transaction: &[u8],
    variant: &[u8; TRIPLE_DES_KEY_BYTES],
) -> Result<Vec<u8>, DerivationRefusal> {
    if transaction.len() < TRIPLE_DES_KEY_BYTES {
        return Err(unusable_material(TRIPLE_DES_DERIVED_TYPE));
    }
    let variant_key: Vec<u8> = transaction
        .iter()
        .zip(variant.iter())
        .map(|(key, mask)| key ^ mask)
        .collect();
    let Some(cipher) = BlockCipher::new(TRIPLE_DES_DERIVED_TYPE, &variant_key) else {
        return Err(unusable_material(TRIPLE_DES_DERIVED_TYPE));
    };
    let mut key = Vec::with_capacity(TRIPLE_DES_KEY_BYTES);
    for half in variant_key.chunks(BLOCK_BYTES) {
        let mut block = half.to_vec();
        cipher.encrypt_block(&mut block);
        key.extend(block);
    }
    Ok(key)
}

/// `key` with [`NON_REVERSIBLE_MASK`] applied to the double-length register the standard
/// defines the mask over.
fn masked(key: &[u8]) -> Vec<u8> {
    key.iter()
        .enumerate()
        .map(|(index, byte)| byte ^ NON_REVERSIBLE_MASK.get(index).copied().unwrap_or(0))
        .collect()
}

/// How many bytes the AES scheme's key serial number carries: the base derivation key
/// id, the device's derivation id and the transaction counter, four bytes each
/// (X9.24-3).
const AES_SERIAL_NUMBER_BYTES: usize = 12;

/// How many bytes of that serial number are the initial key id: the base derivation key
/// id and the derivation id together.
const AES_INITIAL_KEY_ID_BYTES: usize = 8;

/// How many bytes the AES scheme's transaction counter carries.
const AES_COUNTER_BYTES: usize = 4;

/// How many bits that counter carries.
const AES_COUNTER_BITS: u32 = 32;

/// How many bytes one block of X9.24-3 derivation data is: the AES block, which is what
/// the derivation encrypts.
const DERIVATION_DATA_BYTES: usize = 16;

/// The version byte X9.24-3's derivation data opens with.
const DERIVATION_DATA_VERSION: u8 = 0x01;

/// The derivation data's key usage indicator for deriving the next derivation key of
/// the transaction walk.
const USAGE_KEY_DERIVATION: [u8; 2] = [0x80, 0x00];

/// The indicator for a working key that ENCRYPTS data.
const USAGE_DATA_ENCRYPTION_ENCRYPT: [u8; 2] = [0x30, 0x00];

/// The indicator for a working key that DECRYPTS data.
const USAGE_DATA_ENCRYPTION_DECRYPT: [u8; 2] = [0x30, 0x01];

/// The indicator for a working key used in BOTH directions.
const USAGE_DATA_ENCRYPTION_BOTH: [u8; 2] = [0x30, 0x02];

/// The AES scheme's data-encryption working key, from the initial key on (X9.24-3).
fn aes_working_key(
    request: &Derivation<'_>,
    initial: &[u8],
    serial_number: &[u8],
    derived: &DerivedType,
    variant: &str,
) -> Result<WorkingKey, DerivationRefusal> {
    let usage = match variant {
        VARIANT_REQUEST => USAGE_DATA_ENCRYPTION_ENCRYPT,
        VARIANT_RESPONSE => USAGE_DATA_ENCRYPTION_DECRYPT,
        VARIANT_BIDIRECTIONAL => USAGE_DATA_ENCRYPTION_BOTH,
        other => return Err(unknown_variant(other)),
    };
    let Some(base) = derived_type(request.key_algorithm) else {
        return Err(unusable_material(request.key_algorithm));
    };
    let mut counter = [0u8; AES_COUNTER_BYTES];
    counter.copy_from_slice(&serial_number[AES_INITIAL_KEY_ID_BYTES..]);
    let counter = u32::from_be_bytes(counter);
    // The device's own half of the initial key id, which every derivation of its
    // transaction walk carries beside the counter it reached.
    let device = &serial_number[AES_COUNTER_BYTES..AES_INITIAL_KEY_ID_BYTES];

    let mut key = initial.to_vec();
    let mut reached = 0u32;
    for bit in (0..AES_COUNTER_BITS).rev() {
        let step = 1u32 << bit;
        if counter & step != 0 {
            reached |= step;
            key = derive_key(
                &base,
                &key,
                USAGE_KEY_DERIVATION,
                &base,
                &transaction_data(device, reached),
            )?;
        }
    }
    let material = derive_key(
        &base,
        &key,
        usage,
        derived,
        &transaction_data(device, counter),
    )?;
    Ok(WorkingKey {
        key_algorithm: derived.key_algorithm,
        material,
    })
}

/// The eight bytes of derivation data a transaction's derivation carries: the device's
/// half of the initial key id and the transaction counter reached (X9.24-3).
fn transaction_data(device: &[u8], counter: u32) -> [u8; AES_INITIAL_KEY_ID_BYTES] {
    let mut data = [0u8; AES_INITIAL_KEY_ID_BYTES];
    let split = AES_INITIAL_KEY_ID_BYTES - AES_COUNTER_BYTES;
    let len = split.min(device.len());
    data[..len].copy_from_slice(&device[..len]);
    data[split..].copy_from_slice(&counter.to_be_bytes());
    data
}

/// X9.24-3's derive-key function: the derivation data — its version, the block counter,
/// the key usage indicator, the derived key's algorithm indicator, its length in BITS
/// and the eight bytes the derivation is for — encrypted under `derivation_key`, one
/// block per sixteen bytes of key the derivation produces.
fn derive_key(
    derivation_key_type: &DerivedType,
    derivation_key: &[u8],
    usage: [u8; 2],
    derived: &DerivedType,
    data: &[u8; AES_INITIAL_KEY_ID_BYTES],
) -> Result<Vec<u8>, DerivationRefusal> {
    let Some(cipher) = BlockCipher::new(derivation_key_type.key_algorithm, derivation_key)
        .filter(|cipher| cipher.block_len() == DERIVATION_DATA_BYTES)
    else {
        return Err(unusable_material(derivation_key_type.key_algorithm));
    };
    let Ok(bits) = u16::try_from(derived.key_bytes * 8) else {
        return Err(unusable_material(derived.key_algorithm));
    };
    let blocks = derived.key_bytes.div_ceil(DERIVATION_DATA_BYTES);
    let mut material = Vec::with_capacity(blocks * DERIVATION_DATA_BYTES);
    for block_counter in 1..=blocks {
        let Ok(block_counter) = u8::try_from(block_counter) else {
            return Err(unusable_material(derived.key_algorithm));
        };
        let mut block = [0u8; DERIVATION_DATA_BYTES];
        block[0] = DERIVATION_DATA_VERSION;
        block[1] = block_counter;
        block[2..4].copy_from_slice(&usage);
        block[4..6].copy_from_slice(&derived.indicator);
        block[6..8].copy_from_slice(&bits.to_be_bytes());
        block[8..].copy_from_slice(data);
        cipher.encrypt_block(&mut block);
        material.extend_from_slice(&block);
    }
    material.truncate(derived.key_bytes);
    Ok(material)
}

/// The refusal of a `DukptKeyVariant` neither scheme defines a data-encryption key for.
fn unknown_variant(variant: &str) -> DerivationRefusal {
    DerivationRefusal::Invalid(format!(
        "DukptKeyVariant {variant} is not valid for DUKPT key derivation."
    ))
}

/// The key material of a key the derivation ADMITS turned out not to key it: the length
/// or the form of the material this service holds, which is this emulator's own
/// invariant rather than the caller's request.
fn unusable_material(key_algorithm: &str) -> DerivationRefusal {
    DerivationRefusal::Unusable(format!(
        "The working material held for this {key_algorithm} key does not key the DUKPT \
         derivation."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base derivation key's working material of `bytes` bytes, as fixture material:
    /// the derivations under test are functions of these bytes and the serial number,
    /// so any fixed material pins them.
    fn material(bytes: usize) -> Vec<u8> {
        (0..bytes)
            .map(|index| u8::try_from((index * 17 + 5) % 256).unwrap_or_default())
            .collect()
    }

    /// How many bytes of material a key of `key_algorithm` carries.
    fn material_bytes(key_algorithm: &str) -> usize {
        derived_type(key_algorithm)
            .map(|derived| derived.key_bytes)
            .unwrap_or(32)
    }

    /// The derivation of one request, with everything it takes.
    fn derive(
        key_algorithm: &str,
        key_serial_number: &str,
        derivation_type: Option<&str>,
        key_variant: Option<&str>,
    ) -> Result<WorkingKey, DerivationRefusal> {
        data_encryption_key(&Derivation {
            key_algorithm,
            material: &material(material_bytes(key_algorithm)),
            key_serial_number,
            derivation_type,
            key_variant,
        })
    }

    /// One row of the table below: the case, the base derivation key's algorithm, the
    /// key serial number, the `DukptKeyDerivationType` and the `DukptKeyVariant`.
    type DerivationCase = (
        &'static str,
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
    );

    /// RULE: a derivation is a function of the base derivation key, the serial number
    /// and the attributes alone, so the same request always derives the same key, of the
    /// length its own derived type names.
    #[test]
    fn a_derived_key_is_a_function_of_the_request_alone() {
        // (case, key algorithm, serial number, derivation type, variant)
        let rows: &[DerivationCase] = &[
            (
                "the Triple DES scheme",
                "TDES_2KEY",
                "FFFF9876543210E00001",
                None,
                None,
            ),
            (
                "the Triple DES scheme, request variant",
                "TDES_2KEY",
                "FFFF9876543210E00001",
                Some("TDES_2KEY"),
                Some("REQUEST"),
            ),
            (
                "the AES scheme",
                "AES_128",
                "123456789012345600000001",
                None,
                None,
            ),
            (
                "an AES key deriving a Triple DES working key",
                "AES_256",
                "1234567890123456000000FF",
                Some("TDES_2KEY"),
                Some("RESPONSE"),
            ),
            (
                "an AES key deriving a triple-length Triple DES working key",
                "AES_256",
                "1234567890123456000000FF",
                Some("TDES_3KEY"),
                None,
            ),
        ];
        for (case, key_algorithm, serial_number, derivation_type, variant) in rows {
            let first = derive(key_algorithm, serial_number, *derivation_type, *variant)
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: expected a derived key, got {refusal:?}")
                });
            let again = derive(key_algorithm, serial_number, *derivation_type, *variant)
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: expected a derived key, got {refusal:?}")
                });
            assert_eq!(
                first.material, again.material,
                "case {case}: one request must derive one key"
            );
            assert_eq!(
                first.material.len(),
                material_bytes(first.key_algorithm),
                "case {case}: the derived key must be as long as its algorithm says, \
                 got {} bytes of {}",
                first.material.len(),
                first.key_algorithm
            );
            assert_ne!(
                first.material,
                material(first.material.len()),
                "case {case}: the derived key must not be the base derivation key's own \
                 material"
            );
        }
    }

    /// RULE: the transaction counter walks the key forward, so two counters under one
    /// device derive different keys — and so do the two directions.
    #[test]
    fn a_different_transaction_or_direction_derives_a_different_key() {
        // (case, key algorithm, one serial number, another serial number)
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "the Triple DES scheme",
                "TDES_2KEY",
                "FFFF9876543210E00001",
                "FFFF9876543210E00002",
            ),
            (
                "the AES scheme",
                "AES_128",
                "123456789012345600000001",
                "123456789012345600000002",
            ),
        ];
        for (case, key_algorithm, one, another) in rows {
            let first = derive(key_algorithm, one, None, None)
                .unwrap_or_else(|refusal| panic!("case {case}: {refusal:?}"));
            let second = derive(key_algorithm, another, None, None)
                .unwrap_or_else(|refusal| panic!("case {case}: {refusal:?}"));
            assert_ne!(
                first.material, second.material,
                "case {case}: two transaction counters must derive two keys"
            );
            let request = derive(key_algorithm, one, None, Some(VARIANT_REQUEST))
                .unwrap_or_else(|refusal| panic!("case {case}: {refusal:?}"));
            let response = derive(key_algorithm, one, None, Some(VARIANT_RESPONSE))
                .unwrap_or_else(|refusal| panic!("case {case}: {refusal:?}"));
            assert_ne!(
                request.material, response.material,
                "case {case}: the two directions must derive two keys"
            );
        }
    }

    /// RULE: a request outside the scheme's own constraints is refused, naming the
    /// member, rather than derived under a definition the standard does not have.
    #[test]
    fn a_request_outside_the_scheme_is_refused() {
        // (case, key algorithm, serial number, derivation type)
        let rows: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "the AES serial number with a Triple DES key",
                "TDES_2KEY",
                "123456789012345600000001",
                None,
            ),
            (
                "a Triple DES serial number with an AES key",
                "AES_128",
                "FFFF9876543210E00001",
                None,
            ),
            (
                "the shortest serial number the model admits, which neither scheme reads",
                "TDES_2KEY",
                "FFFF9876543210E0",
                None,
            ),
            (
                "an AES derived type from a Triple DES key",
                "TDES_2KEY",
                "FFFF9876543210E00001",
                Some("AES_128"),
            ),
            (
                "a derived type stronger than the key it derives from",
                "AES_128",
                "123456789012345600000001",
                Some("AES_256"),
            ),
            (
                "a key algorithm that names no block cipher",
                "HMAC_SHA256",
                "FFFF9876543210E00001",
                None,
            ),
            (
                "a triple-length Triple DES key, which X9.24-1 derives nothing under",
                "TDES_3KEY",
                "FFFF9876543210E00001",
                None,
            ),
        ];
        for (case, key_algorithm, serial_number, derivation_type) in rows {
            let refusal = derive(key_algorithm, serial_number, *derivation_type, None);
            assert!(
                matches!(refusal, Err(DerivationRefusal::Invalid(_))),
                "case {case}: expected a refusal naming the member, got {:?}",
                refusal.map(|key| key.key_algorithm)
            );
        }
    }
}
