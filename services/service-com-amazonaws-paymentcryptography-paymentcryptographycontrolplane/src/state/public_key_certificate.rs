// SPDX-License-Identifier: CC0-1.0

//! The public key certificate this service hands out for an asymmetric key, and the
//! certificate authority that signs it.
//!
//! One concern: how the public half of a stored key pair leaves the service as a
//! certificate. `GetPublicKeyCertificate` "gets the public key certificate of the
//! asymmetric key pair that exists within Amazon Web Services Payment Cryptography"
//! (the operation's own documentation), so the certificate names the pair the key has
//! held since it was minted — the same pair a certificate signing request
//! ([`super::key_pair`]) carries — and it is signed by this store's own authority,
//! whose self-signed certificate is the chain that comes with it.
//!
//! Both documented spans of that certificate live here: `KeyCertificate`'s own
//! documentation says "the certificate is valid for 90 days from the time it is issued.
//! The service returns a cached certificate if one exists with at least 30 days of
//! remaining validity. Otherwise, a new 90-day certificate is issued." So a key keeps
//! the certificate that was issued for it and every later call is answered with that
//! same certificate until the clock has taken it below the remaining validity the
//! documentation names, at which point the next call issues the next one.
//!
//! Every byte is real: the authority's key pair comes from `aws-lc-rs`, the certificate
//! is encoded by `x509-cert`, and its signature is made by the authority's private half
//! over the DER of the certificate's own `TBSCertificate` — so a caller can verify the
//! certificate it is handed against the chain it is handed, and the public key inside it
//! is the public half of the key pair this store holds for the key.

use std::time::{Duration, SystemTime};

use super::{
    certificate, distinguished_name,
    ComAmazonawsPaymentcryptographyPaymentcryptographycontrolplaneState as KeyStore, KeyMaterial,
    RsaKeyPairDer, StoredKey, KEY_CLASS_ASYMMETRIC_PAIR, KEY_STATE_DELETE_COMPLETE,
};

/// How long an issued public key certificate is valid.
///
/// From `KeyCertificate`'s own documentation: "The certificate is valid for 90 days from
/// the time it is issued."
pub const PUBLIC_KEY_CERTIFICATE_VALIDITY: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// How much validity an issued certificate must have left for a later call to be
/// answered with it instead of issuing the next one.
///
/// From the same documentation: "The service returns a cached certificate if one exists
/// with at least 30 days of remaining validity. Otherwise, a new 90-day certificate is
/// issued."
pub const PUBLIC_KEY_CERTIFICATE_REUSE_REMAINING: Duration =
    Duration::from_secs(30 * 24 * 60 * 60);

/// How long the authority's own certificate is valid.
///
/// Chosen rather than documented: the chain has to outlive every certificate the
/// authority can issue, which is one [`PUBLIC_KEY_CERTIFICATE_VALIDITY`] after the last
/// call that issues one, so the authority is given a span no run of this emulator
/// reaches the end of.
const CERTIFICATE_AUTHORITY_VALIDITY: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

/// The common name the authority's certificate carries. It names the service and what
/// the authority is: the root that signs this service's public key certificates.
const CERTIFICATE_AUTHORITY_COMMON_NAME: &str = "Payment Cryptography Root CA";

/// The modulus size of the authority's RSA key pair, which is what makes its
/// `sha256WithRSAEncryption` signature over each certificate it issues.
const CERTIFICATE_AUTHORITY_KEY_SIZE: ::aws_lc_rs::rsa::KeySize = ::aws_lc_rs::rsa::KeySize::Rsa2048;

/// This store's certificate authority: the key pair that signs every public key
/// certificate, and the self-signed certificate that names it.
///
/// Minted once and never replaced, so every certificate this service hands out is signed
/// by one authority and verifies against the one chain a caller is handed with it.
#[derive(Clone, Debug)]
pub struct PublicKeyCertificateAuthority {
    /// The authority's private half, PKCS#8 DER. Carried in the same redacted wrapper as
    /// a key's own material, so no formatter anywhere can print it, and read only by the
    /// signature over a certificate this authority issues.
    private_key_pkcs8: KeyMaterial,
    /// The authority's public half, a DER X.509 `SubjectPublicKeyInfo`.
    public_key_spki: Vec<u8>,
    /// The authority's own self-signed certificate, as `KeyCertificateChain` reports it.
    certificate_chain: String,
}

impl PublicKeyCertificateAuthority {
    /// The authority's own certificate: "the root certificate authority (CA) that signed
    /// the public key certificate in PEM format (base64 encoded)"
    /// (`KeyCertificateChain`'s own documentation).
    pub fn certificate_chain(&self) -> &str {
        &self.certificate_chain
    }

    /// The key pair this authority signs with, as the certificate encoder takes it.
    fn signer(&self) -> RsaKeyPairDer {
        RsaKeyPairDer {
            public_key_spki: self.public_key_spki.clone(),
            private_key_pkcs8: self.private_key_pkcs8.bytes().to_vec(),
        }
    }

    /// Issue a certificate for the public key `subject_public_key_spki`, naming
    /// `subject_common_name` and valid for `validity`.
    ///
    /// `None` when a field cannot be encoded or the signature cannot be made, which is
    /// this service's own failure rather than anything the request asked for.
    fn issue(
        &self,
        subject_common_name: &str,
        subject_public_key_spki: &[u8],
        validity: (SystemTime, SystemTime),
    ) -> Option<String> {
        certificate(
            &distinguished_name(CERTIFICATE_AUTHORITY_COMMON_NAME)?,
            &distinguished_name(subject_common_name)?,
            subject_public_key_spki,
            validity,
            false,
            &self.signer(),
        )
    }
}

/// Mint this store's certificate authority: an RSA key pair and the self-signed
/// certificate that names it, valid from `now`.
///
/// Generating an RSA key pair is hundreds of milliseconds of CPU, so a caller runs this
/// with no lock held and off the async runtime.
///
/// `None` when the library could not produce or encode the key pair, or when a
/// certificate field cannot be encoded.
pub fn mint_public_key_certificate_authority(
    now: SystemTime,
) -> Option<PublicKeyCertificateAuthority> {
    let key_pair = RsaKeyPairDer::generate(CERTIFICATE_AUTHORITY_KEY_SIZE)?;
    let name = distinguished_name(CERTIFICATE_AUTHORITY_COMMON_NAME)?;
    let not_after = now.checked_add(CERTIFICATE_AUTHORITY_VALIDITY)?;
    // Self-signed and marked a certificate authority, which is what lets it sign the
    // certificates below (RFC 5280 section 4.2.1.9).
    let certificate_chain = certificate(
        &name,
        &name,
        &key_pair.public_key_spki,
        (now, not_after),
        true,
        &key_pair,
    )?;
    Some(PublicKeyCertificateAuthority {
        private_key_pkcs8: KeyMaterial(key_pair.private_key_pkcs8),
        public_key_spki: key_pair.public_key_spki,
        certificate_chain,
    })
}

/// One issued public key certificate, as the two members that report it and the span
/// that decides when the next one is issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedPublicKeyCertificate {
    /// The certificate of the key's public half, as `KeyCertificate` reports it.
    pub certificate: String,
    /// The authority's certificate, as `KeyCertificateChain` reports it.
    pub certificate_chain: String,
    /// When it was issued.
    pub issued: SystemTime,
    /// When it expires: one [`PUBLIC_KEY_CERTIFICATE_VALIDITY`] after it was issued.
    pub not_after: SystemTime,
}

impl IssuedPublicKeyCertificate {
    /// Whether this certificate still has `remaining` of its validity left at `now`,
    /// which is what decides whether a call is answered with it or issues the next one.
    pub fn has_remaining(&self, now: SystemTime, remaining: Duration) -> bool {
        now.checked_add(remaining)
            .is_some_and(|deadline| deadline <= self.not_after)
    }
}

/// Why no public key certificate could be answered for an identifier.
#[derive(Debug, Clone)]
pub enum PublicKeyCertificateRejection {
    /// This store holds no key the identifier names; the string is the identifier AS THE
    /// REQUEST GAVE IT.
    NotFound(String),
    /// The identifier names a key that has no public key to certify; the message says
    /// why, naming the member.
    Invalid(String),
    /// The request is admissible and this service could not encode or sign the
    /// certificate.
    Internal,
}

impl StoredKey {
    /// The common name the certificate of this key carries: the id of the account the key
    /// belongs to, read out of the key's own ARN.
    ///
    /// What the real service names the subject of a public key certificate by, as a
    /// recorded response of it shows. An account id is twelve digits, well inside the
    /// X.520 upper bound on a common name that RFC 5280 section 4.1.2.4 keeps.
    fn certificate_common_name(&self) -> &str {
        // The ARN layout `arn:aws:<service>:<region>:<account-id>:<resource>`. Every ARN
        // this store minted carries one; anything else answers this emulator's own
        // account, which is the account every key of this store belongs to.
        self.key_arn
            .split(':')
            .nth(4)
            .unwrap_or_else(|| ::engine::emulator_account_id())
    }

    /// The certificate already issued for this key's public half, when it still has
    /// [`PUBLIC_KEY_CERTIFICATE_REUSE_REMAINING`] of its validity left at `now`.
    ///
    /// `None` means the next call has to issue one: either nothing has been issued for
    /// this key yet, or the clock has taken what was issued below that remaining
    /// validity.
    fn reusable_public_key_certificate(
        &self,
        now: SystemTime,
    ) -> Option<&IssuedPublicKeyCertificate> {
        self.public_key_certificate
            .as_ref()
            .filter(|issued| issued.has_remaining(now, PUBLIC_KEY_CERTIFICATE_REUSE_REMAINING))
    }

    /// The public half of this key's key pair, as the DER `SubjectPublicKeyInfo` a
    /// certificate carries it in.
    ///
    /// The refusal is the one a key with no public key earns: a key that is not an
    /// asymmetric key pair has no public half to certify, and neither has one whose
    /// material entered this service as a public key alone.
    fn public_key_to_certify(&self) -> Result<&[u8], PublicKeyCertificateRejection> {
        self.key_pair
            .as_ref()
            .map(|pair| pair.public_key_spki())
            .ok_or_else(|| {
                PublicKeyCertificateRejection::Invalid(format!(
                    "KeyIdentifier names the key {}, which is a {}, and a public key \
                     certificate is the certificate of the public key of an \
                     {KEY_CLASS_ASYMMETRIC_PAIR}",
                    self.key_arn, self.key_attributes.key_class
                ))
            })
    }

    /// Issue the next public key certificate for this key under `authority`, store it on
    /// this record and answer it.
    ///
    /// The one writer of [`StoredKey::public_key_certificate`], so a certificate a caller
    /// is handed is always the one this record holds — which is what makes the next call
    /// answer the same certificate.
    fn issue_public_key_certificate(
        &mut self,
        authority: &PublicKeyCertificateAuthority,
        now: SystemTime,
    ) -> Result<IssuedPublicKeyCertificate, PublicKeyCertificateRejection> {
        let not_after = now
            .checked_add(PUBLIC_KEY_CERTIFICATE_VALIDITY)
            .ok_or(PublicKeyCertificateRejection::Internal)?;
        let certificate = authority
            .issue(
                self.certificate_common_name(),
                self.public_key_to_certify()?,
                (now, not_after),
            )
            .ok_or(PublicKeyCertificateRejection::Internal)?;
        let issued = IssuedPublicKeyCertificate {
            certificate,
            certificate_chain: authority.certificate_chain().to_string(),
            issued: now,
            not_after,
        };
        self.public_key_certificate = Some(issued.clone());
        Ok(issued)
    }
}

impl KeyStore {
    /// Whether this store has already minted its certificate authority.
    ///
    /// Asked before a key pair is generated, so only a call that actually has to issue a
    /// certificate under an authority this store does not have yet pays for minting one.
    pub fn has_public_key_certificate_authority(&self) -> bool {
        self.public_key_certificate_authority.is_some()
    }

    /// The certificate the key `identifier` names has already been issued, when a call at
    /// `now` is answered with it rather than issuing the next one.
    ///
    /// Refuses an identifier this store holds no live key for and a key with no public
    /// key to certify, so a call that cannot be served costs no issuance at all.
    pub fn reusable_public_key_certificate(
        &self,
        identifier: &str,
        now: SystemTime,
    ) -> Result<Option<IssuedPublicKeyCertificate>, PublicKeyCertificateRejection> {
        let key = self.live_key(identifier, now).ok_or_else(|| {
            PublicKeyCertificateRejection::NotFound(identifier.to_string())
        })?;
        key.public_key_to_certify()?;
        Ok(key.reusable_public_key_certificate(now).cloned())
    }

    /// The public key certificate of the key `identifier` names at `now`: the one already
    /// issued for it while that one still has enough validity left, and otherwise the
    /// next one, issued under this store's authority and stored on the key.
    ///
    /// `minted_authority` is an authority the caller minted off the lock, used only when
    /// this store has none yet — the authority is installed and read under the ONE borrow
    /// that issues, so concurrent calls cannot each install one and hand out certificates
    /// signed by authorities the other's chain does not name.
    pub fn public_key_certificate(
        &mut self,
        identifier: &str,
        minted_authority: Option<PublicKeyCertificateAuthority>,
        now: SystemTime,
    ) -> Result<IssuedPublicKeyCertificate, PublicKeyCertificateRejection> {
        let arn = self.resolve_key_arn(identifier).ok_or_else(|| {
            PublicKeyCertificateRejection::NotFound(identifier.to_string())
        })?;
        // The key is judged before anything is installed, so a call this store cannot
        // serve leaves it exactly as it was.
        self.keys
            .get(&arn)
            .filter(|key| key.effective_state(now) != KEY_STATE_DELETE_COMPLETE)
            .ok_or_else(|| PublicKeyCertificateRejection::NotFound(identifier.to_string()))?
            .public_key_to_certify()?;
        // Whichever call minted first keeps the authority: an authority already in the
        // store is never replaced, so a certificate issued earlier still verifies against
        // the chain this store answers with.
        let authority = match self.public_key_certificate_authority.as_ref() {
            Some(authority) => authority.clone(),
            None => {
                let minted = minted_authority.ok_or(PublicKeyCertificateRejection::Internal)?;
                self.public_key_certificate_authority = Some(minted.clone());
                minted
            }
        };
        let key = self
            .keys
            .get_mut(&arn)
            .ok_or_else(|| PublicKeyCertificateRejection::NotFound(identifier.to_string()))?;
        // Checked again under this borrow: a call that ran between the read above and
        // this one may have issued the certificate this call was about to, and the stored
        // one is what every reader is answered with.
        if let Some(issued) = key.reusable_public_key_certificate(now) {
            return Ok(issued.clone());
        }
        key.issue_public_key_certificate(&authority, now)
    }
}
