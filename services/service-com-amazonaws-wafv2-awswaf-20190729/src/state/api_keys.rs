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
//! alphabet with its `=` padding. What the envelope seals is the key's own
//! bookkeeping — the layout it was sealed under, the scope it belongs to, when it
//! was created and the key's own unpredictable identifier — and nothing that grows
//! with the definition a caller asked for, so the one width is a short record this
//! service fixes rather than one drawn from the widest list of token domains the
//! model would allow. The definition a key was minted over lives in the key's own
//! record beside it ([`ApiKeyEntry`]), which is where both `ListAPIKeys` and
//! `GetDecryptedAPIKey` report it from; the token is what says the value is one
//! this service minted, in which scope, and that it is not a value a client wrote
//! itself.

use super::*;

/// The nonce an API key's envelope carries, in bytes: the width AES-GCM is
/// specified at.
const API_KEY_NONCE_LEN: usize = 12;

/// The authentication tag AES-GCM appends to the ciphertext, in bytes.
const API_KEY_TAG_LEN: usize = 16;

/// The payload layout this service seals an API key under, so a value sealed under
/// a layout this build does not know is not read as though it were this one.
const API_KEY_PAYLOAD_LAYOUT: u8 = 3;

/// How many bytes of the payload the layout marker takes.
const API_KEY_LAYOUT_LEN: usize = 1;

/// How many bytes of the payload name the scope the key belongs to: ONE, because a
/// `Scope` is one of two values (`smithy_query(query="shape",shape="Scope")` →
/// `CLOUDFRONT | REGIONAL`), so the payload carries the value's code rather than
/// its word and a key's width cannot vary with which scope minted it.
const API_KEY_SCOPE_LEN: usize = 1;

/// The code [`SCOPE_CLOUDFRONT`] is sealed as.
const API_KEY_SCOPE_CODE_CLOUDFRONT: u8 = 1;

/// The code [`SCOPE_REGIONAL`] is sealed as.
const API_KEY_SCOPE_CODE_REGIONAL: u8 = 2;

/// How many bytes the creation instant takes in the payload: the big-endian
/// milliseconds of the `i64` a modelled timestamp is compared at (see
/// [`date_time_millis`]), so every instant — and every key — is the same width.
const API_KEY_CREATED_AT_LEN: usize = ::std::mem::size_of::<i64>();

/// How many bytes the key's own identifier inside the payload carries: the 256 bits
/// of [`fresh_entropy`].
///
/// The identifier is what makes the key UNPREDICTABLE: everything else the payload
/// carries is bookkeeping a reader could guess, so without it a key would be a
/// function of the request and anyone could write one.
const API_KEY_ID_LEN: usize = 32;

/// Where the scope's code sits in the payload.
const API_KEY_SCOPE_AT: usize = API_KEY_LAYOUT_LEN;

/// Where the creation instant sits in the payload.
const API_KEY_CREATED_AT_AT: usize = API_KEY_SCOPE_AT + API_KEY_SCOPE_LEN;

/// Where the key's own identifier sits in the payload.
const API_KEY_ID_AT: usize = API_KEY_CREATED_AT_AT + API_KEY_CREATED_AT_LEN;

/// The ONE width, in bytes, the plaintext of every API key's envelope is sealed at:
/// the key's own bookkeeping and nothing else — the layout marker, the scope's code,
/// the creation instant and the key's identifier, each at a width fixed by what it
/// is.
///
/// No part of this width is drawn from the definition a key was minted over, nor
/// from the maxima the model puts on one: the token domains of a key live in the
/// key's record ([`ApiKeyEntry`]), not inside the token. So a key defined over one
/// short domain and a key defined over the documented maximum of five long ones are
/// the same number of bytes and the same number of characters — a key's width says
/// nothing about the definition it carries — and that one width is a short envelope
/// of this service's own rather than the widest definition the model would permit.
const API_KEY_SEALED_PAYLOAD_LEN: usize =
    API_KEY_LAYOUT_LEN + API_KEY_SCOPE_LEN + API_KEY_CREATED_AT_LEN + API_KEY_ID_LEN;

/// The width, in bytes, of EVERY envelope a mint produces: the nonce, the
/// fixed-width sealed payload and the tag — a short record of the key's own
/// bookkeeping, never a definition padded out to the widest one the model allows.
///
/// A value that decodes to any other size is not one of this service's keys in any
/// scope — a key's width is a property of the key, so there is one width to check
/// against rather than a floor.
pub const API_KEY_ENVELOPE_LEN: usize =
    API_KEY_NONCE_LEN + API_KEY_SEALED_PAYLOAD_LEN + API_KEY_TAG_LEN;

/// How many characters every envelope renders as: base64 carries four characters
/// per three bytes, with its padding completing the last group.
const API_KEY_RENDERED_LEN: usize = API_KEY_ENVELOPE_LEN.div_ceil(3) * 4;

/// The longest value `APIKey` allows
/// (`smithy_query(query="shape",shape="APIKey")` → `@length(1..2048)`).
const MAX_API_KEY_LENGTH: usize = 2048;

/// The one width every key renders at is a width the model allows a key to have.
const _: () = assert!(API_KEY_RENDERED_LEN <= MAX_API_KEY_LENGTH);

/// The longest `TokenDomain` the model allows
/// (`smithy_query(query="shape",shape="TokenDomain")` → `@length(1..253)`).
const MAX_TOKEN_DOMAIN_LENGTH: usize = 253;

/// The one width a key renders at is narrower than the definition a key can be
/// minted over at its widest — [`MAX_TOKEN_DOMAINS_PER_API_KEY`] domains of
/// [`MAX_TOKEN_DOMAIN_LENGTH`] characters — so no key this service mints can be
/// carrying that definition, and the width cannot have been drawn from it.
const _: () =
    assert!(API_KEY_RENDERED_LEN < MAX_TOKEN_DOMAINS_PER_API_KEY * MAX_TOKEN_DOMAIN_LENGTH);

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

/// The code a `Scope` is sealed as, or `None` for a value that names no scope at
/// all — a key is minted in one of the two namespaces the model has, and a value
/// outside them names none.
fn api_key_scope_code(scope: &str) -> Option<u8> {
    if scope == SCOPE_CLOUDFRONT {
        Some(API_KEY_SCOPE_CODE_CLOUDFRONT)
    } else if scope == SCOPE_REGIONAL {
        Some(API_KEY_SCOPE_CODE_REGIONAL)
    } else {
        None
    }
}

/// The `Scope` a sealed code names, or `None` for a code no scope is sealed as.
fn api_key_scope_word(code: u8) -> Option<&'static str> {
    match code {
        API_KEY_SCOPE_CODE_CLOUDFRONT => Some(SCOPE_CLOUDFRONT),
        API_KEY_SCOPE_CODE_REGIONAL => Some(SCOPE_REGIONAL),
        _ => None,
    }
}

/// The bytes an API key's envelope seals, at the ONE width every key is sealed at
/// ([`API_KEY_SEALED_PAYLOAD_LEN`]): the layout marker, the code of the scope the
/// key was defined in, when it was created, and the key's own fresh identifier.
/// `None` when `scope` names neither namespace, which is no key to seal at all.
///
/// Every field is a fixed width, so the payload is one width for every key without
/// anything being padded out to a maximum — and the definition the key was minted
/// over is not among the fields, so no definition can widen it.
fn api_key_payload(scope: &str, created_at_millis: i64) -> Option<Vec<u8>> {
    let identifier = fresh_entropy();
    let mut payload: Vec<u8> = Vec::with_capacity(API_KEY_SEALED_PAYLOAD_LEN);
    payload.push(API_KEY_PAYLOAD_LAYOUT);
    payload.push(api_key_scope_code(scope)?);
    payload.extend_from_slice(&created_at_millis.to_be_bytes());
    payload.extend_from_slice(identifier.get(..API_KEY_ID_LEN)?);
    (payload.len() == API_KEY_SEALED_PAYLOAD_LEN).then_some(payload)
}

/// A fresh API key value for a key defined in `scope` at `created_at_millis`, or
/// `None` when the payload could not be sealed.
///
/// `CreateAPIKey` answers "the generated, encrypted API key", an `APIKey`
/// (`@length(1..2048)`, `@pattern(\S)` —
/// `smithy_query(query="shape",shape="APIKey")`), and the model gives the value no
/// further syntax, because a client does not parse it: it copies the value into its
/// JavaScript CAPTCHA integration and hands it back to this service. So the value is
/// a sealed token — a fresh [`API_KEY_NONCE_LEN`]-byte nonce, the ciphertext of the
/// fixed-width payload and its [`API_KEY_TAG_LEN`]-byte tag, rendered with
/// [`api_key_coder`].
///
/// Every envelope is [`API_KEY_ENVELOPE_LEN`] bytes, because the payload carries the
/// key's own bookkeeping at one width and carries no part of the definition the key
/// was minted over, so every key this service mints is the same short width: a key's
/// width tells a reader nothing about the definition it was minted for — not how
/// many token domains were named, nor how long they are — and it does not grow with
/// the widest definition the model would allow either.
///
/// Each key carries its own identifier ([`API_KEY_ID_LEN`]), so two mints over one
/// definition are two different values and no key can be predicted from the
/// definition it was asked for. A key is nevertheless MINTED only for a definition
/// the scope does not already hold one for
/// ([`ComAmazonawsWafv2Awswaf20190729State::api_key_for_token_domains`]), so a scope
/// holds one key per token-domain set however often a mint is called for it.
pub fn new_api_key(scope: &str, created_at_millis: i64) -> Option<String> {
    use ::aes_gcm::aead::Aead;
    use ::base64::Engine as _;

    let entropy = fresh_entropy();
    let nonce = &entropy[..API_KEY_NONCE_LEN];
    let sealed = api_key_cipher()
        .encrypt(
            ::aes_gcm::Nonce::from_slice(nonce),
            api_key_payload(scope, created_at_millis)?.as_slice(),
        )
        .ok()?;
    let mut envelope: Vec<u8> = Vec::with_capacity(nonce.len() + sealed.len());
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    Some(api_key_coder().encode(envelope))
}

/// What an API key's envelope carries: the bookkeeping the key was sealed with.
///
/// The token domains a key was defined over are NOT in here — they live in the
/// key's own record ([`ApiKeyEntry`]), which is where every read reports them from —
/// so that a key's width stays a property of the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedApiKey {
    /// The `Scope` the key was defined in.
    pub scope: String,
    /// `APIKeySummary$CreationTimestamp` as milliseconds since the Unix epoch.
    pub created_at_millis: i64,
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
    if payload.len() != API_KEY_SEALED_PAYLOAD_LEN {
        return None;
    }
    if *payload.first()? != API_KEY_PAYLOAD_LAYOUT {
        return None;
    }
    let scope = api_key_scope_word(*payload.get(API_KEY_SCOPE_AT)?)?.to_string();
    let created_at_millis = i64::from_be_bytes(
        payload
            .get(API_KEY_CREATED_AT_AT..API_KEY_ID_AT)?
            .try_into()
            .ok()?,
    );
    Some(DecryptedApiKey {
        scope,
        created_at_millis,
    })
}

/// The domain WAF serves the CAPTCHA and challenge JavaScript integration from.
///
/// `ListAPIKeysResponse$ApplicationIntegrationURL` is "the CAPTCHA application
/// integration URL, for use in your JavaScript implementation", and the recorded
/// WAF responses answer an `https` URL under this domain, hosted per region and per
/// account.
const CAPTCHA_SDK_DOMAIN: &str = "captcha-sdk.awswaf.com";

/// How many lowercase hex characters the integration id in
/// [`application_integration_url`] carries.
///
/// The label WAF publishes the CAPTCHA integration under is a SIX-byte id rendered
/// as hex, so the label and the path segment are twelve characters wide. A client
/// that splits the label out of the URL — to build a script tag, a host allow-list
/// entry or a CSP entry — or that recovers the integration id from it, reads an id
/// of that width, so that is the width the URL is composed from.
const CAPTCHA_INTEGRATION_ID_LENGTH: usize = 12;

/// How many bytes of the account digest the id is rendered from: two lowercase hex
/// characters per byte.
const CAPTCHA_INTEGRATION_ID_BYTES: usize = CAPTCHA_INTEGRATION_ID_LENGTH / 2;

/// How many bytes the digest the id is drawn from carries, read off the digest type
/// itself rather than written out, so the bound below stays true of whichever digest
/// [`captcha_integration_id`] renders.
const CAPTCHA_DIGEST_BYTES: usize =
    <<::sha2::Sha256 as ::sha2::digest::OutputSizeUser>::OutputSize as ::sha2::digest::typenum::Unsigned>::USIZE;

/// The id is a whole number of bytes of a digest that HAS that many bytes, so the
/// slice [`captcha_integration_id`] renders is always there and always renders the
/// full width above.
const _: () = assert!(
    CAPTCHA_INTEGRATION_ID_BYTES <= CAPTCHA_DIGEST_BYTES
        && CAPTCHA_INTEGRATION_ID_LENGTH.is_multiple_of(2)
);

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
    let digest = hasher.finalize();
    ::hex::encode(&digest[..CAPTCHA_INTEGRATION_ID_BYTES])
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
///
/// The URL is `https://<id>.<region>.captcha-sdk.awswaf.com/<id>/`: the scheme, the
/// domain, the trailing `/` and the repetition of the SAME
/// [`CAPTCHA_INTEGRATION_ID_LENGTH`]-character id in the leading host label and the
/// single path segment are fixed; the region and the id are the variable parts.
pub fn application_integration_url(region: &str) -> String {
    let integration = captcha_integration_id();
    format!("https://{integration}.{region}.{CAPTCHA_SDK_DOMAIN}/{integration}/")
}
