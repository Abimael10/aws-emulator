// SPDX-License-Identifier: CC0-1.0

//! The metadata a held identity provider reports BESIDE the document or the URL it was
//! registered with, end to end over the awsQuery wire.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour (status, XML members) of providers that were PERSISTED by
//! `CreateSAMLProvider` / `CreateOpenIDConnectProvider`, never a handler called directly.
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites
//! in this crate share, and every key a fixture document publishes is a real key pair and
//! certificate built by `rcgen` — something other than the code under test — so a key hash
//! the service derives is checked against the key the document actually published.
//!
//! The rules under test are:
//!
//! * a SAML provider ALWAYS reports an expiration — the `validUntil` its metadata document
//!   states, and otherwise the default lifetime counted from the instant it was created —
//!   and always reports an encryption setting, the one a call chose or the permissive
//!   default;
//! * `GetSAMLProvider` reports, for each SIGNING key the provider's metadata document
//!   publishes, the identifier of that key, paired with the issuer the document names;
//! * every entry of `ListSAMLProviders` and `ListOpenIDConnectProviders` reports whether
//!   that provider is enabled for trusted identity propagation.
//!
//! Each suite is a TABLE whose rows differ along what those rules generalise over: how the
//! metadata document is spelled and nests its entity, how many keys it publishes (from none
//! upwards) and what each key is published FOR, whether it states an expiration and in
//! which form, the encryption mode, and how many providers of each kind the account holds —
//! so no single request is the specification.

use aws_smithy_types::DateTime;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::MAX_SIGNING_KEYS_PER_SAML_METADATA;
use smithy_server_runtime::MultiServiceRouter;
use x509_cert::der::Decode;

/// The whole sentence IAM refuses a document publishing more than
/// `MAX_SIGNING_KEYS_PER_SAML_METADATA` signing keys with: the quota's name AND its
/// published value (ten), written out rather than built from the implementation's constants,
/// because what this suite pins is the text on the WIRE.
const KEYS_QUOTA_EXCEEDED: &str = "Cannot exceed quota for KeysPerSamlIdp: 10";

use crate::open_id_connect_providers::{issuer_of, list_members};
use crate::role_lifecycle::{borrowed, optional, param};
use crate::saml_provider_listing::split_element;
use crate::saml_providers::{
    DEFAULT_VALIDITY_SECONDS, METADATA_NAMESPACE, REQUIRED_ENCRYPTION_MODE, expected_arn,
    expected_encryption_mode, expected_expiration, reported_instant,
};
use crate::user_lifecycle::{call, element, elements, router};

// ---------------------------------------------------------------------------
// Fixture keys and documents
// ---------------------------------------------------------------------------

/// One key a fixture metadata document publishes: the base64 of its certificate's DER, as
/// the document carries it, beside the identifier of the key inside it.
#[derive(Debug, Clone)]
pub(crate) struct SigningKey {
    /// What the document's `X509Certificate` element carries.
    certificate: String,
    /// The identifier of the key the certificate carries: the hex SHA-1 of its
    /// `subjectPublicKey` bit string (RFC 5280 §4.2.1.2's first method), computed here from
    /// the generated certificate rather than read out of any response.
    key_hash: String,
}

/// Generate a real key pair and a self-signed certificate for it, and report the two forms
/// above.
pub(crate) fn signing_key(common_name: &str) -> SigningKey {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(vec![common_name.to_string()]).expect("parameters");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    let certificate = params.self_signed(&key).expect("a self-signed certificate");
    let der = certificate.der().to_vec();
    let parsed = x509_cert::Certificate::from_der(&der).expect("the certificate parses");
    let mut hasher = <sha1::Sha1 as sha1::Digest>::new();
    sha1::Digest::update(
        &mut hasher,
        parsed
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .raw_bytes(),
    );
    SigningKey {
        certificate: data_encoding::BASE64.encode(&der),
        key_hash: hex::encode(sha1::Digest::finalize(hasher)),
    }
}

/// How a document spells the SAML 2.0 metadata namespace and nests its entity, so no rule
/// below can be keyed to the text of one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentForm {
    /// Root `EntityDescriptor`, the namespace declared as the default.
    Entity,
    /// Root `EntityDescriptor`, the namespace bound to the conventional `md` prefix.
    PrefixedEntity,
    /// Root `EntitiesDescriptor` with the entity descriptor nested inside it.
    EntityGroup,
}

/// One `KeyDescriptor` of a fixture document: what the key it publishes is for ("signing",
/// "encryption", or nothing at all — "the absence of this attribute signifies that the
/// contained key may be used for both encryption and signing", OASIS "Metadata for SAML
/// V2.0" §2.4.1.1) and which key it publishes.
#[derive(Debug, Clone)]
pub(crate) struct PublishedKey {
    pub(crate) purpose: Option<&'static str>,
    pub(crate) key: SigningKey,
}

/// A SAML 2.0 metadata document naming `entity_id` as its issuer, stating `valid_until` as
/// its expiration when one is given, publishing one `KeyDescriptor` per entry of `keys`,
/// spelled in the requested `form`.
///
/// The padding keeps every document past `SAMLMetadataDocumentType`'s 1000-character
/// minimum (`smithy_query(query="operation", operation="CreateSAMLProvider")`) even when it
/// publishes no key at all; it sits in an element nothing reads.
pub(crate) fn metadata_document(
    form: DocumentForm,
    entity_id: &str,
    valid_until: Option<&str>,
    keys: &[PublishedKey],
) -> String {
    let expiry = valid_until
        .map(|until| format!(" validUntil=\"{until}\""))
        .unwrap_or_default();
    let padding = "0123456789".repeat(120);
    let prefix = match form {
        DocumentForm::PrefixedEntity => "md:",
        _ => "",
    };
    let descriptors: String = keys
        .iter()
        .map(|published| {
            let purpose = published
                .purpose
                .map(|purpose| format!(" use=\"{purpose}\""))
                .unwrap_or_default();
            format!(
                "<{prefix}KeyDescriptor{purpose}><KeyInfo \
                 xmlns=\"http://www.w3.org/2000/09/xmldsig#\"><X509Data><X509Certificate>{}\
                 </X509Certificate></X509Data></KeyInfo></{prefix}KeyDescriptor>",
                published.key.certificate
            )
        })
        .collect();
    let body = format!(
        "<{prefix}IDPSSODescriptor \
         protocolSupportEnumeration=\"urn:oasis:names:tc:SAML:2.0:protocol\">{descriptors}\
         <{prefix}SingleSignOnService \
         Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect\" \
         Location=\"https://idp.example.com/sso\"/></{prefix}IDPSSODescriptor>\
         <{prefix}Extensions><Padding>{padding}</Padding></{prefix}Extensions>"
    );
    match form {
        DocumentForm::Entity => format!(
            "<EntityDescriptor xmlns=\"{METADATA_NAMESPACE}\" entityID=\"{entity_id}\"{expiry}>\
             {body}</EntityDescriptor>"
        ),
        DocumentForm::PrefixedEntity => format!(
            "<md:EntityDescriptor xmlns:md=\"{METADATA_NAMESPACE}\" \
             entityID=\"{entity_id}\"{expiry}>{body}</md:EntityDescriptor>"
        ),
        DocumentForm::EntityGroup => format!(
            "<EntitiesDescriptor xmlns=\"{METADATA_NAMESPACE}\" Name=\"example-federation\"\
             {expiry}><EntityDescriptor entityID=\"{entity_id}\">{body}</EntityDescriptor>\
             </EntitiesDescriptor>"
        ),
    }
}

/// One provider the account is made to hold, with the members the row exercises.
struct ProviderCase {
    /// `Name` — pattern `^[\w._-]+$`, len 1..128 (`CreateSAMLProvider$Name`).
    name: &'static str,
    /// How the row's document is spelled.
    form: DocumentForm,
    /// The issuer the document names in `entityID`.
    entity_id: &'static str,
    /// The document's `validUntil`, when it states one.
    valid_until: Option<&'static str>,
    /// The instant that `validUntil` names in the `Z` form, or `None` when the document
    /// states none and the provider must report the default lifetime instead.
    expected_valid_until: Option<&'static str>,
    /// `AssertionEncryptionMode` the create chooses, when it chooses one.
    encryption_mode: Option<&'static str>,
    /// The keys the document publishes, in document order.
    keys: Vec<PublishedKey>,
}

impl ProviderCase {
    fn document(&self) -> String {
        metadata_document(self.form, self.entity_id, self.valid_until, &self.keys)
    }

    /// The key hashes the read must report, in document order: one per SIGNING key the
    /// document publishes — a descriptor saying nothing publishes one too — and one per
    /// KEY, however many descriptors publish it.
    fn expected_key_hashes(&self) -> Vec<String> {
        let mut hashes: Vec<String> = Vec::new();
        for published in &self.keys {
            if published.purpose.is_some_and(|purpose| purpose != "signing") {
                continue;
            }
            if !hashes.contains(&published.key.key_hash) {
                hashes.push(published.key.key_hash.clone());
            }
        }
        hashes
    }

    fn params(&self) -> Vec<(String, String)> {
        let mut params = vec![
            param("Name", self.name),
            param("SAMLMetadataDocument", &self.document()),
        ];
        if let Some(mode) = self.encryption_mode {
            params.push(param("AssertionEncryptionMode", mode));
            if mode == REQUIRED_ENCRYPTION_MODE {
                // A provider that DEMANDS encrypted assertions has to hold a key to
                // decrypt one with — "the private key must be a .pem file that uses
                // AES-GCM or AES-CBC encryption algorithm to decrypt SAML assertions"
                // (`CreateSAMLProvider$AddPrivateKey`) — so a row choosing that mode
                // uploads one, whose body only has to satisfy `privateKeyType`.
                params.push(param(
                    "AddPrivateKey",
                    &format!("-----BEGIN PRIVATE KEY----- {} -----END PRIVATE KEY-----", self.name),
                ));
            }
        }
        params
    }
}

/// The table both SAML suites are written against: each document spelling, key counts from
/// none to several, a key published for signing, for encryption, for both (by saying
/// nothing) and the same key published twice, expirations stated in the `Z` form, with a
/// numeric UTC offset, with no zone at all and not at all, and both encryption modes beside
/// none.
fn provider_cases() -> Vec<ProviderCase> {
    let shared = signing_key("shared.example.com");
    vec![
        ProviderCase {
            name: "OneSigningKey",
            form: DocumentForm::Entity,
            entity_id: "https://idp.example.com/saml",
            valid_until: Some("2032-05-30T09:01:31Z"),
            expected_valid_until: Some("2032-05-30T09:01:31Z"),
            encryption_mode: None,
            keys: vec![PublishedKey {
                purpose: Some("signing"),
                key: signing_key("one.example.com"),
            }],
        },
        ProviderCase {
            name: "NoKey.and-no_expiry",
            form: DocumentForm::PrefixedEntity,
            entity_id: "urn:example:idp:corp",
            valid_until: None,
            expected_valid_until: None,
            encryption_mode: Some("Required"),
            keys: Vec::new(),
        },
        ProviderCase {
            name: "SigningAndEncryption",
            form: DocumentForm::EntityGroup,
            entity_id: "https://federation.example.org/metadata",
            valid_until: Some("2030-01-02T03:04:05.678Z"),
            expected_valid_until: Some("2030-01-02T03:04:05.678Z"),
            encryption_mode: Some("Allowed"),
            keys: vec![
                PublishedKey {
                    purpose: Some("signing"),
                    key: signing_key("signing.example.org"),
                },
                PublishedKey {
                    purpose: Some("encryption"),
                    key: signing_key("encryption.example.org"),
                },
            ],
        },
        ProviderCase {
            name: "UnpurposedAndRepeatedKeys",
            form: DocumentForm::Entity,
            entity_id: "https://idp.example.net/sso/metadata",
            valid_until: Some("2035-12-31T23:59:59+02:00"),
            expected_valid_until: Some("2035-12-31T21:59:59Z"),
            encryption_mode: None,
            keys: vec![
                PublishedKey {
                    purpose: None,
                    key: shared.clone(),
                },
                PublishedKey {
                    purpose: Some("signing"),
                    key: shared,
                },
                PublishedKey {
                    purpose: Some("signing"),
                    key: signing_key("second.example.net"),
                },
            ],
        },
        ProviderCase {
            name: "EncryptionKeyOnly",
            form: DocumentForm::PrefixedEntity,
            entity_id: "https://idp.example.com/zoneless",
            valid_until: Some("2031-07-04T12:00:00"),
            expected_valid_until: Some("2031-07-04T12:00:00Z"),
            encryption_mode: Some("Required"),
            keys: vec![PublishedKey {
                purpose: Some("encryption"),
                key: signing_key("sealed.example.com"),
            }],
        },
    ]
}

/// Create one fixture provider, answering the ARN the create reported.
async fn create_provider(router: &MultiServiceRouter, case: &ProviderCase) -> String {
    let params = case.params();
    let (status, body) = call(router, "CreateSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: the fixture provider must be created, got {status}: {body}",
        case.name
    );
    element(&body, "SAMLProviderArn")
}

/// Read one provider back through `GetSAMLProvider`.
async fn read_provider(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let params = vec![param("SAMLProviderArn", arn)];
    let (status, body) = call(router, "GetSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the held provider must read back, got {status}: {body}"
    );
    body
}

/// The issuer and the key hashes the `SAMLKeyHash` member of a response reports.
fn reported_key_hashes(body: &str) -> (Option<String>, Vec<String>) {
    let Some((list, _)) = split_element(body, "SAMLKeyHash") else {
        panic!("the response must carry a SAMLKeyHash element, got {body}");
    };
    let issuers = elements(list, "IssuerURL");
    assert!(
        issuers.len() <= 1,
        "a document names ONE issuer, so at most one entry is expected, got {body}"
    );
    (issuers.into_iter().next(), list_members(list, "KeyHashes"))
}

/// The entry of a `SAMLProviderList` whose `Arn` is `arn`.
fn entry_of(listing: &str, arn: &str, case: &str) -> String {
    let Some((list, _)) = split_element(listing, "SAMLProviderList") else {
        panic!("case {case:?}: the listing must carry a SAMLProviderList, got {listing}");
    };
    let mut rest = list;
    while let Some((entry, remainder)) = split_element(rest, "member") {
        if element(entry, "Arn") == arn {
            return entry.to_string();
        }
        rest = remainder;
    }
    panic!("case {case:?}: the listing must report {arn:?}, got {listing}")
}

// ---------------------------------------------------------------------------
// An expiration and an encryption setting, always
// ---------------------------------------------------------------------------

/// Every row's provider reports an expiration and an encryption setting: the document's
/// `validUntil` when it states one and the default lifetime from the creation instant when
/// it does not, and the mode the create chose or the permissive default when it chose none.
/// Both are reported by `GetSAMLProvider`, and the expiration by the listing entry too, so
/// a client enumerating providers sees the same instant a per-provider read does.
#[tokio::test]
async fn a_saml_provider_always_reports_an_expiration_and_an_encryption_setting() {
    let router = router();

    for case in provider_cases() {
        let arn = create_provider(&router, &case).await;
        let read = read_provider(&router, case.name, &arn).await;

        let create_date = element(&read, "CreateDate");
        let reported = element(&read, "ValidUntil");
        let expiration = expected_expiration(&create_date, case.expected_valid_until);
        assert_eq!(
            reported_instant(&reported),
            expiration,
            "case {:?}: expected ValidUntil {expiration:?}, got {reported:?}",
            case.name
        );
        // A document stating no expiration is the row that pins the default lifetime
        // itself: the gap between the two instants is exactly it.
        if case.expected_valid_until.is_none() {
            assert_eq!(
                reported_instant(&reported).secs() - reported_instant(&create_date).secs(),
                DEFAULT_VALIDITY_SECONDS,
                "case {:?}: a document stating no expiration must leave the provider the \
                 default lifetime after its CreateDate {create_date:?}, got {reported:?}",
                case.name
            );
        }
        assert_eq!(
            element(&read, "AssertionEncryptionMode"),
            expected_encryption_mode(case.encryption_mode),
            "case {:?}: expected AssertionEncryptionMode {:?}, got {read}",
            case.name,
            expected_encryption_mode(case.encryption_mode)
        );

        // The listing entry reports the SAME expiration, so neither read can describe one
        // provider differently.
        let (status, listing) = call(&router, "ListSAMLProviders", &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from ListSAMLProviders, got {status}: {listing}",
            case.name
        );
        let entry = entry_of(&listing, &arn, case.name);
        assert_eq!(
            reported_instant(&element(&entry, "ValidUntil")),
            expiration,
            "case {:?}: the entry's ValidUntil must be the one the read reports, got {entry}",
            case.name
        );
    }
}

// ---------------------------------------------------------------------------
// The keys the document publishes
// ---------------------------------------------------------------------------

/// `GetSAMLProvider` reports, for every SIGNING key the provider's metadata document
/// publishes, the identifier of that key — in document order, once per key — paired with
/// the issuer the document names. A key published for encryption only is not one of them,
/// and a document publishing no signing key at all pairs nothing while the member is still
/// reported rather than omitted.
#[tokio::test]
async fn get_saml_provider_reports_the_identifier_of_every_signing_key_the_document_publishes() {
    let router = router();

    for case in provider_cases() {
        let arn = create_provider(&router, &case).await;
        let read = read_provider(&router, case.name, &arn).await;
        let (issuer, hashes) = reported_key_hashes(&read);

        let expected = case.expected_key_hashes();
        assert_eq!(
            hashes, expected,
            "case {:?}: expected the identifier of each published signing key in document \
             order, got {read}",
            case.name
        );
        match expected.is_empty() {
            true => assert_eq!(
                issuer, None,
                "case {:?}: a document publishing no signing key pairs no issuer, got \
                 {issuer:?}",
                case.name
            ),
            false => assert_eq!(
                issuer.as_deref(),
                Some(case.entity_id),
                "case {:?}: the hashes must be paired with the document's issuer, got {read}",
                case.name
            ),
        }
        // Reading twice reports the same hashes: the pairing is derived from the stored
        // document, not from the call that reads it.
        let again = read_provider(&router, case.name, &arn).await;
        assert_eq!(
            reported_key_hashes(&again),
            (issuer, hashes),
            "case {:?}: two reads must report the same key hashes",
            case.name
        );
    }
}

/// A metadata document publishes at most `MAX_SIGNING_KEYS_PER_SAML_METADATA` distinct
/// signing keys: a document publishing up to that many has every one of them reported, and
/// one publishing more is refused `LimitExceeded` (HTTP 409) with nothing created — so one
/// request can neither choose how much work reading its document is nor leave a provider
/// holding only the part of a document that fitted.
#[tokio::test]
async fn a_document_past_the_signing_key_quota_is_refused_and_creates_nothing() {
    let router = router();

    for published in [
        MAX_SIGNING_KEYS_PER_SAML_METADATA - 1,
        MAX_SIGNING_KEYS_PER_SAML_METADATA,
        MAX_SIGNING_KEYS_PER_SAML_METADATA + 1,
        MAX_SIGNING_KEYS_PER_SAML_METADATA + 5,
    ] {
        let keys: Vec<PublishedKey> = (0..published)
            .map(|index| PublishedKey {
                purpose: Some("signing"),
                key: signing_key(&format!("key-{index}.example.com")),
            })
            .collect();
        let case = ProviderCase {
            name: "ManyKeys",
            form: DocumentForm::Entity,
            entity_id: "https://idp.example.com/many",
            valid_until: Some("2033-03-03T03:03:03Z"),
            expected_valid_until: Some("2033-03-03T03:03:03Z"),
            encryption_mode: None,
            keys: keys.clone(),
        };
        let params = case.params();
        let (status, body) = call(&router, "CreateSAMLProvider", &borrowed(&params)).await;
        if published > MAX_SIGNING_KEYS_PER_SAML_METADATA {
            assert_eq!(
                status, 409,
                "a document publishing {published} signing keys is past the quota of \
                 {MAX_SIGNING_KEYS_PER_SAML_METADATA} and must be refused, got {status}: {body}"
            );
            assert_eq!(
                crate::user_lifecycle::error_code(&body),
                "LimitExceeded",
                "a document past the signing-key quota must be refused LimitExceeded, got {body}"
            );
            assert_eq!(
                element(&body, "Message"),
                KEYS_QUOTA_EXCEEDED,
                "expected the quota sentence {KEYS_QUOTA_EXCEEDED:?}, got {:?}",
                element(&body, "Message")
            );
            // Nothing was created: the account holds no provider under that name.
            let read = vec![param("SAMLProviderArn", &expected_arn(case.name))];
            let (status, read_body) = call(&router, "GetSAMLProvider", &borrowed(&read)).await;
            assert_eq!(
                status, 404,
                "a document past the quota must create nothing, got {status}: {read_body}"
            );
            continue;
        }

        // Each row holds the account's only provider, so the name can be the same one.
        assert_eq!(
            status, 200,
            "a document publishing {published} signing keys is within the quota and must be \
             served, got {status}: {body}"
        );
        let arn = element(&body, "SAMLProviderArn");
        let read = read_provider(&router, case.name, &arn).await;
        let (_, hashes) = reported_key_hashes(&read);
        let expected: Vec<String> = keys
            .iter()
            .map(|published| published.key.key_hash.clone())
            .collect();
        assert_eq!(
            hashes, expected,
            "a document publishing {published} signing keys must report every one of them, \
             got {read}"
        );
        let delete = vec![param("SAMLProviderArn", &arn)];
        let (status, body) = call(&router, "DeleteSAMLProvider", &borrowed(&delete)).await;
        assert_eq!(
            status, 200,
            "the row's provider must be removable, got {status}: {body}"
        );
    }
}

/// Replacing the metadata document replaces the keys the provider publishes: after an
/// `UpdateSAMLProvider` carrying a document with different keys and a different issuer,
/// `GetSAMLProvider` reports the NEW pairing and nothing of the old one.
#[tokio::test]
async fn replacing_the_document_replaces_the_key_hashes_it_published() {
    let router = router();
    let first = signing_key("before.example.com");
    let case = ProviderCase {
        name: "KeysRotated",
        form: DocumentForm::Entity,
        entity_id: "https://idp.example.com/before",
        valid_until: Some("2030-01-01T00:00:00Z"),
        expected_valid_until: Some("2030-01-01T00:00:00Z"),
        encryption_mode: None,
        keys: vec![PublishedKey {
            purpose: Some("signing"),
            key: first.clone(),
        }],
    };
    let arn = create_provider(&router, &case).await;
    let before = read_provider(&router, case.name, &arn).await;
    assert_eq!(
        reported_key_hashes(&before),
        (
            Some(case.entity_id.to_string()),
            vec![first.key_hash.clone()]
        ),
        "the create's keys must be reported first, got {before}"
    );

    let rotated = [
        PublishedKey {
            purpose: Some("signing"),
            key: signing_key("after-one.example.com"),
        },
        PublishedKey {
            purpose: None,
            key: signing_key("after-two.example.com"),
        },
    ];
    let replacement = metadata_document(
        DocumentForm::EntityGroup,
        "https://idp.example.com/after",
        None,
        &rotated,
    );
    let params = vec![
        param("SAMLProviderArn", &arn),
        param("SAMLMetadataDocument", &replacement),
    ];
    let (status, body) = call(&router, "UpdateSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the update must succeed, got {status} with body {body}"
    );

    let after = read_provider(&router, case.name, &arn).await;
    assert_eq!(
        reported_key_hashes(&after),
        (
            Some("https://idp.example.com/after".to_string()),
            rotated
                .iter()
                .map(|published| published.key.key_hash.clone())
                .collect::<Vec<String>>()
        ),
        "the replacement's keys must be what the read reports, got {after}"
    );
    // The replacement states no expiration, so the provider reports the default lifetime
    // from the instant it was CREATED — not from the instant it was updated.
    let create_date = element(&after, "CreateDate");
    assert_eq!(
        reported_instant(&element(&after, "ValidUntil")),
        DateTime::from_secs(reported_instant(&create_date).secs() + DEFAULT_VALIDITY_SECONDS),
        "the updated provider's ValidUntil must be its own default lifetime, got {after}"
    );
}

// ---------------------------------------------------------------------------
// The flag every list entry carries
// ---------------------------------------------------------------------------

/// Every entry of `ListSAMLProviders` and of `ListOpenIDConnectProviders` reports whether
/// that provider is enabled for trusted identity propagation, so a client enumerating
/// providers can tell without a per-provider read. No operation this service models turns
/// it on, so every held provider reports it disabled — on every entry, whatever else the
/// provider carries and however many the account holds.
#[tokio::test]
async fn every_provider_list_entry_reports_the_trusted_identity_propagation_flag() {
    let router = router();

    // An account holding NO provider: the flag cannot be reported on an entry that does
    // not exist, and the listing is still the empty list.
    for action in ["ListSAMLProviders", "ListOpenIDConnectProviders"] {
        let (status, body) = call(&router, action, &[]).await;
        assert_eq!(
            status, 200,
            "{action}: expected HTTP 200 for an empty account, got {status}: {body}"
        );
        assert!(
            elements(&body, "IsTrustedIdentityPropagationEnabled").is_empty(),
            "{action}: an account holding no provider reports no entry, got {body}"
        );
    }

    let cases = provider_cases();
    for case in &cases {
        create_provider(&router, case).await;
    }
    // OIDC provider URLs spanning a bare host, a one-segment path and a nested path.
    let urls = [
        "https://server.example.org",
        "https://example.com/oidc",
        "https://oidc.eks.us-east-1.amazonaws.com/id/EXAMPLED539D4633E53DE1B716D3041E",
    ];
    for url in urls {
        let params = vec![param("Url", url)];
        let (status, body) = call(&router, "CreateOpenIDConnectProvider", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "{url}: the fixture OIDC provider must be created, got {status}: {body}"
        );
    }

    let (status, saml) = call(&router, "ListSAMLProviders", &[]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListSAMLProviders, got {status}: {saml}"
    );
    assert_eq!(
        elements(&saml, "IsTrustedIdentityPropagationEnabled"),
        vec!["false".to_string(); cases.len()],
        "every SAML entry must report the flag, got {saml}"
    );
    for case in &cases {
        let entry = entry_of(&saml, &expected_arn(case.name), case.name);
        assert_eq!(
            optional(&entry, "IsTrustedIdentityPropagationEnabled").as_deref(),
            Some("false"),
            "case {:?}: the entry must report the flag, got {entry}",
            case.name
        );
    }

    let (status, oidc) = call(&router, "ListOpenIDConnectProviders", &[]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListOpenIDConnectProviders, got {status}: {oidc}"
    );
    assert_eq!(
        elements(&oidc, "IsTrustedIdentityPropagationEnabled"),
        vec!["false".to_string(); urls.len()],
        "every OIDC entry must report the flag, got {oidc}"
    );
    let arns = elements(&oidc, "Arn");
    for url in urls {
        let arn = crate::open_id_connect_providers::expected_arn(url);
        assert!(
            arns.contains(&arn),
            "the listing must report {:?} for {url}, got {oidc}",
            issuer_of(url)
        );
    }
}
