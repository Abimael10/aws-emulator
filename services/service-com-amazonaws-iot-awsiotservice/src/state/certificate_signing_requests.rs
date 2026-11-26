// SPDX-License-Identifier: CC0-1.0

//! The CERTIFICATE SIGNING REQUEST a caller hands `CreateCertificateFromCsr`: what this
//! service reads out of it, what it refuses, and the certificate it issues over it.
//!
//! `CreateCertificateFromCsr` is documented "Creates an X.509 certificate using the
//! specified certificate signing request", so the certificate's SUBJECT and its PUBLIC
//! KEY are the request's — read out of the PKCS#10 `CertificationRequest` the caller sent
//! (RFC 2986) with `x509-cert` — and nothing about them is chosen here. A request this
//! service cannot read is refused rather than answered with a certificate that certifies
//! something the caller never asked for.
//!
//! The same sentence states which keys the request may carry: "The CSR must include a
//! public key that is either an RSA key with a length of at least 2048 bits or an ECC key
//! from NIST P-256, NIST P-384, or NIST P-521 curves"
//! (`smithy_query(query="doc",shape="CreateCertificateFromCsr")`). That is the whole rule
//! [`read_certificate_signing_request`] applies to the key it finds, and it is applied to
//! the key's OWN encoding — an RSA modulus's bit length, an EC key's named curve — rather
//! than to anything the request says about itself.
//!
//! A certificate the caller asked for is signed by AWS IoT and not by the key it
//! certifies, because the caller keeps the private half and this service never sees it.
//! The signing authority is therefore this emulator's own
//! ([`IssuingCertificateAuthority`]): one RSA-2048 key, minted on the first certificate it
//! has to sign and kept in this service's state for every later one, so every certificate
//! issued over a signing request chains to one issuer and a client can verify any of them
//! against that one key. The key is held in memory only, is never reported by any
//! operation and is never written anywhere.
//!
//! Each call issues a DISTINCT certificate, which is what the operation documents of a
//! repeated request: "Reusing the same certificate signing request (CSR) results in a
//! distinct certificate". It follows from the certificate itself — a fresh serial number
//! and the instant it was issued — rather than from a counter, so the identity derived
//! from its DER ([`super::certificate_id`]) differs too.

use std::time::SystemTime;

use x509_cert::der::{Decode, DecodePem};
use x509_cert::name::{Name, RdnSequence};
use x509_cert::request::CertReq;
use x509_cert::spki::{ObjectIdentifier, SubjectPublicKeyInfoOwned};

use super::certificates::{
    certificate_not_after, oid, recorded_certificate, relative_distinguished_name,
    sign_end_entity_certificate, CertificateToIssue, RsaKeyPairDer,
    CERTIFICATE_ORGANIZATION_NAME,
};
use super::RegisteredCertificate;

/// The shortest RSA key a certificate signing request may carry, in bits: "an RSA key
/// with a length of at least 2048 bits" (`CreateCertificateFromCsr`).
pub const MIN_CSR_RSA_KEY_BITS: usize = 2048;

/// The object identifiers a certificate signing request's own public key names.
///
/// Each is the identifier RFC 5480 (section 2.1.1 for the curves, section 2 for the two
/// algorithms) assigns to the key type the operation's documentation names, so what admits
/// a key is the key's own encoding.
mod key_oid {
    use x509_cert::spki::ObjectIdentifier;

    /// `rsaEncryption` — an RSA public key (RFC 8017 appendix C).
    pub const RSA_ENCRYPTION: ObjectIdentifier =
        ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
    /// `id-ecPublicKey` — an elliptic-curve public key over the named curve its
    /// parameters carry (RFC 5480 section 2.1.1).
    pub const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
    /// `secp256r1`, the curve NIST publishes as P-256.
    pub const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
    /// `secp384r1`, the curve NIST publishes as P-384.
    pub const SECP384R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
    /// `secp521r1`, the curve NIST publishes as P-521.
    pub const SECP521R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.35");
}

/// The named curves an EC key in a certificate signing request may be on: "an ECC key
/// from NIST P-256, NIST P-384, or NIST P-521 curves"
/// (`CreateCertificateFromCsr`), each by the identifier RFC 5480 section 2.1.1 gives it.
pub const CSR_EC_CURVES: [ObjectIdentifier; 3] = [
    key_oid::SECP256R1,
    key_oid::SECP384R1,
    key_oid::SECP521R1,
];

/// The common name of the authority this service signs a requested certificate under.
///
/// A certificate issued over a caller's signing request is signed by AWS IoT rather than
/// by the key it certifies, so the issuer is an authority of this service's own and is
/// named as one — apart from [`super::certificates`]'s default subject name, which is the
/// name of the DEVICE a certificate this service minted the key pair for certifies.
const ISSUING_AUTHORITY_COMMON_NAME: &str = "AWS IoT Certificate Authority";

/// Why a certificate signing request did NOT produce a certificate.
///
/// Both variants are answered with the `InvalidRequestException` (HTTP 400) the operation
/// models — "The request is not valid" — because what is refused is the REQUEST's own
/// content: neither refusal depends on anything the account holds, so both are taken
/// before the store is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateSigningRequestRefusal {
    /// Material that is not a PEM-encoded PKCS#10 certification request at all.
    Unreadable(String),
    /// A readable request whose public key is not one of the keys the operation documents.
    UnsupportedKey(String),
}

impl CertificateSigningRequestRefusal {
    /// The message the refusal states, whichever reason it names.
    pub fn message(&self) -> &str {
        match self {
            Self::Unreadable(message) | Self::UnsupportedKey(message) => message,
        }
    }
}

/// The message a request refuses material it cannot read as a certification request with.
///
/// AWS IoT states this refusal in ONE fixed sentence: it names neither the member that
/// carried the material, nor the bytes it held, nor what about them could not be read, so
/// the sentence has no variable part and nothing of the request reaches it. The class it
/// is stated under is the modelled `InvalidRequestException` (HTTP 400), "The request is
/// not valid".
pub fn unreadable_certificate_signing_request_message() -> String {
    "Invalid CSR format".to_string()
}

/// The message a request refuses a public key the operation does not admit with.
///
/// A readable certification request whose key violates the key-strength constraint the
/// operation documents — "The CSR must include a public key that is either an RSA key with
/// a length of at least 2048 bits or an ECC key from NIST P-256, NIST P-384, or NIST P-521
/// curves" — is refused in ONE fixed sentence that states the class of the violation and
/// not the rule that was violated: it names neither the member that carried the request,
/// nor the key type or size submitted, nor the shortest admitted key or the admitted curve
/// names, so the sentence has no variable part. The class it is stated under is the
/// modelled `InvalidRequestException` (HTTP 400), "The request is not valid".
pub fn unsupported_csr_key_message() -> String {
    "CSR violates constraints".to_string()
}

/// The certification request `material` holds, or why it is not one this operation can
/// issue a certificate over.
///
/// The surrounding whitespace is trimmed because `CertificateSigningRequest` is modelled
/// with the pattern `^[\s\S]*$` — any whitespace is part of a legal value — so a PEM block
/// that arrived with a trailing newline is the same request as one that did not.
pub fn read_certificate_signing_request(
    material: &str,
) -> Result<CertReq, CertificateSigningRequestRefusal> {
    let request = CertReq::from_pem(material.trim().as_bytes()).map_err(|_| {
        CertificateSigningRequestRefusal::Unreadable(
            unreadable_certificate_signing_request_message(),
        )
    })?;
    if !is_supported_csr_key(&request.info.public_key) {
        return Err(CertificateSigningRequestRefusal::UnsupportedKey(
            unsupported_csr_key_message(),
        ));
    }
    Ok(request)
}

/// Whether `public_key` is one of the keys the operation documents: an RSA key of at least
/// [`MIN_CSR_RSA_KEY_BITS`] bits, or an EC key on one of [`CSR_EC_CURVES`].
///
/// Read off the key's own encoding: the RSA length is the bit length of the modulus inside
/// the `SubjectPublicKeyInfo`, and the curve is the named curve the key's algorithm
/// parameters carry (RFC 5480 section 2.1.1). A key of any other type — an Ed25519 key, an
/// EC key on another curve — is not one of the two the sentence admits.
fn is_supported_csr_key(public_key: &SubjectPublicKeyInfoOwned) -> bool {
    if public_key.algorithm.oid == key_oid::RSA_ENCRYPTION {
        return rsa_key_bits(public_key).is_some_and(|bits| bits >= MIN_CSR_RSA_KEY_BITS);
    }
    if public_key.algorithm.oid == key_oid::EC_PUBLIC_KEY {
        return public_key
            .algorithm
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.decode_as::<ObjectIdentifier>().ok())
            .is_some_and(|curve| CSR_EC_CURVES.contains(&curve));
    }
    false
}

/// The length, in bits, of the RSA key `public_key` carries, or `None` when its
/// `subjectPublicKey` is not a PKCS#1 `RSAPublicKey`.
///
/// The length of an RSA key IS the bit length of its modulus (RFC 8017 section 3.1), and
/// `UintRef` holds that modulus without the leading zero byte DER adds to keep it
/// positive, so the bit length is read off the bytes it does hold.
fn rsa_key_bits(public_key: &SubjectPublicKeyInfoOwned) -> Option<usize> {
    let key = ::pkcs1::RsaPublicKey::from_der(public_key.subject_public_key.raw_bytes()).ok()?;
    let modulus = key.modulus.as_bytes();
    let leading = modulus.first()?;
    Some(modulus.len() * 8 - usize::try_from(leading.leading_zeros()).ok()?)
}

/// The certificate AUTHORITY this service signs the certificates it issues over callers'
/// certification requests with.
///
/// One per emulator, minted on the first certificate it has to sign and kept in this
/// service's state from then on ([`super::ComAmazonawsIotAwsiotserviceState::issuing_certificate_authority`]),
/// so every such certificate has one issuer and one verification key rather than one per
/// call.
pub struct IssuingCertificateAuthority {
    /// The authority's RSA-2048 key pair. Private: the signing key is reached only through
    /// [`IssuingCertificateAuthority::issue_certificate`], so nothing can report it.
    key: RsaKeyPairDer,
}

/// The key material is NOT formatted: a derived formatter is the usual way a signing key
/// reaches a log, and this record exists only to sign with.
impl ::std::fmt::Debug for IssuingCertificateAuthority {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("IssuingCertificateAuthority")
            .finish_non_exhaustive()
    }
}

impl IssuingCertificateAuthority {
    /// Mint the authority: a fresh RSA-2048 key pair, `None` when the library refuses to
    /// produce or encode one.
    ///
    /// Generating an RSA key pair is hundreds of milliseconds of CPU, so a caller runs
    /// this on the blocking pool (`tokio::task::spawn_blocking`) and with no lock held,
    /// for the reason [`super::mint_keys_and_certificate`] documents.
    pub fn generate() -> Option<Self> {
        Some(Self {
            key: RsaKeyPairDer::generate()?,
        })
    }

    /// The authority's own PUBLIC key, as the DER `SubjectPublicKeyInfo` that verifies
    /// every certificate this authority signed.
    ///
    /// Public key material only — the half a verifier needs and the half that is safe to
    /// hand out; the private half is reachable from nowhere, inside this crate or outside
    /// it.
    pub fn verification_key_der(&self) -> Vec<u8> {
        self.key.public_key_spki.clone()
    }

    /// The X.500 distinguished name this authority issues under, which is the `issuer` of
    /// every certificate it signs.
    fn name() -> Option<Name> {
        Some(Name::from(RdnSequence(vec![
            relative_distinguished_name(oid::ORGANIZATION, CERTIFICATE_ORGANIZATION_NAME)?,
            relative_distinguished_name(oid::COMMON_NAME, ISSUING_AUTHORITY_COMMON_NAME)?,
        ])))
    }

    /// Issue the certificate `request` asks for, as the record this service keeps of it.
    ///
    /// The certificate certifies the request's OWN subject and public key, is valid from
    /// `now` until [`super::certificates::certificate_not_after`] — the validity of every
    /// device certificate this service issues — is signed by this authority, and is
    /// recorded exactly as a certificate minted here is
    /// ([`super::certificates::recorded_certificate`]): `set_as_active` is
    /// `CreateCertificateFromCsr$setAsActive`, which "Specifies whether the certificate is
    /// active", and the record's id and ARN are derived from the certificate's own DER.
    ///
    /// `None` when a field cannot be encoded or the library refuses to sign.
    pub fn issue_certificate(
        &self,
        region: &str,
        owned_by: &str,
        request: &CertReq,
        set_as_active: bool,
        now: SystemTime,
    ) -> Option<RegisteredCertificate> {
        let not_after = certificate_not_after();
        let certificate = sign_end_entity_certificate(
            CertificateToIssue {
                issuer: Self::name()?,
                subject: request.info.subject.clone(),
                subject_public_key_info: request.info.public_key.clone(),
                not_before: now,
                not_after,
            },
            &self.key,
        )?;
        recorded_certificate(region, owned_by, set_as_active, now, not_after, &certificate)
    }
}
