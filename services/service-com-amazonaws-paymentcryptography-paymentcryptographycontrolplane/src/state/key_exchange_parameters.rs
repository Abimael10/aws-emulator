// SPDX-License-Identifier: CC0-1.0

//! The parameters this service hands out BEFORE a key is exchanged with it: the token
//! that names one set, the certified key pair whose certificate the caller is given, and
//! how long both stay usable.
//!
//! One concern, both directions of the exchange. `GetParametersForExport` hands out a
//! SIGNING key — the key that signs the TR-34 payload a later `ExportKey` builds — and
//! `GetParametersForImport` a WRAPPING key, the key a caller encrypts material under for
//! a later `ImportKey`. Everything else about the two is the same rule, so it is stated
//! here once: each call mints a set, names it with a token carrying that direction's own
//! prefix, keeps it for [`KEY_EXCHANGE_PARAMETERS_VALIDITY`], and answers a stored set
//! again instead of minting when the caller asks for the last generated token and one
//! with enough validity left exists for the same key material type and algorithm.
//!
//! The key pair of a set is REAL ([`super::mint_certified_rsa_key`]): the certificate a
//! caller is handed is an X.509 certificate of a key pair this store generated, signed by
//! an authority whose own certificate comes with it, and the private half stays here —
//! which is what makes a later exchange against that certificate possible at all.

use std::time::{Duration, SystemTime};

use super::key_pair::{key_pair_algorithm, KeyPairAlgorithm};
use super::{
    splitmix64, ComAmazonawsPaymentcryptographyPaymentcryptographycontrolplaneState as KeyStore,
    KeyMaterial, MintedCertifiedKey, MAX_KEY_ID_ATTEMPTS,
};

/// The prefix every export token carries, from the modelled `ExportTokenId` pattern
/// `^(export-token-[0-9a-zA-Z]{16,64})?$`.
pub const EXPORT_TOKEN_PREFIX: &str = "export-token-";

/// The prefix every import token carries, from the modelled `ImportTokenId` pattern
/// `^(import-token-[0-9a-zA-Z]{16,64})?$`.
pub const IMPORT_TOKEN_PREFIX: &str = "import-token-";

/// Length of the id segment of a generated token: the minimum both of those patterns
/// admit.
pub const TOKEN_ID_LEN: usize = 16;

/// How long a parameter set and its certificate stay valid.
///
/// Both operations document the same period: "The export token expires in 30 days"
/// (`GetParametersForExport`) and "The import token expires in 30 days"
/// (`GetParametersForImport`).
pub const KEY_EXCHANGE_PARAMETERS_VALIDITY: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How much validity a stored set must have left for `ReuseLastGeneratedToken` to answer
/// with it instead of minting a new one.
///
/// From that member's own documentation, worded the same way on both operations: "If set
/// to true and a valid export token exists for the same key material type and signing key
/// algorithm with at least 7 days of remaining validity, the existing token and signing
/// key certificate are returned."
pub const TOKEN_REUSE_REMAINING: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The most parameter sets this emulator will hold per direction at once.
///
/// Emulator resource guard, NOT an AWS quota: every call that does not reuse mints a set
/// and the store keeps it until it expires, so the store needs a bound. Sets the clock
/// has already expired do not count against it.
pub const MAX_KEY_EXCHANGE_PARAMETERS: usize = 100;

/// `RSA_2048`: the one `KeyAlgorithm` the TR-34 key exchange uses.
///
/// It is "the only signing key algorithm allowed" (`SigningKeyAlgorithm`'s own
/// documentation) and, on the import side, "RSA_2048 is the allowed algorithm for TR-34
/// WrappedKeyBlock import" (`WrappingKeyAlgorithm`'s own documentation).
pub const TR34_SIGNING_KEY_ALGORITHM: &str = "RSA_2048";

/// The `KeyMaterialType` of a TR-34 wrapped key block, from
/// `smithy_query(query="shape",shape="KeyMaterialType")`.
pub const KEY_MATERIAL_TYPE_TR34_KEY_BLOCK: &str = "TR34_KEY_BLOCK";

/// The `KeyAlgorithm` values a wrapping key certificate is issued for when the material
/// is not a TR-34 wrapped key block: "RSA_2048, RSA_3072, RSA_4096 are the allowed
/// algorithms for RSA WrappedKeyCryptogram import" (`WrappingKeyAlgorithm`'s own
/// documentation), named one by one rather than matched on a name prefix.
pub const RSA_WRAPPING_KEY_ALGORITHMS: &[&str] = &["RSA_2048", "RSA_3072", "RSA_4096"];

/// The `KeyAlgorithm` values `GetParametersForImport` issues a wrapping key certificate
/// for, given the `KeyMaterialType` the caller is preparing to import.
///
/// `WrappingKeyAlgorithm`'s own documentation states the rule: "At this time, RSA_2048 is
/// the allowed algorithm for TR-34 WrappedKeyBlock import. Additionally, RSA_2048,
/// RSA_3072, RSA_4096 are the allowed algorithms for RSA WrappedKeyCryptogram import." A
/// wrapping key is an RSA key in either case — the material a caller wraps is encrypted
/// under the public half of the key whose certificate this operation hands out — so a
/// material type that sentence does not name takes the RSA set rather than the narrower
/// TR-34 one.
pub fn wrapping_key_algorithms(key_material_type: &str) -> &'static [&'static str] {
    if key_material_type == KEY_MATERIAL_TYPE_TR34_KEY_BLOCK {
        return &[TR34_SIGNING_KEY_ALGORITHM];
    }
    RSA_WRAPPING_KEY_ALGORITHMS
}

/// The common name a minted TR-34 signing key certificate carries: the key it names is
/// the service's own signing key for a TR-34 key export.
const TR34_SIGNING_KEY_SUBJECT: &str = "Payment Cryptography TR-34 Signing Key";

/// The common name a minted wrapping key certificate carries: the key it names is the
/// service's own key for material a caller is about to import.
const WRAPPING_KEY_SUBJECT: &str = "Payment Cryptography Wrapping Key";

/// Which side of a key exchange a parameter set prepares.
///
/// The two sides differ in exactly two things — the prefix of the token that names a set,
/// and what the certified key is for, which its certificate's subject says — so they are
/// one store parameterised by this rather than two stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyExchangeDirection {
    /// `GetParametersForExport`: the set's key SIGNS the payload a later `ExportKey`
    /// builds.
    Export,
    /// `GetParametersForImport`: the set's key is the key a caller WRAPS material under
    /// for a later `ImportKey`.
    Import,
}

impl KeyExchangeDirection {
    /// The prefix this direction's modelled token id carries.
    pub fn token_prefix(self) -> &'static str {
        match self {
            Self::Export => EXPORT_TOKEN_PREFIX,
            Self::Import => IMPORT_TOKEN_PREFIX,
        }
    }

    /// The common name the certificate of this direction's key carries, which says what
    /// the key is for.
    fn certificate_subject(self) -> &'static str {
        match self {
            Self::Export => TR34_SIGNING_KEY_SUBJECT,
            Self::Import => WRAPPING_KEY_SUBJECT,
        }
    }
}

/// One minted parameter set: the token that names it, the certificate of the key pair it
/// hands out, and the instant both expire at.
#[derive(Debug, Clone)]
pub struct KeyExchangeParameters {
    /// The modelled token id naming this set; also its key in the store.
    pub token: String,
    /// Which side of the exchange this set prepares, which is where its token prefix and
    /// its certificate's subject come from.
    pub direction: KeyExchangeDirection,
    /// The `KeyMaterialType` the set was minted for, which with
    /// [`KeyExchangeParameters::key_algorithm`] is what a reuse request matches on.
    pub key_material_type: String,
    /// The `KeyAlgorithm` of the minted key, as `SigningKeyAlgorithm` and
    /// `WrappingKeyAlgorithm` report it.
    pub key_algorithm: String,
    /// The certificate of the minted key, as `SigningKeyCertificate` and
    /// `WrappingKeyCertificate` report it.
    pub certificate: String,
    /// The certificate of the root authority that signed it, as
    /// `SigningKeyCertificateChain` and `WrappingKeyCertificateChain` report it.
    pub certificate_chain: String,
    /// When this set was minted.
    pub created: SystemTime,
    /// When it expires, as `ParametersValidUntilTimestamp` reports it.
    pub valid_until: SystemTime,
    /// The private half of the minted key pair, PKCS#8 DER.
    ///
    /// Held because it is what the exchange this set prepares is defined in terms of: the
    /// key that signs the payload of a TR-34 export, and the key that unwraps material a
    /// caller encrypted under the certificate of an import. Never returned and never
    /// logged, and its `Debug` is redacted like a key's own material. Private to this
    /// module, so every set is minted through
    /// [`KeyStore::remember_key_exchange_parameters`] and no handler can store one whose
    /// certificate is not the certificate of this key.
    private_key_pkcs8: KeyMaterial,
}

impl KeyExchangeParameters {
    /// The private half of this set's key pair, PKCS#8 DER: the key whose public half the
    /// certificate carries.
    pub fn private_key_pkcs8(&self) -> &[u8] {
        self.private_key_pkcs8.bytes()
    }

    /// Whether this set is still valid at `now`.
    pub fn is_valid(&self, now: SystemTime) -> bool {
        now < self.valid_until
    }

    /// Whether this set still has `remaining` of its validity left at `now`, which is
    /// what `ReuseLastGeneratedToken` requires of the set it answers with.
    pub fn has_remaining(&self, now: SystemTime, remaining: Duration) -> bool {
        now.checked_add(remaining)
            .is_some_and(|deadline| deadline <= self.valid_until)
    }
}

/// Mint the key pair one parameter set hands out: an RSA key pair of the modulus
/// `key_algorithm` names, certified for what `direction` uses it for and valid from
/// `not_before` until `not_after`.
///
/// `None` when `key_algorithm` names no RSA key pair this service generates — every
/// caller judges the algorithm against [`wrapping_key_algorithms`] or
/// [`TR34_SIGNING_KEY_ALGORITHM`] first, so reaching that is this service's own failure —
/// or when the library could not produce or encode the pair.
///
/// Generating two RSA key pairs is hundreds of milliseconds of CPU, so a caller runs this
/// with no lock held and off the async runtime.
pub fn mint_key_exchange_key(
    direction: KeyExchangeDirection,
    key_algorithm: &str,
    not_before: SystemTime,
    not_after: SystemTime,
) -> Option<MintedCertifiedKey> {
    let KeyPairAlgorithm::Rsa(key_size) = key_pair_algorithm(key_algorithm)? else {
        return None;
    };
    super::mint_certified_rsa_key(
        direction.certificate_subject(),
        key_size,
        not_before,
        not_after,
    )
}

impl KeyStore {
    /// The parameter set `ReuseLastGeneratedToken` answers with, if this store holds one:
    /// the most recently minted set of `direction` for the same key material type and
    /// algorithm that still has [`TOKEN_REUSE_REMAINING`] of its validity left at `now`.
    pub fn reusable_key_exchange_parameters(
        &self,
        direction: KeyExchangeDirection,
        key_material_type: &str,
        key_algorithm: &str,
        now: SystemTime,
    ) -> Option<&KeyExchangeParameters> {
        self.key_exchange_parameters
            .values()
            .filter(|parameters| {
                parameters.direction == direction
                    && parameters.key_material_type == key_material_type
                    && parameters.key_algorithm == key_algorithm
                    && parameters.has_remaining(now, TOKEN_REUSE_REMAINING)
            })
            .max_by_key(|parameters| parameters.created)
    }

    /// How many parameter sets of `direction` are still valid at `now`.
    pub fn valid_key_exchange_parameters(
        &self,
        direction: KeyExchangeDirection,
        now: SystemTime,
    ) -> usize {
        self.key_exchange_parameters
            .values()
            .filter(|parameters| parameters.direction == direction && parameters.is_valid(now))
            .count()
    }

    /// Store a parameter set for `minted`, minted at `now` and expiring at `valid_until`,
    /// and answer it.
    ///
    /// The sets the clock has already expired are dropped here: an expired token names no
    /// parameters a later exchange could use, so it is not kept and does not count
    /// against [`MAX_KEY_EXCHANGE_PARAMETERS`].
    ///
    /// `None` when the store already holds that many unexpired sets of this direction or
    /// when no unused token is available — both judged under the SAME borrow that
    /// inserts, so concurrent calls cannot each pass a check and together push the store
    /// past the bound. Nothing is stored on a refusal.
    pub fn remember_key_exchange_parameters(
        &mut self,
        direction: KeyExchangeDirection,
        key_material_type: &str,
        key_algorithm: &str,
        minted: MintedCertifiedKey,
        now: SystemTime,
        valid_until: SystemTime,
    ) -> Option<KeyExchangeParameters> {
        self.key_exchange_parameters
            .retain(|_, parameters| parameters.is_valid(now));
        if self.valid_key_exchange_parameters(direction, now) >= MAX_KEY_EXCHANGE_PARAMETERS {
            return None;
        }
        let token = self.fresh_key_exchange_token(direction)?;
        let parameters = KeyExchangeParameters {
            token: token.clone(),
            direction,
            key_material_type: key_material_type.to_string(),
            key_algorithm: key_algorithm.to_string(),
            certificate: minted.certificate,
            certificate_chain: minted.certificate_chain,
            created: now,
            valid_until,
            private_key_pkcs8: minted.private_key_pkcs8,
        };
        self.key_exchange_parameters
            .insert(token, parameters.clone());
        Some(parameters)
    }

    /// A token of `direction` no stored parameter set is using, or `None` when every id
    /// this generator can produce is taken.
    ///
    /// Drawn from the same mixing stream as a key id, and checked against the store the
    /// same way, so no mint can ever replace a set a caller still holds the token for.
    fn fresh_key_exchange_token(&mut self, direction: KeyExchangeDirection) -> Option<String> {
        let prefix = direction.token_prefix();
        let mut seed = self.draw();
        for _ in 0..MAX_KEY_ID_ATTEMPTS {
            let candidate: String = (0..TOKEN_ID_LEN)
                .map(|_| {
                    let draw = splitmix64(&mut seed);
                    super::KEY_ID_ALPHABET[(draw % super::KEY_ID_ALPHABET.len() as u64) as usize]
                        as char
                })
                .collect();
            let token = format!("{prefix}{candidate}");
            if !self.key_exchange_parameters.contains_key(&token) {
                return Some(token);
            }
        }
        // Bounded fallback, for the same reason as the key id generator's: the
        // zero-padded decimals are also `[0-9a-zA-Z]{16}`, and there are always more of
        // them than there are stored sets.
        (0u64..=self.key_exchange_parameters.len() as u64)
            .map(|n| format!("{prefix}{n:016}"))
            .find(|token| !self.key_exchange_parameters.contains_key(token))
    }
}
