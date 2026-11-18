// SPDX-License-Identifier: CC0-1.0

//! The X.509 certificate a user pool signs its SAML 2.0 federation traffic with.
//!
//! `GetSigningCertificate` "returns the signing certificate for SAML 2.0
//! federation" of the pool it names, and the member it answers carries "the
//! x.509 certificate that signs SAML 2.0 authentication requests for your user
//! pool" (this service's model) — so what the member carries has to be a
//! certificate the SAML identity provider on the other side can decode, parse
//! and install as the pool's signature verifier, not text in the shape of one.
//! That is what every identity provider configured for a Cognito user pool does
//! with it: the operator copies the value into the provider's trust
//! configuration, where it is read as the base64 of a DER X.509 certificate —
//! the same coding a SAML metadata `X509Certificate` element carries.
//!
//! The certificate describes the pool's OWN signing key pair — the RSA key pair
//! the pool signs with and publishes at `<iss>/.well-known/jwks.json` — rather
//! than a key pair minted for the certificate alone: a pool has one signing
//! identity, so a verifier that reads the key out of this certificate and a
//! verifier that reads it out of the pool's published key set are configured for
//! the same key, and an RSA-expecting SAML verifier can parse it at all.
//!
//! A child module of [`super`] rather than more of `state.rs`: certifying that
//! key is one concern. The private half of the key never comes here — the
//! certificate is signed by handing the bytes to sign back to the key that holds
//! it ([`::service_common::jws_signing::SigningKey::sign`]) — so nothing of this
//! service stores or reports a private key.

use super::*;

/// How long an issued signing certificate is valid: "Issued certificates are
/// valid for 10 years from the date of issue" (`GetSigningCertificate` in this
/// service's model), counted as the 3652 days ten Gregorian years span.
pub const SIGNING_CERTIFICATE_VALIDITY: Duration = Duration::from_secs(3652 * 24 * 60 * 60);

/// The SAML 2.0 service-provider entity id of the user pool `user_pool_id`:
/// `urn:amazon:cognito:sp:<UserPoolId>`, the name a pool identifies itself under
/// in the SAML exchange and therefore the subject its signing certificate is
/// issued to (Amazon Cognito Developer Guide, "Adding SAML identity providers to
/// a user pool", which has the operator configure that URN as the provider's
/// audience).
#[must_use]
pub fn saml_service_provider_entity_id(user_pool_id: &str) -> String {
    format!("urn:amazon:cognito:sp:{user_pool_id}")
}

/// The signing certificate of the pool `user_pool_id`, issued at `issued_at`
/// over the pool's own signing key `signing_key`: the base64 of a REAL
/// self-signed X.509 certificate, issued to that pool's SAML service-provider
/// entity id ([`saml_service_provider_entity_id`]) and valid for
/// [`SIGNING_CERTIFICATE_VALIDITY`] from the date of issue.
///
/// Issued ONCE, when the pool is created, and kept on the pool's record: the
/// operation that reports it renders the stored value, so every read of one pool
/// answers the same certificate, and two pools answer different ones because
/// each certifies its own pool's key under its own entity id.
///
/// `None` when the certificate does not issue — including for a key of an
/// algorithm no X.509 signature algorithm here corresponds to — which the caller
/// reports rather than storing a pool whose signing certificate is a value
/// nothing signed.
#[must_use]
pub fn issue_signing_certificate(
    user_pool_id: &str,
    issued_at: SystemTime,
    signing_key: &::service_common::jws_signing::SigningKey,
) -> Option<String> {
    let certificate = signing_certificate_der(user_pool_id, issued_at, signing_key);
    match certificate {
        Some(der) => Some(::aws_smithy_types::base64::encode(der)),
        None => {
            // The caller answers its modelled server error, which says nothing
            // about WHY: the generator's own failure is worth a line so the path
            // is diagnosable if it ever fires.
            ::tracing::warn!(
                user_pool_id,
                "the user pool's SAML signing certificate could not be issued"
            );
            None
        }
    }
}

/// The DER-encoded certificate itself, which [`issue_signing_certificate`] is
/// the base64 of.
fn signing_certificate_der(
    user_pool_id: &str,
    issued_at: SystemTime,
    signing_key: &::service_common::jws_signing::SigningKey,
) -> Option<Vec<u8>> {
    let key = ::rcgen::KeyPair::from_remote(Box::new(PoolSigningKey::new(signing_key)?)).ok()?;
    // No subject alternative name: the pool is named by a URN entity id rather
    // than by a host it serves, so there is no DNS name or address to certify.
    let mut params = ::rcgen::CertificateParams::new(Vec::<String>::new()).ok()?;
    let mut subject = ::rcgen::DistinguishedName::new();
    subject.push(
        ::rcgen::DnType::CommonName,
        saml_service_provider_entity_id(user_pool_id),
    );
    params.distinguished_name = subject;
    params.not_before = offset_date_time(issued_at)?;
    params.not_after = offset_date_time(issued_at.checked_add(SIGNING_CERTIFICATE_VALIDITY)?)?;
    // An end-entity certificate, marked explicitly as not a certificate
    // authority: it signs the pool's own SAML messages and certifies no other
    // certificate.
    params.is_ca = ::rcgen::IsCa::ExplicitNoCa;
    params.key_usages = vec![::rcgen::KeyUsagePurpose::DigitalSignature];
    Some(params.self_signed(&key).ok()?.der().to_vec())
}

/// The pool's own signing key, presented to the certificate generator as the key
/// the certificate describes and is signed by.
///
/// The generator needs two things of a key: the PUBLIC half, which it certifies,
/// and a signature over the certificate body. Both come from the key the pool
/// already holds and publishes, so the certificate and the pool's key set
/// describe one key pair; the private half stays inside that key and is reached
/// only to sign.
struct PoolSigningKey {
    /// The pool's key, which signs the certificate body.
    key: ::service_common::jws_signing::SigningKey,
    /// Its PUBLIC half, in the encoding the certificate's
    /// `SubjectPublicKeyInfo` carries.
    public_key: Vec<u8>,
    /// The X.509 signature algorithm matching the key's own JWS algorithm.
    algorithm: &'static ::rcgen::SignatureAlgorithm,
}

impl PoolSigningKey {
    /// `key` as the generator's key, or `None` for a key whose algorithm no
    /// X.509 signature algorithm here corresponds to.
    fn new(key: &::service_common::jws_signing::SigningKey) -> Option<Self> {
        Some(Self {
            key: key.clone(),
            public_key: key.public_key_der(),
            algorithm: certificate_signature_algorithm(key.algorithm())?,
        })
    }
}

impl ::rcgen::RemoteKeyPair for PoolSigningKey {
    fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, ::rcgen::Error> {
        self.key
            .sign(message)
            .ok_or(::rcgen::Error::RemoteKeyError)
    }

    fn algorithm(&self) -> &'static ::rcgen::SignatureAlgorithm {
        self.algorithm
    }
}

/// The X.509 signature algorithm a certificate over a key of the JWS algorithm
/// `algorithm` is signed under: a pool's keys are RSA and sign RSASSA-PKCS1-v1_5
/// over SHA-256, which X.509 names `sha256WithRSAEncryption` over an
/// `rsaEncryption` subject public key.
///
/// `None` for an algorithm this correspondence does not cover, so a certificate
/// is either signed under the key's own algorithm or not issued.
fn certificate_signature_algorithm(
    algorithm: &str,
) -> Option<&'static ::rcgen::SignatureAlgorithm> {
    match algorithm {
        ::service_common::jws_signing::RS256 => Some(&::rcgen::PKCS_RSA_SHA256),
        _ => None,
    }
}

/// `at` as the date-time type the certificate generator takes a validity bound
/// in, at the one-second resolution an X.509 validity period is stated at.
///
/// `None` for an instant outside the range that type represents, which is the
/// same answer the caller gives for a certificate that does not issue.
fn offset_date_time(at: SystemTime) -> Option<::time::OffsetDateTime> {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_secs()).ok())?;
    ::time::OffsetDateTime::from_unix_timestamp(seconds).ok()
}
