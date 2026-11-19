// SPDX-License-Identifier: CC0-1.0

//! The CAPTCHA integration surface: the form an `APIKey` VALUE takes, what it
//! decrypts back to, and the application integration URL a client loads the CAPTCHA
//! script from.
//!
//! An API key is "the generated, encrypted API key" a client "can copy for use in
//! its JavaScript CAPTCHA integration" (`APIKeySummary$APIKey`); the key "contains a
//! set of token domains" (`CreateAPIKey`'s own doc), and `GetDecryptedAPIKey`
//! "returns your API key in decrypted form" so a caller "can check the token domains
//! that you have defined for the key". So the value is not an identifier the store
//! happens to key a record by: it is a sealed token minted under a key only this
//! service holds, which is what makes a value handed back a value this service
//! minted rather than one a client wrote itself.
//!
//! The token is rendered at ONE width, in ONE alphabet, for every key: a
//! fixed-width envelope ([`API_KEY_ENVELOPE_LEN`]) over the standard base64
//! alphabet with its `=` padding. The token CARRIES the key's definition — which is
//! what lets `GetDecryptedAPIKey` read the domains back out of it — but it carries
//! it inside a payload sealed at ONE width ([`API_KEY_SEALED_PAYLOAD_LEN`], the
//! width the model's own bounds on a definition imply), so the width of a key is a
//! property of the key and not of the definition a caller asked for. The record
//! beside it ([`ApiKeyEntry`]) is what says the key EXISTS in a scope; the token is
//! what says what it carries.

use super::*;

/// The nonce an API key's envelope carries, in bytes: the width AES-GCM is
/// specified at.
const API_KEY_NONCE_LEN: usize = 12;

/// The authentication tag AES-GCM appends to the ciphertext, in bytes.
const API_KEY_TAG_LEN: usize = 16;

/// The byte that separates the fields of an API key's encrypted payload, and the
/// byte the payload is padded to its fixed width with.
///
/// A `Scope` is one of two words and the other fields are digits and hex, so no
/// field of the payload can contain a NUL — the split that reads the payload back is
/// therefore exact for every key this service seals, and the padding past the last
/// field is read as the empty fields it is.
const API_KEY_FIELD_SEPARATOR: u8 = 0;

/// The payload layout this service seals an API key under, so a value sealed under
/// a layout this build does not know is not read as though it were this one.
const API_KEY_PAYLOAD_LAYOUT: &str = "2";

/// The longest `TokenDomain` the model allows
/// (`smithy_query(query="shape",shape="TokenDomain")` → `@length(1..253)`), which
/// is what the width a payload is sealed at is drawn for.
const MAX_TOKEN_DOMAIN_LENGTH: usize = 253;

/// How many characters the key's own identifier inside the payload carries: the 256
/// bits of [`fresh_entropy`], two characters per byte.
///
/// The identifier is what makes the key UNPREDICTABLE: the rest of the payload is
/// the definition the caller asked for, so without it a key would be a function of
/// the request and anyone could write one.
const API_KEY_ID_LENGTH: usize = 64;

/// The widest decimal rendering of a creation instant in the payload: the 19 digits
/// of `i64::MAX` milliseconds plus a sign.
const API_KEY_CREATED_AT_WIDTH: usize = 20;

/// How many bytes the fields a key carries take at their widest: the layout, the
/// longer of the two `Scope` words, a creation instant, the key's own identifier,
/// and the separator between each pair.
const API_KEY_FIELDS_MAX: usize = API_KEY_PAYLOAD_LAYOUT.len()
    + SCOPE_CLOUDFRONT.len()
    + API_KEY_CREATED_AT_WIDTH
    + API_KEY_ID_LENGTH
    + 3;

/// The ONE width, in bytes, the plaintext of every API key's envelope is sealed at:
/// the fields a key carries plus the WIDEST definition a key can be minted over —
/// [`MAX_TOKEN_DOMAINS_PER_API_KEY`] domains of [`MAX_TOKEN_DOMAIN_LENGTH`]
/// characters, each behind its separator.
///
/// Every payload is padded to this width with [`API_KEY_FIELD_SEPARATOR`] before it
/// is sealed, so a key defined over one short domain and a key defined over five
/// long ones are the same number of bytes and the same number of characters: the
/// width of a key says nothing about the definition it carries. The rendering a
/// payload this wide produces is well inside the 2048 characters `APIKey` allows,
/// which is the bound the model draws for a key that carries its own definition.
const API_KEY_SEALED_PAYLOAD_LEN: usize =
    API_KEY_FIELDS_MAX + MAX_TOKEN_DOMAINS_PER_API_KEY * (1 + MAX_TOKEN_DOMAIN_LENGTH);

/// The widest `Scope` word is the one [`API_KEY_FIELDS_MAX`] is drawn from, so that
/// figure stays a true maximum whatever the two words are.
const _: () = assert!(SCOPE_CLOUDFRONT.len() >= SCOPE_REGIONAL.len());

/// The width, in bytes, of EVERY envelope a mint produces: the nonce, the
/// fixed-width sealed payload and the tag.
///
/// A value that decodes to any other size is not one of this service's keys in any
/// scope — a key's width is a property of the key, so there is one width to check
/// against rather than a floor.
pub const API_KEY_ENVELOPE_LEN: usize =
    API_KEY_NONCE_LEN + API_KEY_SEALED_PAYLOAD_LEN + API_KEY_TAG_LEN;

/// How many characters an envelope renders as: base64 carries four characters per
/// three bytes, padded to the group.
const API_KEY_RENDERED_LEN: usize = API_KEY_ENVELOPE_LEN.div_ceil(3) * 4;

/// The longest value `APIKey` allows
/// (`smithy_query(query="shape",shape="APIKey")` → `@length(1..2048)`).
const MAX_API_KEY_LENGTH: usize = 2048;

/// The one width every key renders at is a width the model allows a key to have.
const _: () = assert!(API_KEY_RENDERED_LEN <= MAX_API_KEY_LENGTH);

/// The AEAD an API key's payload is sealed with, keyed for the lifetime of the
/// process.
///
/// The 256-bit key is drawn once from [`fresh_entropy`] and never leaves this
/// module: it is not a caller's credential and is in no response, no log and no
/// file — it exists so that the token a client is handed is genuinely encrypted,
/// and so that a key handed back is a key this service minted rather than one the
/// client wrote itself. The AEAD is the maintained `aes-gcm` implementation;
/// nothing here hand-rolls one.
fn api_key_cipher() -> &'static ::aes_gcm::Aes256Gcm {
    use ::aes_gcm::KeyInit;

    static CIPHER: ::std::sync::OnceLock<::aes_gcm::Aes256Gcm> = ::std::sync::OnceLock::new();
    CIPHER.get_or_init(|| {
        let secret = fresh_entropy();
        ::aes_gcm::Aes256Gcm::new(::aes_gcm::Key::<::aes_gcm::Aes256Gcm>::from_slice(&secret))
    })
}

/// The coder an API key's envelope is written with: base64 over the standard
/// alphabet (`A-Za-z0-9+/`) with its `=` padding — the rendering a generated,
/// encrypted key is handed out in, and every character of it non-whitespace, which
/// is `APIKey`'s own `pattern(\S)`.
fn api_key_coder() -> &'static ::base64::engine::general_purpose::GeneralPurpose {
    &::base64::engine::general_purpose::STANDARD
}

/// The bytes an API key's envelope seals, at the ONE width every key is sealed at:
/// the layout, the scope the key was defined in, when it was created, the key's own
/// identifier and the token domains it carries, in the order they are stored in
/// ([`normalized_token_domains`]), padded to [`API_KEY_SEALED_PAYLOAD_LEN`] with
/// [`API_KEY_FIELD_SEPARATOR`]; `None` when a field carries the separator itself or
/// the fields do not fit that width, neither of which a definition the model allows
/// can do.
///
/// The padding is what keeps the ENVELOPE one width: the domains travel inside the
/// key, and the key is still the same width whatever they are.
fn api_key_payload(
    scope: &str,
    token_domains: &[String],
    created_at_millis: i64,
) -> Option<Vec<u8>> {
    let mut payload: Vec<u8> = Vec::with_capacity(API_KEY_SEALED_PAYLOAD_LEN);
    for field in [
        API_KEY_PAYLOAD_LAYOUT.to_string(),
        scope.to_string(),
        created_at_millis.to_string(),
        ::hex::encode(fresh_entropy()),
    ]
    .iter()
    .chain(token_domains.iter())
    {
        // A field carrying the separator itself would be read back as two fields, so
        // it is not sealed at all: the split that reads a payload back stays exact
        // for every key this service mints.
        if field.as_bytes().contains(&API_KEY_FIELD_SEPARATOR) {
            return None;
        }
        if !payload.is_empty() {
            payload.push(API_KEY_FIELD_SEPARATOR);
        }
        payload.extend_from_slice(field.as_bytes());
    }
    if payload.len() > API_KEY_SEALED_PAYLOAD_LEN {
        return None;
    }
    payload.resize(API_KEY_SEALED_PAYLOAD_LEN, API_KEY_FIELD_SEPARATOR);
    Some(payload)
}

/// A fresh API key value for a key defined in `scope` over `token_domains` at
/// `created_at_millis`, or `None` when the payload could not be sealed.
///
/// `CreateAPIKey` answers "the generated, encrypted API key", an `APIKey`
/// (`@length(1..2048)`, `@pattern(\S)` —
/// `smithy_query(query="shape",shape="APIKey")`), and the model gives the value no
/// further syntax, because a client does not parse it: it copies the value into its
/// JavaScript CAPTCHA integration and hands it back to this service. What the model
/// does say is what the value IS — an encrypted key that contains the token domains
/// it was defined over — so the value is that definition sealed: a fresh
/// [`API_KEY_NONCE_LEN`]-byte nonce, the ciphertext of the fixed-width payload and
/// its [`API_KEY_TAG_LEN`]-byte tag, rendered with [`api_key_coder`].
///
/// Every envelope is [`API_KEY_ENVELOPE_LEN`] bytes, because the payload is sealed at
/// one width whatever it carries, so every key this service mints is the same width:
/// a key's width tells a reader nothing about the definition it was minted for — not
/// how many token domains were named, nor how long they are.
///
/// Each key carries its own identifier ([`API_KEY_ID_LENGTH`]), so two mints over
/// one definition are two different values and no key can be predicted from the
/// definition it was asked for. A key is nevertheless MINTED only for a definition
/// the scope does not already hold one for
/// ([`ComAmazonawsWafv2Awswaf20190729State::api_key_for_token_domains`]), so a scope
/// holds one key per token-domain set however often a mint is called for it.
pub fn new_api_key(
    scope: &str,
    token_domains: &[String],
    created_at_millis: i64,
) -> Option<String> {
    use ::aes_gcm::aead::Aead;
    use ::base64::Engine as _;

    let entropy = fresh_entropy();
    let nonce = &entropy[..API_KEY_NONCE_LEN];
    let sealed = api_key_cipher()
        .encrypt(
            ::aes_gcm::Nonce::from_slice(nonce),
            api_key_payload(scope, token_domains, created_at_millis)?.as_slice(),
        )
        .ok()?;
    let mut envelope: Vec<u8> = Vec::with_capacity(nonce.len() + sealed.len());
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    Some(api_key_coder().encode(envelope))
}

/// What an API key's envelope carries: the definition the key was sealed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedApiKey {
    /// The `Scope` the key was defined in.
    pub scope: String,
    /// `APIKeySummary$CreationTimestamp` as milliseconds since the Unix epoch.
    pub created_at_millis: i64,
    /// "The token domains that are defined in this API key"
    /// (`GetDecryptedAPIKeyResponse$TokenDomains`), in the order the key carries
    /// them.
    pub token_domains: Vec<String>,
}

/// The envelope `value` decodes to, or `None` when it is not one at all: not of the
/// one width a key is rendered at ([`API_KEY_RENDERED_LEN`]), not padded base64 over
/// the key alphabet, or not [`API_KEY_ENVELOPE_LEN`] bytes wide. The width is read
/// first, so a value of any other length is answered without decoding it.
fn api_key_envelope(value: &str) -> Option<Vec<u8>> {
    use ::base64::Engine as _;

    if value.len() != API_KEY_RENDERED_LEN {
        return None;
    }
    let decoded = api_key_coder().decode(value).ok()?;
    (decoded.len() == API_KEY_ENVELOPE_LEN).then_some(decoded)
}

/// `true` when `value` has the form this service mints an API key in: an envelope
/// of the one width every key is rendered at, as [`new_api_key`] renders it.
///
/// A key is "the generated, encrypted API key", so a value that does not even
/// decode to an envelope is not a key WAF could decrypt, whatever any scope holds —
/// which is the difference between a value that is not a key and a well-formed key
/// naming nothing stored. This judges the FORM only: a well-formed envelope this
/// service did not seal is a key the scope does not hold, not a malformed one.
pub fn is_api_key_shaped(value: &str) -> bool {
    api_key_envelope(value).is_some()
}

/// What `value` decrypts to, or `None` when it is not a key this service sealed — a
/// value of another form, one sealed under another payload layout, or one whose
/// authentication tag does not hold.
pub fn decrypt_api_key(value: &str) -> Option<DecryptedApiKey> {
    use ::aes_gcm::aead::Aead;

    let envelope = api_key_envelope(value)?;
    let (nonce, sealed) = envelope.split_at(API_KEY_NONCE_LEN);
    let payload = api_key_cipher()
        .decrypt(::aes_gcm::Nonce::from_slice(nonce), sealed)
        .ok()?;
    let mut fields = payload.split(|byte| *byte == API_KEY_FIELD_SEPARATOR);
    if fields.next()? != API_KEY_PAYLOAD_LAYOUT.as_bytes() {
        return None;
    }
    let scope = ::std::str::from_utf8(fields.next()?).ok()?.to_string();
    let created_at_millis = ::std::str::from_utf8(fields.next()?)
        .ok()?
        .parse::<i64>()
        .ok()?;
    // The key's own identifier is not part of the definition: it is read for its
    // presence and its width, and then read past.
    if fields.next()?.len() != API_KEY_ID_LENGTH {
        return None;
    }
    // What is left is the definition and then the padding that holds every key to
    // one width. A `TokenDomain` is `@length(1..253)`, so no domain is empty and the
    // padding is exactly the empty fields.
    let token_domains = fields
        .filter(|domain| !domain.is_empty())
        .map(|domain| ::std::str::from_utf8(domain).map(str::to_string))
        .collect::<Result<Vec<String>, _>>()
        .ok()?;
    Some(DecryptedApiKey {
        scope,
        created_at_millis,
        token_domains,
    })
}

/// The domain WAF serves the CAPTCHA and challenge JavaScript integration from.
///
/// `ListAPIKeysResponse$ApplicationIntegrationURL` is "the CAPTCHA application
/// integration URL, for use in your JavaScript implementation", and the recorded
/// WAF responses answer an `https` URL under this domain, hosted per region and per
/// account.
const CAPTCHA_SDK_DOMAIN: &str = "captcha-sdk.awswaf.com";

/// How many hex characters the integration id in [`application_integration_url`]
/// carries: the 128 bits of the digest it is rendered from, two characters per
/// byte.
const CAPTCHA_INTEGRATION_ID_LENGTH: usize = 32;

/// The id of this account's CAPTCHA integration: the host label, and the path
/// segment, of [`application_integration_url`].
///
/// An account's integration is not a resource a caller creates — nothing in this
/// model mints or names one — so the id is a PROPERTY of the account: the same
/// value on every call, derived from the account the emulator serves (the `sha2`
/// digest this crate renders every other derived value with), never from the
/// request that happens to ask for the URL.
fn captcha_integration_id() -> String {
    use ::sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(CAPTCHA_SDK_DOMAIN.as_bytes());
    hasher.update(::engine::emulator_account_id().as_bytes());
    let digest = ::hex::encode(hasher.finalize());
    digest
        .chars()
        .take(CAPTCHA_INTEGRATION_ID_LENGTH)
        .collect()
}

/// The `ApplicationIntegrationURL` this account's clients load the CAPTCHA
/// JavaScript integration from, at the endpoint region `region`.
///
/// "The CAPTCHA application integration URL, for use in your JavaScript
/// implementation" (`ListAPIKeysResponse$ApplicationIntegrationURL`): WAF hosts the
/// integration per account and per region and answers its URL whatever keys the
/// account holds — a client reads the URL from this listing BEFORE it mints its
/// first key — so it is reported from the account and the region alone, and is the
/// same URL on every call.
pub fn application_integration_url(region: &str) -> String {
    let integration = captcha_integration_id();
    format!("https://{integration}.{region}.{CAPTCHA_SDK_DOMAIN}/{integration}/")
}
