// SPDX-License-Identifier: CC0-1.0

//! What a held identity provider reports BESIDE the document or the URL it was registered
//! with: the instant a SAML provider stops being valid, its encryption setting, the hashes
//! of the signing keys its metadata publishes, and whether a provider of either kind is
//! enabled for trusted identity propagation.
//!
//! Every value here is DERIVED — from the metadata document the request carried, from the
//! instant the provider was created, or from the operations this service models — so the
//! create, the update and both reads cannot describe one provider differently: each states
//! the rule once, here.

#[allow(unused_imports)]
use super::*;

/// How long a SAML provider whose metadata document states no expiration stays valid,
/// counted from the instant it was created.
///
/// A provider always HAS an expiration: `GetSAMLProvider`'s own example answer (IAM API
/// Reference / AWS CLI reference for `get-saml-provider`) reports a `ValidUntil` exactly
/// 36500 days after the `CreateDate` beside it, to the second, for a document that states
/// none — so a document carrying no `validUntil` leaves the provider with this default
/// lifetime rather than with no expiration at all.
pub const DEFAULT_SAML_PROVIDER_VALIDITY_DAYS: i64 = 36_500;

/// The encryption setting a SAML provider carries when no call has chosen one.
///
/// `assertionEncryptionModeType` admits `Allowed` and `Required`
/// (`smithy_query(query="shape", shape="assertionEncryptionModeType")`), and a provider
/// always reports one of them: a create or update that names no mode leaves the provider
/// with the permissive member, under which an encrypted assertion is accepted but not
/// demanded, which is the only value that cannot change how assertions already in flight
/// are treated.
pub const DEFAULT_ASSERTION_ENCRYPTION_MODE: &str = "Allowed";

/// The encryption setting under which a SAML provider DEMANDS encrypted assertions: the
/// strict member of `assertionEncryptionModeType`
/// (`crate::ops::types::ASSERTION_ENCRYPTION_MODE_TYPE_VALUES`).
///
/// A provider under it can serve nothing without a key to DECRYPT an assertion with: "the
/// private key must be a .pem file that uses AES-GCM or AES-CBC encryption algorithm to
/// decrypt SAML assertions" (`CreateSAMLProvider$AddPrivateKey`), and that key is the only
/// thing a provider holds that can. So a call that would leave a provider demanding
/// encrypted assertions with no private key on file supplied "an invalid … value … for an
/// input parameter" across its members, refused with
/// [`ASSERTION_ENCRYPTION_REQUIRES_PRIVATE_KEY`] — the rule both the create and the update
/// of a provider apply, because either can reach that state.
pub const REQUIRED_ASSERTION_ENCRYPTION_MODE: &str = "Required";

/// The message IAM refuses a call that would leave a SAML provider demanding encrypted
/// assertions with no private key to decrypt them with.
///
/// Recorded behaviour, and entirely fixed text: the sentence names the MODE the call asked
/// for and the key that is missing, and quotes nothing of the request back — what it
/// reports is that the setting could not be put in force, which is why it opens with
/// `Failed to set`.
pub const ASSERTION_ENCRYPTION_REQUIRES_PRIVATE_KEY: &str =
    "Failed to set assertion encryption mode to Required because no private key is present.";

/// Whether a held identity provider is enabled for trusted identity propagation.
///
/// Reported on every entry of `ListSAMLProviders` and `ListOpenIDConnectProviders`, so a
/// caller enumerating providers can tell without a per-provider read. No operation this
/// service models turns it on — nothing in the model carries the setting, so no request can
/// name it — which is why every held provider reports it the one way it can honestly be
/// reported.
pub const TRUSTED_IDENTITY_PROPAGATION_ENABLED: bool = false;

/// When a SAML provider created at `created`, from the document `metadata` was read out of,
/// stops being valid.
///
/// "That document includes the issuer's name, expiration information, and keys"
/// (`CreateSAMLProvider`), so the document's own `validUntil` is the provider's expiration
/// whenever it states one; a document that states none leaves the provider with the default
/// lifetime every provider gets ([`DEFAULT_SAML_PROVIDER_VALIDITY_DAYS`]). Either way the
/// provider HAS an expiration, so `GetSAMLProvider$ValidUntil` and every list entry report
/// one, and it is a property of the provider rather than of the moment it is read.
pub fn saml_provider_valid_until(
    created: ::aws_smithy_types::DateTime,
    metadata: &SamlMetadata,
) -> ::aws_smithy_types::DateTime {
    metadata.valid_until.unwrap_or_else(|| {
        ::aws_smithy_types::DateTime::from_secs(
            created
                .secs()
                .saturating_add(DEFAULT_SAML_PROVIDER_VALIDITY_DAYS.saturating_mul(SECONDS_PER_DAY)),
        )
    })
}

/// The refusal of an OIDC provider `Url` that is not of the published form — one that does
/// not begin with the scheme `Url` requires, that carries query parameters, or that names no
/// host.
///
/// "An invalid or out-of-range value was supplied for an input parameter"
/// (`InvalidInputException`), answered as the CODE ALONE: recorded behaviour for this class
/// is a refusal carrying no `Message` member at all, so there is nothing to word here. A
/// sentence written in its place would be text AWS never emits, which is a divergence a
/// client comparing refusals sees — and one shared constant is what keeps every reading of
/// the `Url`'s form answering the same way.
pub const OPEN_ID_CONNECT_URL_FORM_REFUSAL: crate::ops::types::InvalidInputException =
    crate::ops::types::InvalidInputException { message: None };

/// The server-certificate thumbprint an OIDC provider registered for `issuer` is PINNED
/// with when the request named none.
///
/// "This parameter is optional. If it is not included, IAM will retrieve and use the top
/// intermediate certificate authority (CA) thumbprint of the OpenID Connect identity
/// provider server certificate" (`CreateOpenIDConnectProvider$ThumbprintList`), so a
/// provider is never registered unpinned: the thumbprint belongs to the provider, which is
/// why one read of it is every read of it. Its FORM is published too — "the hex-encoded
/// SHA-1 hash value of the X.509 certificate" (`thumbprintType`), exactly 40 characters —
/// and that is what this derives: the SHA-1 of the endpoint the certificate would be
/// presented for, hex, since this emulator opens no connection to the IdP and so has no
/// fetched chain to hash. `issuer` is the provider's URL as the record holds it (its host
/// and path, without the scheme), so the thumbprint is a property of the provider named
/// rather than of the request or the moment, and two providers never share one.
///
/// The hex is [`data_encoding`]'s HEXLOWER, the one coding this crate spells a digest with
/// ([`key_hash`]).
///
/// What it is NOT: a claim about a certificate the IdP presented. No chain is fetched here —
/// this emulator opens no outbound connection, and a provider URL it cannot reach has no
/// chain to read — so what the provider is pinned to is the identifier THIS service derives
/// for the endpoint, of the published form and stable for it. A caller that needs the
/// thumbprint of a particular certificate supplies it on the request, which is stored as
/// written ([`stored_open_id_connect_thumbprints`]).
pub fn derived_open_id_connect_thumbprint(issuer: &str) -> String {
    let mut hasher = <::sha1::Sha1 as ::sha1::Digest>::new();
    ::sha1::Digest::update(&mut hasher, issuer.as_bytes());
    ::data_encoding::HEXLOWER.encode(&::sha1::Digest::finalize(hasher))
}

/// The thumbprint list a provider registered for `issuer` is STORED with when the request
/// carried `supplied`.
///
/// ONE rule for every write that registers a provider: the thumbprints the request named,
/// or — for a request that named none — the single derived one
/// ([`derived_open_id_connect_thumbprint`]), because IAM pins a certificate for the provider
/// either way.
pub fn stored_open_id_connect_thumbprints(issuer: &str, supplied: Vec<String>) -> Vec<String> {
    if supplied.is_empty() {
        return vec![derived_open_id_connect_thumbprint(issuer)];
    }
    supplied
}

/// The hashes of the signing keys `metadata` publishes, paired with the issuer it names.
///
/// A reader of a provider's metadata checks a message from that provider against the keys
/// the document publishes, so what names one of those KEYS without carrying it is the digest
/// of the key itself ([`key_hash`]) — a property of the key rather than of the certificate
/// around it, so a certificate renewed over one key keeps the key's hash. The issuer is the
/// entity descriptor's `entityID`, which is the name that provider's own messages carry as
/// their `Issuer`.
///
/// A document publishing no readable key (none at all, a body that is not the base64 of a
/// DER certificate, or one that is not an X.509 certificate) pairs nothing: there is no key
/// to identify, so the list is empty.
pub fn saml_key_hashes(metadata: &SamlMetadata) -> Vec<SAMLKeyHashEntry> {
    let key_hashes: Vec<String> = metadata
        .signing_certificates
        .iter()
        .filter_map(|certificate| ::data_encoding::BASE64.decode(certificate.as_bytes()).ok())
        .filter_map(|der| {
            use ::x509_cert::der::{Decode, Encode};
            let certificate = ::x509_cert::Certificate::from_der(&der).ok()?;
            let key = certificate
                .tbs_certificate
                .subject_public_key_info
                .to_der()
                .ok()?;
            Some(key_hash(&key))
        })
        .collect();
    if key_hashes.is_empty() {
        return Vec::new();
    }
    vec![SAMLKeyHashEntry {
        issuer_url: Some(metadata.entity_id.clone()),
        key_hashes: Some(key_hashes),
    }]
}

/// The hash that names the public key whose `SubjectPublicKeyInfo` DER is `key_der`: the
/// SHA-384 of that structure, hex.
///
/// Recorded behaviour fixes the digest: a key hash is 96 lowercase hex characters, a 384-bit
/// digest, and it is derived from the key material the metadata document publishes rather
/// than from the call that reads it — so one document always reports the same hash, and a
/// client comparing a fingerprint of that family against it compares strings of the one
/// width. What is hashed is the published key in the one encoding every reader of the
/// document already has for it: the `SubjectPublicKeyInfo` of the certificate that carries
/// it, algorithm and all, which is the structure a public-key fingerprint is taken over. The
/// hex is [`data_encoding`]'s HEXLOWER, the one coding this crate spells a digest with.
fn key_hash(key_der: &[u8]) -> String {
    let mut hasher = <::sha2::Sha384 as ::sha2::Digest>::new();
    ::sha2::Digest::update(&mut hasher, key_der);
    ::data_encoding::HEXLOWER.encode(&::sha2::Digest::finalize(hasher))
}
