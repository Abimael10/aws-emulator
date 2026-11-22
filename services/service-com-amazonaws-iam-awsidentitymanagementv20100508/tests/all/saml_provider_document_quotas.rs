// SPDX-License-Identifier: CC0-1.0

//! What a SAML provider's metadata document may describe, and the key a provider that
//! DEMANDS encrypted assertions must hold — both end to end over the awsQuery wire.
//!
//! The rules under test:
//!
//! * a metadata document is read for the identity providers it describes and the signing
//!   keys it publishes, and each of those is a QUOTA: `CreateSAMLProvider` and
//!   `UpdateSAMLProvider` model `LimitExceededException` ("it attempted to create resources
//!   beyond the current … account limits. The error message describes the limit exceeded",
//!   HTTP 409 / `LimitExceeded`), so a document past either quota is refused with the
//!   sentence naming the quota that fired and the account is left exactly as it was — never
//!   a provider built from the part of the document that fitted;
//! * a provider under the strict member of `assertionEncryptionModeType` demands encrypted
//!   assertions, and the private key a request uploads is the only thing it would hold that
//!   can decrypt one ("the private key must be a .pem file that uses AES-GCM or AES-CBC
//!   encryption algorithm to decrypt SAML assertions",
//!   `CreateSAMLProvider$AddPrivateKey`), so any call that would LEAVE a provider demanding
//!   them with no key on file is refused `InvalidInput` (HTTP 400) and nothing is written —
//!   whether that call is the create, an update choosing the mode, or an update removing the
//!   last key.
//!
//! Each suite is a TABLE whose rows differ along what the rules generalise over: how many
//! identity providers a document describes and how many keys it publishes, on both sides of
//! each quota; which operation carries the document; and, for the encryption rule, which
//! combination of mode, upload and removal a request names.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns, and the single-entity
//! document fixtures are the ones `tests/all/saml_providers.rs` owns, the many-keys document
//! is built from the key fixtures `tests/all/saml_provider_metadata.rs` owns (which is also
//! where the signing-key quota is asserted on the CREATE, so this suite drives it only on
//! the update), and the only document built here is the one no other suite needs: one
//! describing several identity providers.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    MAX_IDENTITY_PROVIDERS_PER_SAML_METADATA, MAX_SIGNING_KEYS_PER_SAML_METADATA,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::{borrowed, param};
use crate::saml_provider_metadata::{
    DocumentForm, PublishedKey, metadata_document as keyed_metadata_document, signing_key,
};
use crate::saml_providers::{METADATA_NAMESPACE, MetadataForm, expected_arn, metadata_document};
use crate::user_lifecycle::{call, element, elements, error_code, router};

/// The sentence IAM refuses a call that would leave a provider demanding encrypted
/// assertions with no private key with, written out here rather than read from the
/// implementation's constant, because what this suite pins is the text on the WIRE.
const ENCRYPTION_NEEDS_A_KEY: &str =
    "Failed to set assertion encryption mode to Required because no private key is present.";

/// The whole sentence IAM refuses a document describing too many identity providers with:
/// the quota's name AND its published value (ten), written out for the same reason — a
/// wrong quota or a wrong limit must fail this suite.
const IDPS_QUOTA_EXCEEDED: &str = "Cannot exceed quota for SamlIdpsPerSamlProvider: 10";

/// The whole sentence IAM refuses a document publishing too many keys with.
const KEYS_QUOTA_EXCEEDED: &str = "Cannot exceed quota for KeysPerSamlIdp: 10";

/// A metadata document describing `identity_providers` identity providers — one entity
/// descriptor each, nested in the group form the OASIS vocabulary uses for several
/// ("Metadata for SAML V2.0" §2.3). The padding keeps even a one-entity document past
/// `SAMLMetadataDocumentType`'s 1000-character minimum; it sits in an element nothing reads.
fn document_with_identity_providers(identity_providers: usize) -> String {
    let entities: String = (0..identity_providers)
        .map(|index| {
            format!(
                "<EntityDescriptor entityID=\"https://idp-{index}.example.com/saml\">\
                 <IDPSSODescriptor \
                 protocolSupportEnumeration=\"urn:oasis:names:tc:SAML:2.0:protocol\">\
                 <SingleSignOnService \
                 Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect\" \
                 Location=\"https://idp-{index}.example.com/sso\"/></IDPSSODescriptor>\
                 </EntityDescriptor>"
            )
        })
        .collect();
    format!(
        "<EntitiesDescriptor xmlns=\"{METADATA_NAMESPACE}\" Name=\"example-federation\">\
         {entities}<Extensions><Padding>{}</Padding></Extensions></EntitiesDescriptor>",
        "0123456789".repeat(120)
    )
}

/// A metadata document describing ONE identity provider that publishes `keys` distinct
/// signing keys, built from the key fixtures `tests/all/saml_provider_metadata.rs` owns so
/// every key is a real key pair and no second builder exists for them.
fn document_with_keys(keys: usize) -> String {
    let published: Vec<PublishedKey> = (0..keys)
        .map(|index| PublishedKey {
            purpose: Some("signing"),
            key: signing_key(&format!("key-{index}.example.com")),
        })
        .collect();
    keyed_metadata_document(
        DocumentForm::Entity,
        "https://many-keys.example.com/saml",
        None,
        &published,
    )
}

/// A private key body satisfying `privateKeyType` (len 1..16384, pattern
/// `^[\u0009\u000A\u000D\u0020-\u00FF]+$`): "the private key must be a .pem file"
/// (`CreateSAMLProvider$AddPrivateKey`).
fn private_key(seed: &str) -> String {
    format!("-----BEGIN PRIVATE KEY----- {seed} -----END PRIVATE KEY-----")
}

/// One `CreateSAMLProvider` call carrying `document`.
async fn create(
    router: &MultiServiceRouter,
    name: &str,
    document: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![
        param("Name", name),
        param("SAMLMetadataDocument", document),
    ];
    params.extend_from_slice(extra);
    call(router, "CreateSAMLProvider", &borrowed(&params)).await
}

/// One `UpdateSAMLProvider` call.
async fn update(
    router: &MultiServiceRouter,
    arn: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![param("SAMLProviderArn", arn)];
    params.extend_from_slice(members);
    call(router, "UpdateSAMLProvider", &borrowed(&params)).await
}

/// `GetSAMLProvider` for `arn`, with the status it answered.
async fn read(router: &MultiServiceRouter, arn: &str) -> (u16, String) {
    call(router, "GetSAMLProvider", &[("SAMLProviderArn", arn)]).await
}

/// A provider the account holds, created from a one-entity document with the members
/// `extra` names, answering its ARN.
async fn given_provider(
    router: &MultiServiceRouter,
    name: &str,
    extra: &[(String, String)],
) -> String {
    let document = metadata_document(MetadataForm::Entity, "https://idp.example.com/saml", None);
    let (status, body) = create(router, name, &document, extra).await;
    assert_eq!(
        status, 200,
        "the fixture provider {name:?} must be created, got {status}: {body}"
    );
    element(&body, "SAMLProviderArn")
}

/// Assert a refusal: the status, the error code and the whole message.
fn assert_refused(case: &str, status: u16, body: &str, want_status: u16, code: &str, message: &str) {
    assert_eq!(
        status, want_status,
        "case {case:?}: expected HTTP {want_status}, got {status}: {body}"
    );
    assert_eq!(
        error_code(body),
        code,
        "case {case:?}: expected the error code {code:?}, got {:?}: {body}",
        error_code(body)
    );
    assert_eq!(
        element(body, "Message"),
        message,
        "case {case:?}: expected the message {message:?}, got {:?}",
        element(body, "Message")
    );
}

/// RULE: a document describing up to the quota of identity providers is served; one
/// describing more is refused `LimitExceeded` naming that quota, and creates nothing.
#[tokio::test]
async fn a_document_past_the_identity_provider_quota_is_refused_and_creates_nothing() {
    let router = router();
    let quota = MAX_IDENTITY_PROVIDERS_PER_SAML_METADATA;
    let expected = IDPS_QUOTA_EXCEEDED;

    // Rows on both sides of the quota: one entity, a handful, exactly the quota, one past
    // it, and well past it.
    for described in [1, 2, quota - 1, quota, quota + 1, quota + 4] {
        let case = format!("a document describing {described} identity providers");
        let name = format!("ManyIdps{described}");
        let document = document_with_identity_providers(described);
        let (status, body) = create(&router, &name, &document, &[]).await;

        if described > quota {
            assert_refused(&case, status, &body, 409, "LimitExceeded", expected);
            let (status, read_body) = read(&router, &expected_arn(&name)).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a refused document must create nothing, got {status}: \
                 {read_body}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: a document within the quota must be served, got {status}: {body}"
        );
        let arn = element(&body, "SAMLProviderArn");
        let (status, read_body) = read(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created provider must be readable, got {status}: {read_body}"
        );
    }
}

/// RULE: the same quota is taken on a REPLACEMENT document, so an update past it is refused
/// and the held provider keeps the document it had.
#[tokio::test]
async fn a_replacement_document_past_a_document_quota_is_refused_and_changes_nothing() {
    let router = router();
    let arn = given_provider(&router, "ReplacedPastTheQuota", &[]).await;
    let (_, before) = read(&router, &arn).await;
    let held_document = crate::user_lifecycle::document_element(&before, "SAMLMetadataDocument");

    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a replacement describing one identity provider too many",
            document_with_identity_providers(MAX_IDENTITY_PROVIDERS_PER_SAML_METADATA + 1),
            IDPS_QUOTA_EXCEEDED,
        ),
        (
            "a replacement publishing one key too many",
            document_with_keys(MAX_SIGNING_KEYS_PER_SAML_METADATA + 1),
            KEYS_QUOTA_EXCEEDED,
        ),
    ];

    for (case, document, expected) in rows {
        let (status, body) =
            update(&router, &arn, &[param("SAMLMetadataDocument", &document)]).await;
        assert_refused(case, status, &body, 409, "LimitExceeded", expected);

        let (status, after) = read(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the held provider must still be readable, got {status}: {after}"
        );
        assert_eq!(
            crate::user_lifecycle::document_element(&after, "SAMLMetadataDocument"),
            held_document,
            "case {case:?}: a refused update must leave the held document exactly as it was"
        );
    }
}

/// RULE: a create that would leave the provider demanding encrypted assertions with no
/// private key is refused `InvalidInput` with IAM's sentence and creates nothing, while
/// every combination that leaves a key — or does not demand encryption — is served.
#[tokio::test]
async fn a_create_demanding_encrypted_assertions_without_a_key_is_refused() {
    let router = router();

    // Rows: the mode the request names (none, the permissive member, the strict one) across
    // whether it uploads a key.
    struct Row {
        case: &'static str,
        members: Vec<(String, String)>,
        /// Whether the combination leaves a provider that can decrypt what it demands, and
        /// so is served.
        served: bool,
    }

    let rows: Vec<Row> = vec![
        Row {
            case: "no mode and no key",
            members: Vec::new(),
            served: true,
        },
        Row {
            case: "no mode with a key",
            members: vec![param("AddPrivateKey", &private_key("no-mode"))],
            served: true,
        },
        Row {
            case: "the permissive mode and no key",
            members: vec![param("AssertionEncryptionMode", "Allowed")],
            served: true,
        },
        Row {
            case: "the strict mode with a key",
            members: vec![
                param("AssertionEncryptionMode", "Required"),
                param("AddPrivateKey", &private_key("strict")),
            ],
            served: true,
        },
        Row {
            case: "the strict mode and no key",
            members: vec![param("AssertionEncryptionMode", "Required")],
            served: false,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("EncryptionMode{index}");
        let document =
            metadata_document(MetadataForm::Entity, "https://idp.example.com/saml", None);
        let (status, body) = create(&router, &name, &document, &row.members).await;

        if !row.served {
            assert_refused(
                case,
                status,
                &body,
                400,
                "InvalidInput",
                ENCRYPTION_NEEDS_A_KEY,
            );
            let (status, read_body) = read(&router, &expected_arn(&name)).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a refused create must create nothing, got {status}: {read_body}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: the create must be served, got {status}: {body}"
        );
        let (status, read_body) = read(&router, &element(&body, "SAMLProviderArn")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created provider must be readable, got {status}: {read_body}"
        );
    }
}

/// RULE: the same rule is taken on the state an UPDATE would leave, so choosing the strict
/// mode on a provider holding no key, and removing the last key from a provider under that
/// mode, are both refused and change nothing — while a request that supplies the key in the
/// same call, rotates one, or leaves the strict mode behind, is served.
#[tokio::test]
async fn an_update_that_would_leave_encryption_required_without_a_key_is_refused() {
    let router = router();

    // Choosing the strict mode on a provider that holds no key: refused, and the provider
    // keeps the mode it had.
    let keyless = given_provider(
        &router,
        "KeylessProvider",
        &[param("AssertionEncryptionMode", "Allowed")],
    )
    .await;
    let (status, body) = update(
        &router,
        &keyless,
        &[param("AssertionEncryptionMode", "Required")],
    )
    .await;
    assert_refused(
        "the strict mode on a provider holding no key",
        status,
        &body,
        400,
        "InvalidInput",
        ENCRYPTION_NEEDS_A_KEY,
    );
    let (_, after) = read(&router, &keyless).await;
    assert_eq!(
        element(&after, "AssertionEncryptionMode"),
        "Allowed",
        "a refused update must leave the encryption setting as it was, got {after}"
    );
    assert!(
        elements(&after, "KeyId").is_empty(),
        "a refused update must store no key, got {after}"
    );

    // The same mode WITH the key in the same request: served, and both members land.
    let (status, body) = update(
        &router,
        &keyless,
        &[
            param("AssertionEncryptionMode", "Required"),
            param("AddPrivateKey", &private_key("same-request")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the strict mode supplied with a key must be served, got {status}: {body}"
    );
    let (_, after) = read(&router, &keyless).await;
    assert_eq!(
        element(&after, "AssertionEncryptionMode"),
        "Required",
        "the mode the served update chose must be in force, got {after}"
    );
    let held_key = elements(&after, "KeyId");
    assert_eq!(
        held_key.len(),
        1,
        "the served update must leave the uploaded key, got {after}"
    );

    // Removing that last key while the mode still demands encryption: refused, key kept.
    let (status, body) = update(
        &router,
        &keyless,
        &[param("RemovePrivateKey", &held_key[0])],
    )
    .await;
    assert_refused(
        "removing the last key from a provider demanding encryption",
        status,
        &body,
        400,
        "InvalidInput",
        ENCRYPTION_NEEDS_A_KEY,
    );
    let (_, after) = read(&router, &keyless).await;
    assert_eq!(
        elements(&after, "KeyId"),
        held_key,
        "a refused removal must leave the held key, got {after}"
    );

    // Rotating it — removing the held key and uploading another in one request — leaves a
    // key, so it is served.
    let (status, body) = update(
        &router,
        &keyless,
        &[
            param("RemovePrivateKey", &held_key[0]),
            param("AddPrivateKey", &private_key("rotated")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "a rotation leaves a key on file, so it must be served, got {status}: {body}"
    );
    let (_, after) = read(&router, &keyless).await;
    let rotated = elements(&after, "KeyId");
    assert_eq!(
        rotated.len(),
        1,
        "the rotation must leave exactly one key, got {after}"
    );
    assert_ne!(
        rotated, held_key,
        "the rotation must leave the NEW key, got {after}"
    );

    // Leaving the strict mode behind in the same request that removes the last key: served,
    // because the provider no longer demands encrypted assertions.
    let (status, body) = update(
        &router,
        &keyless,
        &[
            param("AssertionEncryptionMode", "Allowed"),
            param("RemovePrivateKey", &rotated[0]),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "dropping the strict mode and the key together must be served, got {status}: {body}"
    );
    let (_, after) = read(&router, &keyless).await;
    assert_eq!(
        element(&after, "AssertionEncryptionMode"),
        "Allowed",
        "the provider must report the mode the served update chose, got {after}"
    );
    assert!(
        elements(&after, "KeyId").is_empty(),
        "the provider must hold no key after the served removal, got {after}"
    );
}
