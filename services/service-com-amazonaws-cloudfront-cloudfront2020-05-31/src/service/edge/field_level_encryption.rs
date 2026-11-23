// SPDX-License-Identifier: CC0-1.0

//! FIELD-LEVEL ENCRYPTION: which fields of one viewer request body the edge replaces with
//! ciphertext before it forwards the request to the origin, and under whose key.
//!
//! `CacheBehavior$FieldLevelEncryptionId` is "the value of `ID` for the field-level encryption
//! configuration that you want CloudFront to use for encrypting specific fields of data for
//! this cache behavior", so a behaviour that names one is a TRANSFORMATION of the request on
//! its way to the origin, and this module is the whole of it:
//!
//! 1. the configuration the behaviour names decides WHICH PROFILE this request is encrypted
//!    under. `QueryArgProfileConfig` maps the value the viewer passed in the
//!    [`PROFILE_QUERY_ARG`] query argument to a profile, and `ContentTypeProfileConfig` maps
//!    the request's own content type to one — "profiles to use as by default in a request if a
//!    query argument doesn't specify a profile to use" — so a request that names a profile is
//!    encrypted under that one and every other request under the one its content type selects;
//! 2. a request NO profile is selected for is decided by the flag of the mapping that did not
//!    resolve: `ForwardWhenQueryArgProfileIsUnknown` is "set if you want a request to be
//!    forwarded to the origin even if the profile specified by the field-level encryption query
//!    argument ... is unknown", and `ForwardWhenContentTypeIsUnknown` — "if true, content is
//!    forwarded without being encrypted when the content type is unknown. If false (the
//!    default), an error is returned when the content type is unknown" — says the same for the
//!    content type. So the request travels with its fields as the viewer sent them, or it is
//!    refused and the origin never sees it ([`Forwarded::Refused`]);
//! 3. the profile's `EncryptionEntities` decide WHICH FIELDS are encrypted and under which
//!    key: each entity carries "the public key associated with a set of field-level encryption
//!    patterns, to be used when encrypting the fields that match the patterns", and a pattern
//!    is a field name or "any beginning characters followed by a wildcard (*)", compared
//!    case-sensitively ([`designating_entity`]);
//! 4. every field the viewer sent whose name a pattern designates travels to the origin as the
//!    BASE64 of its value encrypted under that entity's public key, and every other field — and
//!    the names themselves — travel exactly as the viewer sent them ([`encrypted_body`]). The
//!    body is read in the one format `ContentTypeProfile$Format` carries,
//!    [`URL_ENCODED`], by the crate that owns that format.
//!
//! What this leaves the origin is the point of the mechanism: the designated field is
//! unreadable to it, since the private half of the key pair is the client's and this emulator
//! never holds one, while the rest of the request is untouched.
//!
//! The encryption is RSA-OAEP with SHA-256 as both digest and MGF1 hash — the transformation
//! CloudFront's own published decryption procedure applies the profile's private key under —
//! and the ciphertext is base64-encoded in the standard alphabet before it is put back into the
//! body.

use ::base64::Engine as _;

use crate::ops::types::{
    ContentTypeProfileConfig, FieldLevelEncryptionConfig, FieldLevelEncryptionProfileConfig,
    EncryptionEntity, QueryArgProfileConfig,
};
use crate::state::ComAmazonawsCloudfrontCloudfront20200531State;

/// The query argument a viewer names the profile to encrypt its fields under in:
/// `fle-profile`, the name `ForwardWhenQueryArgProfileIsUnknown` states — "the profile
/// specified by the field-level encryption query argument, `fle-profile`".
const PROFILE_QUERY_ARG: &str = "fle-profile";

/// The one value `ContentTypeProfile$Format` carries (`smithy_query(query="shape",
/// shape="Format")`): the request body is read as `application/x-www-form-urlencoded`.
const URL_ENCODED: &str = "URLEncoded";

/// The character a field pattern names "any beginning characters followed by a wildcard" with.
const FIELD_PATTERN_WILDCARD: char = '*';

/// The most bytes of request body field-level encryption reads, from the published CloudFront
/// quota "maximum length of a request body when field-level encryption is configured": 1 MB.
///
/// A viewer chooses the length, and every field of the body is parsed and encrypted, so a body
/// past the quota is not encrypted at all — and a configuration that asked for encryption is
/// not forwarded unencrypted either, so such a request is refused.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The most fields of one body that are encrypted, from the published CloudFront quota
/// "maximum number of fields per request body that can be encrypted": 10.
const MAX_ENCRYPTED_FIELDS: usize = 10;

/// The viewer request body field-level encryption is decided over: what the viewer said the
/// body IS, what profile it named, and the bytes themselves.
#[derive(Debug)]
pub struct Submitted<'a> {
    /// The request's own `Content-Type`, as the viewer stated it, which is what a content
    /// type-profile mapping is read against.
    pub content_type: &'a str,
    /// The request's query string, which carries [`PROFILE_QUERY_ARG`] when the viewer named a
    /// profile itself.
    pub query: &'a str,
    /// The body the viewer sent, which is what the origin receives when nothing designates a
    /// field of it.
    pub body: &'a [u8],
}

/// What the behaviour's field-level encryption configuration makes of one request body.
#[derive(Debug, PartialEq, Eq)]
pub enum Forwarded {
    /// The body the ORIGIN is asked with: the viewer's own, with every designated field's value
    /// replaced by the base64 of that value encrypted under the profile's public key.
    Body(Vec<u8>),
    /// CloudFront does not forward this request at all: no profile is selected for it and the
    /// configuration's own flag says a request it cannot encrypt is not forwarded, so the
    /// origin never sees the fields.
    Refused,
}

/// What the field-level encryption configuration `id` makes of `submitted`, or `None` when the
/// request travels to the origin exactly as the viewer sent it.
///
/// `store` is read for the configuration, the profile and the public keys, so the fields one
/// request encrypts are the ones the records designate AT THIS INSTANT — an `UpdateFieldLevel
/// EncryptionProfile` that adds a field pattern designates that field from the next request on.
pub fn applied(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    id: Option<&str>,
    submitted: &Submitted<'_>,
) -> Option<Forwarded> {
    let id = id.filter(|id| !id.is_empty())?;
    // A request carrying no body designates nothing: there are no fields to encrypt, whatever
    // the behaviour configures.
    if submitted.body.is_empty() {
        return None;
    }
    let config = &store.field_level_encryption_config(id)?.config;
    let profile_id = match selected_profile(config, submitted) {
        Selected::Profile(profile_id) => profile_id,
        Selected::Forward => return None,
        Selected::Refuse => return Some(Forwarded::Refused),
    };
    // A body past the quota is not read at all, and a configuration that designates fields of
    // it does not forward it unencrypted either.
    if submitted.body.len() > MAX_BODY_BYTES {
        ::tracing::debug!(
            "this request's body is longer than field-level encryption reads, so its \
             designated fields cannot be encrypted and the request is not forwarded"
        );
        return Some(Forwarded::Refused);
    }
    let Some(profile) = store
        .field_level_encryption_profile(&profile_id)
        .map(|stored| stored.config.clone())
    else {
        // The configuration names a profile this account no longer holds, so there is no key to
        // encrypt the designated fields with and nothing says which fields they are.
        ::tracing::debug!(
            "this field-level encryption configuration names a profile this service does not \
             hold, so the request's fields cannot be encrypted and it is not forwarded"
        );
        return Some(Forwarded::Refused);
    };
    let keys = Keys { store };
    match encrypted_body(&profile, &keys, submitted.body) {
        Some(body) => Some(Forwarded::Body(body)),
        None => Some(Forwarded::Refused),
    }
}

/// WHICH profile one request's fields are encrypted under.
#[derive(Debug, PartialEq, Eq)]
enum Selected {
    /// The field-level encryption profile the request resolved to.
    Profile(String),
    /// No profile is selected and the configuration forwards the request as the viewer sent it.
    Forward,
    /// No profile is selected and the configuration does not forward a request it cannot
    /// encrypt.
    Refuse,
}

/// The profile `config` selects for `submitted`.
///
/// The query argument is read FIRST, for a configuration that carries a query arg-profile
/// mapping at all: `ContentTypeProfileConfig` holds the "profiles to use as by default in a
/// request if a query argument doesn't specify a profile to use", so a request that specifies
/// one is encrypted under that profile and the content type decides every other request. A
/// configuration that carries neither mapping designates nothing.
fn selected_profile(config: &FieldLevelEncryptionConfig, submitted: &Submitted<'_>) -> Selected {
    if let Some(query_args) = config.query_arg_profile_config.as_ref() {
        if let Some(named) = named_profile(submitted.query) {
            return by_query_arg(query_args, &named);
        }
    }
    match config.content_type_profile_config.as_ref() {
        Some(content_types) => by_content_type(content_types, submitted.content_type),
        // A configuration whose mappings are both absent selects no profile for any request,
        // and designates no field of it: there is nothing it says to encrypt.
        None => Selected::Forward,
    }
}

/// The value of the [`PROFILE_QUERY_ARG`] parameter of `query`, or `None` when the viewer named
/// no profile.
///
/// Read by the crate that owns the `application/x-www-form-urlencoded` format a query string is
/// spelled in, so a percent-encoded parameter name is the name it decodes to.
fn named_profile(query: &str) -> Option<String> {
    ::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _value)| name == PROFILE_QUERY_ARG)
        .map(|(_name, value)| value.to_string())
}

/// The profile the query arg-profile mapping selects for the profile the viewer `named`.
///
/// `QueryArgProfile$QueryArg` is the "query argument for field-level encryption query
/// argument-profile mapping" — the value the viewer passes — and `ProfileId` the profile it
/// stands for. A value the mapping does not carry is the unknown profile its own flag decides.
fn by_query_arg(config: &QueryArgProfileConfig, named: &str) -> Selected {
    let resolved = config
        .query_arg_profiles
        .as_ref()
        .and_then(|profiles| profiles.items.as_ref())
        .into_iter()
        .flatten()
        .find(|profile| profile.query_arg == named)
        .map(|profile| profile.profile_id.clone());
    match resolved {
        Some(profile_id) => Selected::Profile(profile_id),
        None if config.forward_when_query_arg_profile_is_unknown => Selected::Forward,
        None => Selected::Refuse,
    }
}

/// The profile the content type-profile mapping selects for a body the viewer described as
/// `content_type`.
///
/// `ContentTypeProfile$ContentType` is "the content type for a field-level encryption content
/// type-profile mapping", matched against the request's own MEDIA TYPE — the type before any
/// parameter, which an HTTP media type is case-insensitive in. A content type the mapping does
/// not carry is the unknown content type `ForwardWhenContentTypeIsUnknown` decides.
///
/// Two readings of a content type the mapping DOES carry:
///
/// * a mapping under a `Format` this service cannot read designates a body it cannot parse, so
///   its fields cannot be encrypted and the request is not forwarded with them in the clear —
///   the configuration asked for this content type to be encrypted, and that is not the unknown
///   content type the flag above governs. `URLEncoded` is the only value the model carries, so
///   this is a mapping no create of this service admits today;
/// * a mapping that names NO profile designates no field by itself: the model leaves
///   `ContentTypeProfile$ProfileId` optional, and the create's own refusal of a configuration
///   where "no default profile id is given in any content type profile" unless a query argument
///   mapping names one ([`crate::state::query_arg_profile_missing`]) is what such a
///   configuration is for — the profile then comes from the query argument, and a request that
///   names none has no field designated.
fn by_content_type(config: &ContentTypeProfileConfig, content_type: &str) -> Selected {
    let media_type = media_type(content_type);
    let mapped = config
        .content_type_profiles
        .as_ref()
        .and_then(|profiles| profiles.items.as_ref())
        .into_iter()
        .flatten()
        .find(|profile| profile.content_type.eq_ignore_ascii_case(media_type));
    match mapped {
        Some(profile) if !profile.format.eq_ignore_ascii_case(URL_ENCODED) => Selected::Refuse,
        Some(profile) => match profile.profile_id.as_deref() {
            Some(profile_id) if !profile_id.is_empty() => Selected::Profile(profile_id.to_string()),
            _ => Selected::Forward,
        },
        None if config.forward_when_content_type_is_unknown => Selected::Forward,
        None => Selected::Refuse,
    }
}

/// The MEDIA TYPE of a `Content-Type` field value: the type and subtype, without the parameters
/// a sender may state after them (RFC 9110 section 8.3).
fn media_type(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
}

/// The public keys an encryption entity's `PublicKeyId` resolves to, read from the store.
struct Keys<'s> {
    store: &'s ComAmazonawsCloudfrontCloudfront20200531State,
}

impl Keys<'_> {
    /// The RSA public key `public_key_id` names, as the X.509 `SubjectPublicKeyInfo` DER the
    /// encryption takes, or `None` when this account holds no such key or its material is not
    /// an RSA public key.
    ///
    /// The material is read the one way this crate reads it
    /// ([`crate::state::rsa_public_key_spki_der`]), so the key a field is encrypted under is the
    /// one `CreatePublicKey` admitted.
    fn material(&self, public_key_id: &str) -> Option<Vec<u8>> {
        let encoded = &self.store.public_key(public_key_id)?.config.encoded_key;
        crate::state::rsa_public_key_spki_der(encoded)
    }
}

/// `body` with every field `profile` designates replaced by the base64 of its value encrypted
/// under the designating entity's public key, or `None` when a designated field could not be
/// encrypted at all.
///
/// The body is `application/x-www-form-urlencoded` ([`URL_ENCODED`]), so it is a list of
/// `name=value` pairs separated by `&`. Each pair is read by the crate that owns that format —
/// the pattern is matched against the name the pair DECODES to, field names being percent- and
/// plus-encoded on the wire — and a pair no pattern designates is carried over byte for byte,
/// so a field CloudFront does not encrypt reaches the origin exactly as the viewer spelled it.
/// A DESIGNATED pair is written back in that same format from the name and the ciphertext, so a
/// pair the viewer spelled with no `=` at all (an empty value) carries one once its value is
/// the ciphertext there is now to carry.
fn encrypted_body(
    profile: &FieldLevelEncryptionProfileConfig,
    keys: &Keys<'_>,
    body: &[u8],
) -> Option<Vec<u8>> {
    let mut encrypted: Vec<Vec<u8>> = Vec::new();
    let mut count = 0usize;
    for pair in body.split(|byte| *byte == b'&') {
        let Some((name, value)) = ::form_urlencoded::parse(pair)
            .next()
            .map(|(name, value)| (name.to_string(), value.to_string()))
        else {
            encrypted.push(pair.to_vec());
            continue;
        };
        let Some(entity) = designating_entity(profile, &name) else {
            encrypted.push(pair.to_vec());
            continue;
        };
        count = count.saturating_add(1);
        if count > MAX_ENCRYPTED_FIELDS {
            ::tracing::debug!(
                "this request designates more fields for encryption than field-level \
                 encryption encrypts, so it is not forwarded"
            );
            return None;
        }
        let material = keys.material(&entity.public_key_id)?;
        let ciphertext = encrypt(&material, value.as_bytes())?;
        // The pair is written back in the format the body is in, so the base64 the origin
        // reads out of the field is the ciphertext and nothing of the encoding.
        let rewritten = ::form_urlencoded::Serializer::new(String::new())
            .append_pair(&name, &ciphertext)
            .finish();
        encrypted.push(rewritten.into_bytes());
    }
    Some(encrypted.join(&b'&'))
}

/// The encryption entity of `profile` whose field patterns designate the field `name`, or
/// `None` when no pattern of the profile does.
///
/// `EncryptionEntity$FieldPatterns`: "You can provide the full field name, or any beginning
/// characters followed by a wildcard (*) ... Note that field patterns are case-sensitive." So a
/// pattern is either the whole name or a prefix of it, compared byte for byte. The patterns of
/// one entity cannot overlap (the create refuses a profile whose do), so the first entity whose
/// patterns designate the name is the one key that encrypts it.
fn designating_entity<'p>(
    profile: &'p FieldLevelEncryptionProfileConfig,
    name: &str,
) -> Option<&'p EncryptionEntity> {
    profile
        .encryption_entities
        .items
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|entity| {
            entity
                .field_patterns
                .items
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|pattern| designates(pattern, name))
        })
}

/// Whether the field pattern `pattern` designates the field `name`.
fn designates(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix(FIELD_PATTERN_WILDCARD) {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// `plaintext` encrypted under the RSA public key `material`, base64-encoded.
///
/// RSA-OAEP with SHA-256 as the digest and MGF1 hash: the transformation CloudFront's published
/// procedure for decrypting a field at the origin applies the profile's private key under. A
/// plaintext longer than the key can encrypt in one operation, and material no RSA encryption
/// accepts, produce nothing — such a field cannot be encrypted at all.
fn encrypt(material: &[u8], plaintext: &[u8]) -> Option<String> {
    use ::aws_lc_rs::rsa::{
        OaepPublicEncryptingKey, PublicEncryptingKey, OAEP_SHA256_MGF1SHA256,
    };

    let key = PublicEncryptingKey::from_der(material).ok()?;
    let key = OaepPublicEncryptingKey::new(key).ok()?;
    let mut ciphertext = vec![0u8; key.ciphertext_size()];
    let written = key
        .encrypt(&OAEP_SHA256_MGF1SHA256, plaintext, &mut ciphertext, None)
        .ok()?;
    Some(::base64::engine::general_purpose::STANDARD.encode(written))
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns: which pattern designates which field name, which
    //! profile a content type-profile mapping selects, and how a `Content-Type` field value is
    //! read down to its media type. What one configuration does to a request end to end is
    //! driven through the wire in `tests/all/edge_field_level_encryption.rs`.

    use super::*;
    use crate::ops::types::{ContentTypeProfile, ContentTypeProfiles};

    /// The mapping of ONE content type to `profile_id` under `format`.
    fn mapping(content_type: &str, format: &str, profile_id: Option<&str>) -> ContentTypeProfiles {
        ContentTypeProfiles {
            quantity: 1,
            items: Some(vec![ContentTypeProfile {
                format: format.to_string(),
                profile_id: profile_id.map(str::to_string),
                content_type: content_type.to_string(),
            }]),
        }
    }

    #[test]
    fn a_content_type_mapping_selects_a_profile_forwards_or_refuses() {
        /// One row: the mapping the configuration carries, its flag, and the content type the
        /// request stated.
        struct Row {
            case: &'static str,
            /// The `ContentType` the one mapping names.
            mapped: &'static str,
            /// Its `Format`.
            format: &'static str,
            /// Its `ProfileId`, which the model leaves optional.
            profile_id: Option<&'static str>,
            forward_when_unknown: bool,
            /// The `Content-Type` the request stated.
            stated: &'static str,
            expected: Selected,
        }
        // The rows span what the mapping can say about one request's content type: the type
        // mapped to a profile in the one `Format` the model carries (bare, and with the
        // parameters a sender may state after it), the same type mapped under a format this
        // service cannot read, a type mapped to no profile at all (which the create admits
        // beside a query argument mapping that names one), and a type the mapping does not
        // carry under both settings of `ForwardWhenContentTypeIsUnknown`.
        let form = "application/x-www-form-urlencoded";
        let rows = [
            Row {
                case: "the mapped type, in the format the model carries",
                mapped: form,
                format: URL_ENCODED,
                profile_id: Some("P1"),
                forward_when_unknown: false,
                stated: form,
                expected: Selected::Profile("P1".to_string()),
            },
            Row {
                case: "the mapped type, with the parameters a sender may state after it",
                mapped: form,
                format: URL_ENCODED,
                profile_id: Some("P1"),
                forward_when_unknown: false,
                stated: "Application/X-WWW-Form-Urlencoded; charset=UTF-8",
                expected: Selected::Profile("P1".to_string()),
            },
            Row {
                case: "the mapped type, under a format this service cannot read",
                mapped: form,
                format: "SomeFutureFormat",
                profile_id: Some("P1"),
                // Forwarding an UNKNOWN content type says nothing about a type the mapping
                // carries, so the flag does not rescue this one.
                forward_when_unknown: true,
                stated: form,
                expected: Selected::Refuse,
            },
            Row {
                case: "the mapped type, naming no profile of its own",
                mapped: form,
                format: URL_ENCODED,
                profile_id: None,
                forward_when_unknown: false,
                stated: form,
                expected: Selected::Forward,
            },
            Row {
                case: "a type the mapping does not carry, forwarded",
                mapped: form,
                format: URL_ENCODED,
                profile_id: Some("P1"),
                forward_when_unknown: true,
                stated: "application/json",
                expected: Selected::Forward,
            },
            Row {
                case: "a type the mapping does not carry, refused",
                mapped: form,
                format: URL_ENCODED,
                profile_id: Some("P1"),
                forward_when_unknown: false,
                stated: "application/json",
                expected: Selected::Refuse,
            },
        ];
        for row in rows {
            let config = ContentTypeProfileConfig {
                forward_when_content_type_is_unknown: row.forward_when_unknown,
                content_type_profiles: Some(mapping(row.mapped, row.format, row.profile_id)),
            };
            let actual = by_content_type(&config, row.stated);
            assert_eq!(
                actual, row.expected,
                "case {:?}: expected {:?}, got {actual:?}",
                row.case, row.expected
            );
        }
    }

    #[test]
    fn a_pattern_designates_the_name_it_spells_or_the_names_it_prefixes() {
        // (pattern, name, designated) — the sentence on `FieldPatterns` is the source: the full
        // field name, or any beginning characters followed by `*`, case-sensitive.
        let rows = [
            ("secret", "secret", true),
            ("secret", "secret_field", false),
            ("secret", "Secret", false),
            ("secret*", "secret", true),
            ("secret*", "secret_field", true),
            ("secret*", "sec", false),
            ("secret*", "SECRET_FIELD", false),
            ("*", "anything", true),
            ("", "", true),
        ];
        for (pattern, name, designated) in rows {
            assert_eq!(
                designates(pattern, name),
                designated,
                "case {pattern:?} against {name:?}: expected {designated}, got {}",
                designates(pattern, name)
            );
        }
    }

    #[test]
    fn the_material_a_public_key_create_admits_encrypts_a_field() {
        use ::aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
        use ::aws_lc_rs::signature::KeyPair as _;

        // The material a client submits to `CreatePublicKey`: a PEM-enveloped RSA public key,
        // read down to the DER the encryption takes by the one reading of it this crate has.
        let key = ::aws_lc_rs::signature::RsaKeyPair::generate(
            ::aws_lc_rs::rsa::KeySize::Rsa2048,
        )
        .expect("the provider generates a 2048-bit RSA pair");
        let public: PublicKeyX509Der<'_> =
            AsDer::as_der(key.public_key()).expect("the public half encodes as an SPKI");
        let encoded_key = ::pem::encode(&::pem::Pem::new(
            "PUBLIC KEY".to_string(),
            public.as_ref().to_vec(),
        ));
        let material = crate::state::rsa_public_key_spki_der(&encoded_key)
            .expect("a generated RSA public key is one this crate reads");
        // The rows span what one field's value can be: text, the empty value, non-ASCII bytes
        // and the longest plaintext the scheme encrypts under a 2048-bit key
        // (`k - 2 * hLen - 2` of RFC 8017 section 7.1.1), plus one byte past it, which no
        // single encryption can carry.
        let longest = "p".repeat(256 - 2 * 32 - 2);
        let too_long = "p".repeat(256 - 2 * 32 - 1);
        let rows = [
            ("a plain value", "4111111111111111", true),
            ("the empty value", "", true),
            ("a value that is not ASCII", "p\u{e4}ssw\u{f6}rd-\u{2602}", true),
            ("the longest value one encryption carries", longest.as_str(), true),
            ("one byte past it", too_long.as_str(), false),
        ];
        for (case, plaintext, encrypted) in rows {
            let actual = encrypt(&material, plaintext.as_bytes());
            assert_eq!(
                actual.is_some(),
                encrypted,
                "case {case:?}: expected encrypted {encrypted}, got {actual:?}"
            );
            if let Some(ciphertext) = actual {
                // A base64 value far longer than the plaintext: the ciphertext is as long as
                // the key's modulus whatever the field held.
                assert!(
                    ciphertext.len() > 100,
                    "case {case:?}: expected a base64 ciphertext of the modulus's width, got \
                     {ciphertext:?}"
                );
            }
        }
    }

    #[test]
    fn a_content_type_is_read_down_to_its_media_type() {
        let rows = [
            ("application/x-www-form-urlencoded", "application/x-www-form-urlencoded"),
            (
                "application/x-www-form-urlencoded; charset=UTF-8",
                "application/x-www-form-urlencoded",
            ),
            (" text/plain ", "text/plain"),
            ("", ""),
        ];
        for (stated, expected) in rows {
            assert_eq!(
                media_type(stated),
                expected,
                "case {stated:?}: expected {expected:?}, got {:?}",
                media_type(stated)
            );
        }
    }
}
