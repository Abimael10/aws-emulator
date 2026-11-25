// SPDX-License-Identifier: CC0-1.0

//! What a SAML provider's key hashes ARE, and which family the identifiers IAM mints for a
//! SAML provider are drawn from, end to end over the awsQuery wire.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour (status, XML members) of providers that were PERSISTED by
//! `CreateSAMLProvider`, never a handler called directly. The awsQuery harness is the one
//! `tests/all/user_lifecycle.rs` owns, and every key a fixture document publishes is a real
//! key pair and certificate built by `rcgen` — something other than the code under test — so
//! the hash the service reports is checked against the key the document actually published.
//!
//! The rules under test are:
//!
//! * for every signing key a provider's metadata document publishes, `GetSAMLProvider`
//!   reports that key's hash as the lowercase hex of a 384-bit digest over the published key
//!   — 96 characters — derived from the document's own content, so one document always
//!   reports the same hashes and two different keys never share one;
//! * an identifier IAM mints for an id type that publishes nothing of its values but a
//!   length and an alphabet (`privateKeyIdType`, which `GetSAMLProvider$SAMLProviderUUID`
//!   and `SAMLPrivateKey$KeyId` target) is OPAQUE: it satisfies that length and alphabet,
//!   it is its own, it is stable across reads, and it spells out no fixed prefix that would
//!   say which operation minted it.
//!
//! Each suite is a TABLE whose rows differ along what those rules generalise over: the kind
//! of key the document publishes (three signature algorithms), how many keys it publishes
//! and what each one is published FOR, how the document is spelled, and whether the create
//! uploaded a private key.

use smithy_server_runtime::MultiServiceRouter;

use crate::open_id_connect_providers::list_members;
use crate::role_lifecycle::{borrowed, param};
use crate::saml_provider_listing::split_element;
use crate::saml_provider_metadata::{
    DocumentForm, PublishedKey, metadata_document, signing_key_using,
};
use crate::saml_providers::assert_private_key_id_shape;
use crate::user_lifecycle::{call, element, elements, router};

/// How many characters a key hash carries: a 384-bit digest is 48 bytes, and hex spells two
/// characters per byte. Recorded behaviour for every SAML provider.
const KEY_HASH_CHARACTERS: usize = 96;

/// How many leading characters of two minted identifiers this suite requires to differ.
///
/// An id family that spelled out which operation minted it would do so in a fixed prefix,
/// which every id of that family would then share; `privateKeyIdType` publishes no prefix,
/// so no two minted ids may agree even on this many leading characters.
const MINTED_ID_PREFIX_CHARACTERS: usize = 5;

/// One provider whose document publishes keys, and the keys it publishes.
struct KeyHashCase {
    /// `Name` — pattern `^[\w._-]+$` (`CreateSAMLProvider$Name`).
    name: &'static str,
    /// How the row's document is spelled.
    form: DocumentForm,
    /// The issuer the document names.
    entity_id: &'static str,
    /// The keys the document publishes, in document order.
    keys: Vec<PublishedKey>,
}

impl KeyHashCase {
    fn document(&self) -> String {
        metadata_document(self.form, self.entity_id, None, &self.keys)
    }

    /// The hashes the read must report: one per SIGNING key the document publishes — a
    /// descriptor naming no purpose publishes a signing key too (OASIS "Metadata for SAML
    /// V2.0" §2.4.1.1) — in document order, each computed in this file from the generated
    /// key rather than read out of a response.
    fn expected_key_hashes(&self) -> Vec<String> {
        self.keys
            .iter()
            .filter(|published| {
                published
                    .purpose
                    .is_none_or(|purpose| purpose == "signing")
            })
            .map(|published| published.key.key_hash.clone())
            .collect()
    }
}

/// The table: one key of each of three signature algorithms, a document publishing all
/// three at once, and a document publishing a signing key beside a key published for
/// encryption only — so neither the digest nor the selection can be keyed to one kind of key
/// or to one document spelling.
fn key_hash_cases() -> Vec<KeyHashCase> {
    // The algorithms `rcgen` generates a key pair for, each a different public key
    // structure: a P-256 point, a P-384 point, and an Ed25519 point.
    let p256 = signing_key_using(&rcgen::PKCS_ECDSA_P256_SHA256, "p256.example.com");
    let p384 = signing_key_using(&rcgen::PKCS_ECDSA_P384_SHA384, "p384.example.com");
    let ed25519 = signing_key_using(&rcgen::PKCS_ED25519, "ed25519.example.com");
    vec![
        KeyHashCase {
            name: "EcdsaP256Key",
            form: DocumentForm::Entity,
            entity_id: "https://p256.example.com/saml",
            keys: vec![PublishedKey {
                purpose: Some("signing"),
                key: p256.clone(),
            }],
        },
        KeyHashCase {
            name: "EcdsaP384Key",
            form: DocumentForm::PrefixedEntity,
            entity_id: "urn:example:idp:p384",
            keys: vec![PublishedKey {
                purpose: Some("signing"),
                key: p384.clone(),
            }],
        },
        KeyHashCase {
            // A descriptor naming no purpose publishes a key usable for signing.
            name: "Ed25519Key_no.purpose",
            form: DocumentForm::EntityGroup,
            entity_id: "https://ed25519.example.org/metadata",
            keys: vec![PublishedKey {
                purpose: None,
                key: ed25519.clone(),
            }],
        },
        KeyHashCase {
            name: "ThreeKindsOfKey",
            form: DocumentForm::Entity,
            entity_id: "https://mixed.example.net/idp",
            keys: vec![
                PublishedKey {
                    purpose: Some("signing"),
                    key: p384.clone(),
                },
                PublishedKey {
                    purpose: None,
                    key: ed25519.clone(),
                },
                PublishedKey {
                    purpose: Some("signing"),
                    key: p256.clone(),
                },
            ],
        },
        KeyHashCase {
            name: "SigningBesideEncryption",
            form: DocumentForm::PrefixedEntity,
            entity_id: "https://encrypting.example.com/saml",
            keys: vec![
                PublishedKey {
                    purpose: Some("encryption"),
                    key: p256.clone(),
                },
                PublishedKey {
                    purpose: Some("signing"),
                    key: ed25519.clone(),
                },
            ],
        },
    ]
}

/// Create one fixture provider, answering the ARN the create reported.
async fn create_provider(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let params = vec![param("Name", name), param("SAMLMetadataDocument", document)];
    let (status, body) = call(router, "CreateSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture provider must be created, got {status}: {body}"
    );
    element(&body, "SAMLProviderArn")
}

/// Read one provider back through `GetSAMLProvider`.
async fn read_provider(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let params = vec![param("SAMLProviderArn", arn)];
    let (status, body) = call(router, "GetSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetSAMLProvider, got {status}: {body}"
    );
    body
}

/// The key hashes the `SAMLKeyHash` member of a response reports.
fn reported_key_hashes(case: &str, body: &str) -> Vec<String> {
    let Some((entries, _)) = split_element(body, "SAMLKeyHash") else {
        panic!("case {case:?}: the response must carry a SAMLKeyHash element, got {body}");
    };
    list_members(entries, "KeyHashes")
}

// ---------------------------------------------------------------------------
// The digest a key hash is
// ---------------------------------------------------------------------------

/// RULE: every key hash `GetSAMLProvider` reports is the lowercase hex of a 384-bit digest
/// over the key the document published — 96 characters — and is DERIVED from that key, so it
/// is the hash of the key material and not of the provider, the call or the moment.
#[tokio::test]
async fn every_published_signing_key_is_named_by_a_96_character_lowercase_hex_digest() {
    let router = router();

    for case in key_hash_cases() {
        let arn = create_provider(&router, case.name, &case.document()).await;
        let read = read_provider(&router, case.name, &arn).await;
        let reported = reported_key_hashes(case.name, &read);

        let expected = case.expected_key_hashes();
        assert_eq!(
            reported, expected,
            "case {:?}: expected the hash of each published signing key in document order, \
             {expected:?}, got {reported:?}",
            case.name
        );
        for hash in &reported {
            assert_eq!(
                hash.len(),
                KEY_HASH_CHARACTERS,
                "case {:?}: a key hash must be {KEY_HASH_CHARACTERS} characters, got {} in \
                 {hash:?}",
                case.name,
                hash.len()
            );
            assert!(
                hash.chars()
                    .all(|spelled| spelled.is_ascii_digit() || ('a'..='f').contains(&spelled)),
                "case {:?}: a key hash must be lowercase hex, got {hash:?}",
                case.name
            );
        }
        // Distinct keys are named distinctly: a digest that collapsed two keys to one hash
        // could not tell a caller WHICH key the provider trusts.
        let mut distinct = reported.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            reported.len(),
            "case {:?}: two different keys must not share a hash, got {reported:?}",
            case.name
        );
    }
}

/// RULE, continued: the hash is content-derived, so the SAME document reports the same
/// hashes however many providers hold it and however often it is read — nothing of the
/// provider or of the call enters the digest.
#[tokio::test]
async fn one_metadata_document_always_reports_the_same_key_hashes() {
    let router = router();

    for case in key_hash_cases() {
        let document = case.document();
        let first = create_provider(&router, case.name, &document).await;
        let second_name = format!("{}_Copy", case.name);
        let second = create_provider(&router, &second_name, &document).await;

        let from_first = reported_key_hashes(case.name, &read_provider(&router, case.name, &first).await);
        let from_second =
            reported_key_hashes(&second_name, &read_provider(&router, &second_name, &second).await);
        assert_eq!(
            from_first, from_second,
            "case {:?}: two providers holding one document must report the same key hashes, \
             got {from_first:?} and {from_second:?}",
            case.name
        );

        let again = reported_key_hashes(case.name, &read_provider(&router, case.name, &first).await);
        assert_eq!(
            again, from_first,
            "case {:?}: a second read must report the same key hashes, got {again:?} after \
             {from_first:?}",
            case.name
        );
    }
}

// ---------------------------------------------------------------------------
// The identifiers a SAML provider's read reports
// ---------------------------------------------------------------------------

/// A private key body a create may upload: "the private key must be a .pem file"
/// (`CreateSAMLProvider$AddPrivateKey`), whose pattern admits this ASCII body.
fn private_key_body() -> String {
    format!(
        "-----BEGIN PRIVATE KEY----- {} -----END PRIVATE KEY-----",
        "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVph".repeat(8)
    )
}

/// RULE: every identifier IAM mints for `privateKeyIdType` is opaque — inside that type's
/// length 22..64 and `^[A-Z0-9]+$`, its own, stable across reads, and carrying no fixed
/// prefix that would spell out the operation that minted it.
#[tokio::test]
async fn a_minted_saml_identifier_is_opaque_and_within_its_modelled_type() {
    let router = router();
    // Rows: providers holding documents that publish different kinds of key, with and
    // without a private key uploaded — so the ids come from both families this type covers
    // (a provider's uuid and an uploaded key's id) and from more than one provider of each.
    let mut minted: Vec<(String, String)> = Vec::new();

    for (index, case) in key_hash_cases().into_iter().enumerate() {
        let uploads_key = index.is_multiple_of(2);
        let mut params = vec![
            param("Name", case.name),
            param("SAMLMetadataDocument", &case.document()),
        ];
        if uploads_key {
            params.push(param("AddPrivateKey", &private_key_body()));
        }
        let (status, created) = call(&router, "CreateSAMLProvider", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: the fixture provider must be created, got {status}: {created}",
            case.name
        );
        let arn = element(&created, "SAMLProviderArn");

        let read = read_provider(&router, case.name, &arn).await;
        let uuid = element(&read, "SAMLProviderUUID");
        assert_private_key_id_shape(case.name, "SAMLProviderUUID", &uuid);
        minted.push((format!("{}/SAMLProviderUUID", case.name), uuid.clone()));

        let key_ids = elements(&read, "KeyId");
        assert_eq!(
            key_ids.len(),
            usize::from(uploads_key),
            "case {:?}: expected {} private key entries, got {key_ids:?}",
            case.name,
            usize::from(uploads_key)
        );
        for key_id in &key_ids {
            assert_private_key_id_shape(case.name, "KeyId", key_id);
            minted.push((format!("{}/KeyId", case.name), key_id.clone()));
        }

        // The id belongs to the provider, not to the call that reports it.
        let again = read_provider(&router, case.name, &arn).await;
        assert_eq!(
            element(&again, "SAMLProviderUUID"),
            uuid,
            "case {:?}: two reads must report the same SAMLProviderUUID, got {uuid:?} first",
            case.name
        );
        assert_eq!(
            elements(&again, "KeyId"),
            key_ids,
            "case {:?}: two reads must report the same KeyId, got {key_ids:?} first",
            case.name
        );
    }

    for (position, (member, value)) in minted.iter().enumerate() {
        for (other_member, other) in minted.iter().skip(position + 1) {
            assert_ne!(
                value, other,
                "case {member:?} against {other_member:?}: every minted id must be its own"
            );
            assert_ne!(
                value[..MINTED_ID_PREFIX_CHARACTERS],
                other[..MINTED_ID_PREFIX_CHARACTERS],
                "case {member:?} against {other_member:?}: a minted id must carry no fixed \
                 prefix, got {value:?} and {other:?}"
            );
        }
    }
}
