// SPDX-License-Identifier: CC0-1.0

//! The Secure Remote Password exchange Amazon Cognito runs, in ONE place: the
//! group it runs over, the key schedule it derives a session key with, and the
//! three steps both of its legs share.
//!
//! Two sign-in surfaces of this service run this exchange — the device's proof
//! of possession ([`super::device_auth`]) and the user's own secure-remote-password
//! sign-in ([`super::user_srp_auth`]) — and they differ only in what the
//! verifier comes from and which two names the claim is signed over. Everything
//! else has to be bit-identical on both, since a client library computes its own
//! half from the same definition: so the group, the padding, the hash, the HKDF
//! expansion and the claim check live HERE and each leg calls them, rather than
//! each leg carrying its own copy to drift from the other.
//!
//! SRP-6a over the 3072-bit prime of RFC 3526 — the constant comes from the
//! library that defines the SRP groups rather than from a prime written out here
//! — with Amazon Cognito's own generator and key schedule: the server's public
//! value is `B = k·v + g^b (mod N)`, the session key is the HKDF-SHA256
//! expansion of `S = (A · v^u)^b (mod N)` salted with the scrambling parameter
//! `u = H(PAD(A) | PAD(B))`, and the claim is the HMAC-SHA256 of two names, the
//! secret block the challenge served and the instant the caller signed.

use super::*;
use ::num_bigint::BigUint;

/// The private exponent of one exchange: 256 bits, the floor RFC 5054 §2.5.4
/// states for the server's private value.
const SERVER_SECRET_BYTES: usize = 32;

/// The block a password claim signs, minted per exchange.
const SECRET_BLOCK_BYTES: usize = 64;

/// The size of the key the claim is signed under, which is what this key
/// schedule expands its shared secret to.
const SESSION_KEY_BYTES: usize = 16;

/// The label this key schedule expands the shared secret under.
const DERIVED_KEY_LABEL: &[u8] = b"Caldera Derived Key";

/// The server's half of one exchange, as the leg that serves it stores it: the
/// three values the session key is derived from and the block the claim signs.
///
/// Bytes rather than numbers, in the padded spelling both sides of the exchange
/// hash ([`padded`]), so what is stored is exactly what was served.
#[derive(Clone)]
pub(super) struct SrpServerHalf {
    /// The private exponent the served public value was computed from.
    pub(super) server_secret: Vec<u8>,
    /// The caller's public value, as the `SRP_A` it presented.
    pub(super) client_public: Vec<u8>,
    /// The public value served as `SRP_B`.
    pub(super) server_public: Vec<u8>,
    /// The bytes served as `SECRET_BLOCK`, which the claim signs.
    pub(super) secret_block: Vec<u8>,
}

/// Stated rather than derived: the private exponent and the secret block of one
/// exchange are secrets of the sign-in it belongs to, so nothing renders them.
impl ::std::fmt::Debug for SrpServerHalf {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("SrpServerHalf")
            .finish_non_exhaustive()
    }
}

/// Serve one exchange over the verifier `verifier`, against the public value
/// `client_srp_a` the caller presented.
///
/// The server's public value is `B = k·v + g^b (mod N)` over the group below,
/// with `b` a private exponent minted for THIS exchange, so the public value a
/// caller is served carries no credential and two exchanges of one holder share
/// nothing.
///
/// `None` when the presented value is not a usable public value — one that is
/// not hexadecimal, that is spelled in more digits than the group's own prime,
/// or that is zero modulo that prime, which RFC 5054 §2.5.4 aborts the exchange
/// on — or when the randomness this needs could not be read.
pub(super) fn serve(verifier: &BigUint, client_srp_a: &str) -> Option<SrpServerHalf> {
    let prime = group_prime();
    // A public value of this group is a number modulo its prime, so one spelled
    // in more digits than the prime itself is no public value of it — and
    // reading one at all is work whose size the caller would otherwise choose.
    if client_srp_a.len() > padded(prime).len().saturating_mul(2) {
        return None;
    }
    let client_public = BigUint::parse_bytes(client_srp_a.as_bytes(), 16)?;
    if &client_public % prime == zero() {
        return None;
    }
    let server_secret = random_bytes::<SERVER_SECRET_BYTES>()?;
    let secret_block = random_bytes::<SECRET_BLOCK_BYTES>()?;
    let exponent = BigUint::from_bytes_be(&server_secret);
    let server_public = (multiplier() * verifier + generator().modpow(&exponent, prime)) % prime;
    if server_public == zero() {
        return None;
    }
    Some(SrpServerHalf {
        server_secret: server_secret.to_vec(),
        client_public: padded(&client_public),
        server_public: padded(&server_public),
        secret_block: secret_block.to_vec(),
    })
}

impl SrpServerHalf {
    /// The `ChallengeParameters` entries this half serves, added to
    /// `parameters`: the public value and the salt as hexadecimal, and the
    /// secret block as base64, which is how a client library of this exchange
    /// reads each of them.
    pub(super) fn report(&self, parameters: &mut HashMap<String, String>, salt: &[u8]) {
        parameters.insert(
            SRP_B_PARAMETER.to_string(),
            ::hex::encode(&self.server_public),
        );
        parameters.insert(SALT_PARAMETER.to_string(), ::hex::encode(salt));
        parameters.insert(
            SECRET_BLOCK_PARAMETER.to_string(),
            encode_base64(&self.secret_block),
        );
    }

    /// The key this exchange's claim is signed under, for the verifier
    /// `verifier` of the credential it checks.
    ///
    /// The scrambling parameter is `u = H(PAD(A) | PAD(B))` and the shared
    /// secret is `S = (A · v^u)^b (mod N)`, which is the same secret the caller
    /// computes from its own private exponent and its own credential; the key is
    /// the first [`SESSION_KEY_BYTES`] of the HKDF-SHA256 expansion of that
    /// secret, salted with the scrambling parameter and labelled
    /// [`DERIVED_KEY_LABEL`].
    fn session_key(&self, verifier: &BigUint) -> Option<[u8; SESSION_KEY_BYTES]> {
        let prime = group_prime();
        let client_public = BigUint::from_bytes_be(&self.client_public);
        let server_public = BigUint::from_bytes_be(&self.server_public);
        let exponent = BigUint::from_bytes_be(&self.server_secret);
        let scrambler = BigUint::from_bytes_be(&digest(&[
            &padded(&client_public),
            &padded(&server_public),
        ]));
        if scrambler == zero() {
            return None;
        }
        let shared =
            (client_public * verifier.modpow(&scrambler, prime) % prime).modpow(&exponent, prime);
        let mut session_key = [0_u8; SESSION_KEY_BYTES];
        ::hkdf::Hkdf::<::sha2::Sha256>::new(Some(&padded(&scrambler)), &padded(&shared))
            .expand(DERIVED_KEY_LABEL, &mut session_key)
            .ok()?;
        Some(session_key)
    }

    /// Whether the claim `presented_signature`, made at `timestamp` over
    /// `presented_secret_block`, proves that the caller holds the credential
    /// `verifier` was derived from.
    ///
    /// The caller echoes the block the challenge served it, so a response
    /// carrying any other block is answering an exchange this one did not serve;
    /// the signature itself is checked against the HMAC this exchange's own
    /// session key computes over `signed_names`, the served block and the
    /// instant the claim was signed at, compared in constant time by the library
    /// that computes it. `signed_names` is what each leg signs its claim over —
    /// the device group key and the device key, or the pool name and the
    /// username — which is the one thing the two legs differ in here.
    pub(super) fn claim_verifies(
        &self,
        verifier: &BigUint,
        signed_names: [&str; 2],
        presented_secret_block: &str,
        timestamp: &str,
        presented_signature: &str,
    ) -> bool {
        if presented_secret_block != encode_base64(&self.secret_block) {
            return false;
        }
        let Some(signature) = decode_base64(presented_signature) else {
            return false;
        };
        let Some(session_key) = self.session_key(verifier) else {
            return false;
        };
        let mut message = Vec::new();
        for name in signed_names {
            message.extend_from_slice(name.as_bytes());
        }
        message.extend_from_slice(&self.secret_block);
        message.extend_from_slice(timestamp.as_bytes());
        ::ring::hmac::verify(
            &::ring::hmac::Key::new(::ring::hmac::HMAC_SHA256, &session_key),
            &message,
            &signature,
        )
        .is_ok()
    }
}

/// The `ChallengeParameters` entry carrying the server's public value of the
/// exchange.
pub(super) const SRP_B_PARAMETER: &str = "SRP_B";

/// The `ChallengeParameters` entry carrying the salt the checked verifier was
/// derived with.
pub(super) const SALT_PARAMETER: &str = "SALT";

/// The `ChallengeParameters` entry carrying the block the password claim signs.
pub(super) const SECRET_BLOCK_PARAMETER: &str = "SECRET_BLOCK";

/// The prime of the group the exchange runs over: the 3072-bit group of RFC
/// 3526, read from the library that defines the SRP groups (RFC 5054 takes its
/// 3072-bit group from there) so the constant has one source rather than a prime
/// written out here.
pub(super) fn group_prime() -> &'static BigUint {
    &::srp::groups::G_3072.n
}

/// The generator Amazon Cognito's SRP runs over that prime: 2, which is the
/// generator every client library of this exchange computes its public value
/// with, rather than the one RFC 5054 pairs with the 3072-bit prime.
pub(super) fn generator() -> BigUint {
    BigUint::from(2_u32)
}

/// The multiplier parameter of SRP-6a over that group: `k = H(PAD(N) | PAD(g))`.
fn multiplier() -> BigUint {
    BigUint::from_bytes_be(&digest(&[&padded(group_prime()), &padded(&generator())]))
}

/// Zero, the value a public value of the exchange may not be modulo the group's
/// prime.
pub(super) fn zero() -> BigUint {
    BigUint::from(0_u32)
}

/// The SHA-256 digest of `parts` concatenated, the hash of this key schedule.
pub(super) fn digest(parts: &[&[u8]]) -> [u8; 32] {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// `value` as the big-endian bytes this key schedule hashes and reports it as:
/// its shortest representation, with a leading zero byte where the first byte's
/// top bit is set, so every value is read as a positive number by both sides of
/// the exchange.
pub(super) fn padded(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.first().is_some_and(|first| first & 0x80 != 0) {
        let mut padded = Vec::with_capacity(bytes.len().saturating_add(1));
        padded.push(0);
        padded.extend_from_slice(&bytes);
        return padded;
    }
    bytes
}

/// `N` bytes of the randomness this exchange mints secrets from
/// (`ring::rand::SystemRandom`), or `None` when it could not be read.
pub(super) fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    ::ring::rand::SecureRandom::fill(&::ring::rand::SystemRandom::new(), &mut bytes).ok()?;
    Some(bytes)
}

/// `bytes` in base64, the encoding the served secret block travels in.
pub(super) fn encode_base64(bytes: &[u8]) -> String {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The bytes `text` carries as base64, padded or not: a value a caller
/// registered or signed is read whichever way its library wrote the padding.
pub(super) fn decode_base64(text: &str) -> Option<Vec<u8>> {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()
        .or_else(|| {
            ::base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(text)
                .ok()
        })
}
