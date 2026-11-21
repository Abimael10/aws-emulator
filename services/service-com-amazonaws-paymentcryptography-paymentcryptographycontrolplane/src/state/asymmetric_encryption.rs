// SPDX-License-Identifier: CC0-1.0

//! Public-key encryption under an asymmetric key this store holds: the ciphertext its
//! PUBLIC half seals a plaintext to, and the plaintext only its PRIVATE half recovers.
//!
//! One concern of [`super`]'s store, in its own file, and the one place either half of a
//! key pair is used for encryption. It lives here, with the key pair, because the
//! private half never leaves this store: "unlike the private key of an asymmetric key,
//! which never leaves Amazon Web Services Payment Cryptography unencrypted"
//! (`GetPublicKeyCertificate`'s own documentation). A caller of either plane names a key
//! and a padding and is answered the bytes — the key material itself is never handed out.
//!
//! Nothing cryptographic is composed here: both schemes come from `aws-lc-rs`
//! (`aws_lc_rs::rsa`), under the very key pair this store generated when the key was
//! minted, so a ciphertext produced under the public half the certificate publishes is
//! one this key's private half opens and no other key does.

use super::{StoredKey, KEY_CLASS_ASYMMETRIC_PAIR};

/// Which RSA encryption scheme a computation under a key pair runs.
///
/// The caller names the scheme in its own model's terms (the data plane's
/// `PaddingType`, say) and maps it onto this set, so neither plane's modelled value set
/// leaks into this store and a scheme no library here performs is refused by the
/// mapping rather than by a guess made here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsaEncryptionScheme {
    /// RSAES-PKCS1-v1_5 (RFC 8017 section 7.2).
    Pkcs1,
    /// RSAES-OAEP with SHA-1, used as both the message digest and MGF1's hash (RFC 8017
    /// section 7.1).
    OaepSha1,
    /// RSAES-OAEP with SHA-256.
    OaepSha256,
    /// RSAES-OAEP with SHA-512.
    OaepSha512,
}

/// Why no computation under a key's asymmetric half could be performed.
#[derive(Debug, Clone)]
pub enum RsaRefusal {
    /// The key is not an RSA key pair, so no public-key encryption is defined for it.
    NotRsaKeyPair,
    /// The data is longer than the scheme admits under this key's modulus; the value is
    /// the longest it does admit, in bytes.
    TooLong(usize),
    /// The bytes are not a ciphertext this key's private half opens under that scheme:
    /// they were produced for another key, under another padding, or altered.
    NotThisCiphertext,
    /// The library implements the scheme and refused this particular operation, with the
    /// request already judged against every rule the scheme states — so it is a server
    /// fault rather than a refusal of the request.
    Failed,
}

impl StoredKey {
    /// The ciphertext of `plaintext` under the PUBLIC half of this key's pair, with
    /// `scheme`.
    ///
    /// The public half is COMPUTED from the private half this store holds, by the same
    /// library that implements the scheme, so what seals a plaintext here is the public
    /// key this service publishes for the key and nothing else opens the result.
    pub fn rsa_encrypt(
        &self,
        scheme: RsaEncryptionScheme,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, RsaRefusal> {
        let private_key = self.rsa_private_key()?;
        let longest = longest_plaintext(private_key.key_size_bytes(), scheme);
        if plaintext.len() > longest {
            return Err(RsaRefusal::TooLong(longest));
        }
        // An RSA ciphertext is exactly one modulus wide.
        let mut ciphertext = vec![0u8; private_key.key_size_bytes()];
        let len = match oaep_algorithm(scheme) {
            Some(algorithm) => ::aws_lc_rs::rsa::OaepPublicEncryptingKey::new(
                private_key.public_key(),
            )
            .map_err(|_| RsaRefusal::Failed)?
            .encrypt(algorithm, plaintext, &mut ciphertext, None)
            .map_err(|_| RsaRefusal::Failed)?
            .len(),
            None => ::aws_lc_rs::rsa::Pkcs1PublicEncryptingKey::new(private_key.public_key())
                .map_err(|_| RsaRefusal::Failed)?
                .encrypt(plaintext, &mut ciphertext)
                .map_err(|_| RsaRefusal::Failed)?
                .len(),
        };
        ciphertext.truncate(len);
        Ok(ciphertext)
    }

    /// The plaintext of `ciphertext`, which was encrypted under the PUBLIC half of this
    /// key's pair with `scheme`.
    ///
    /// Nothing about an RSA ciphertext says which key or padding produced it, so both
    /// come from the request: bytes that are not a ciphertext of this key under that
    /// padding are refused rather than answered with whatever they decrypt to.
    pub fn rsa_decrypt(
        &self,
        scheme: RsaEncryptionScheme,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, RsaRefusal> {
        let private_key = self.rsa_private_key()?;
        // A padded plaintext is shorter than the modulus the ciphertext occupies, so one
        // modulus of room always holds it.
        let mut plaintext = vec![0u8; private_key.key_size_bytes()];
        let len = match oaep_algorithm(scheme) {
            Some(algorithm) => ::aws_lc_rs::rsa::OaepPrivateDecryptingKey::new(private_key)
                .map_err(|_| RsaRefusal::Failed)?
                .decrypt(algorithm, ciphertext, &mut plaintext, None)
                .map_err(|_| RsaRefusal::NotThisCiphertext)?
                .len(),
            None => ::aws_lc_rs::rsa::Pkcs1PrivateDecryptingKey::new(private_key)
                .map_err(|_| RsaRefusal::Failed)?
                .decrypt(ciphertext, &mut plaintext)
                .map_err(|_| RsaRefusal::NotThisCiphertext)?
                .len(),
        };
        plaintext.truncate(len);
        Ok(plaintext)
    }

    /// The longest plaintext this key encrypts under `scheme`, in bytes, or the reason
    /// it encrypts none — so a caller can state the limit in its own refusal before it
    /// sends anything to be encrypted.
    pub fn rsa_longest_plaintext(&self, scheme: RsaEncryptionScheme) -> Result<usize, RsaRefusal> {
        let private_key = self.rsa_private_key()?;
        Ok(longest_plaintext(private_key.key_size_bytes(), scheme))
    }

    /// This key's RSA key pair, read as the library's own private key.
    ///
    /// `RsaRefusal::NotRsaKeyPair` for a key that is not an asymmetric key pair of an
    /// RSA algorithm — a symmetric key, or an elliptic curve pair, which is a signing
    /// and key-agreement pair rather than an encryption one.
    fn rsa_private_key(&self) -> Result<::aws_lc_rs::rsa::PrivateDecryptingKey, RsaRefusal> {
        let attributes = &self.key_attributes;
        if attributes.key_class != KEY_CLASS_ASYMMETRIC_PAIR
            || !RSA_KEY_ALGORITHMS.contains(&attributes.key_algorithm.as_str())
        {
            return Err(RsaRefusal::NotRsaKeyPair);
        }
        let Some(key_pair) = self.key_pair.as_ref() else {
            return Err(RsaRefusal::NotRsaKeyPair);
        };
        ::aws_lc_rs::rsa::PrivateDecryptingKey::from_pkcs8(key_pair.private_key_pkcs8())
            .map_err(|_| RsaRefusal::Failed)
    }
}

/// The modelled `KeyAlgorithm` values that name an RSA key pair
/// (`smithy_query(query="shape",shape="KeyAlgorithm")`), matched exactly — the rule
/// every table over that enum in this crate follows.
const RSA_KEY_ALGORITHMS: &[&str] = &["RSA_2048", "RSA_3072", "RSA_4096"];

/// The OAEP parameters a scheme names, or `None` for the PKCS1 scheme, which takes no
/// digest.
fn oaep_algorithm(
    scheme: RsaEncryptionScheme,
) -> Option<&'static ::aws_lc_rs::rsa::OaepAlgorithm> {
    match scheme {
        RsaEncryptionScheme::Pkcs1 => None,
        RsaEncryptionScheme::OaepSha1 => Some(&::aws_lc_rs::rsa::OAEP_SHA1_MGF1SHA1),
        RsaEncryptionScheme::OaepSha256 => Some(&::aws_lc_rs::rsa::OAEP_SHA256_MGF1SHA256),
        RsaEncryptionScheme::OaepSha512 => Some(&::aws_lc_rs::rsa::OAEP_SHA512_MGF1SHA512),
    }
}

/// How many bytes of overhead the PKCS1 scheme's padding occupies: a message of at most
/// `k - 11` bytes, where `k` is the modulus in bytes (RFC 8017 section 7.2.1).
const PKCS1_OVERHEAD: usize = 11;

/// The longest plaintext a modulus of `modulus_bytes` admits under `scheme`.
///
/// Each scheme's own ceiling: `k - 11` for PKCS1, and `k - 2*hLen - 2` for OAEP, where
/// `hLen` is the digest's output length (RFC 8017 sections 7.1.1 and 7.2.1). Derived
/// from the modulus and the digest rather than listed, so every key size answers its own
/// limit.
fn longest_plaintext(modulus_bytes: usize, scheme: RsaEncryptionScheme) -> usize {
    let overhead = match scheme {
        RsaEncryptionScheme::Pkcs1 => PKCS1_OVERHEAD,
        RsaEncryptionScheme::OaepSha1 => 2 * ::aws_lc_rs::digest::SHA1_OUTPUT_LEN + 2,
        RsaEncryptionScheme::OaepSha256 => 2 * ::aws_lc_rs::digest::SHA256_OUTPUT_LEN + 2,
        RsaEncryptionScheme::OaepSha512 => 2 * ::aws_lc_rs::digest::SHA512_OUTPUT_LEN + 2,
    };
    modulus_bytes.saturating_sub(overhead)
}
