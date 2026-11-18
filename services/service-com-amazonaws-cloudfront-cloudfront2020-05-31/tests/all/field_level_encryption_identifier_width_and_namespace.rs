// SPDX-License-Identifier: CC0-1.0

//! The WIDTH and the NAMESPACE LETTER of a field-level encryption path identifier do NOT
//! decide which of the one not-found code's two sentences an unresolvable identifier is
//! answered with — its ALPHABET does, end to end over HTTP.
//!
//! The rule under test: an operation addressed by a field-level encryption identifier reads
//! only the identifier ALPHABET of the value it was handed — a run of the digits and uppercase
//! letters CloudFront writes an identifier in. Its `Id` member is a plain `string` in the model
//! (no `@pattern`, no `@length`), so neither the namespace letter nor the body width this
//! service happens to issue is a constraint a request may be refused on: a value of that
//! alphabet the store holds no record for is the record reported ABSENT ("The specified field
//! level encryption config/profile does not exist.") whatever letter it opens with and however
//! wide it is, and only a value outside the alphabet is the identifier the service reports it
//! cannot READ ("Id format is invalid"). Both sentences travel under the kind's one modelled
//! not-found code and its 404
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryption,GetFieldLevelEncryptionConfig,GetFieldLevelEncryptionProfile,GetFieldLevelEncryptionProfileConfig")`
//! reports the kind's not-found error and `AccessDenied` and nothing else), so the MESSAGE is
//! all that tells the two conditions apart.
//!
//! The rows therefore vary what the rule generalises over:
//!
//! * the BODY WIDTH, from one character up to three times the width this service issues,
//!   including one short of, one past it and the width itself;
//! * the NAMESPACE LETTER: the addressed kind's own, one no kind of this service issues an
//!   identifier under, the one every other CloudFront entity id is drawn from, and a leading
//!   digit;
//! * the identifiers of the NEIGHBOURING kinds this service really minted — the sibling
//!   field-level encryption record's and a public key's — which are of the alphabet too, so
//!   which kind an identifier was issued for is read from the STORE rather than from the value;
//! * both RESOURCE KINDS of this surface, each through every operation that addresses it by a
//!   path identifier — both readers, the update and the delete;
//! * and the values that are outside the alphabet altogether: a minted identifier in lower
//!   case, the kind's own letter over a lower-cased body, a UUID, a caller reference, a
//!   hyphenated word and an identifier body carrying a separator.
//!
//! The issued width is read off an identifier this emulator really issued, so no width here is
//! written out by hand. The harness, the document builders and the create paths are the
//! crate's own, reused rather than copied — and the harness this suite builds ([`Kind`],
//! [`seed`], [`address`], [`assert_refusal`]) is itself reused by
//! `field_level_encryption_identifier_issued_form.rs`, which reads the same sentence back
//! through the resources' LIFECYCLE rather than through generated identifiers.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::field_level_encryption_config_lifecycle::{
    create_config, get_config, get_config_document, simple,
};
use crate::field_level_encryption_profile_lifecycle::{
    create_profile, get_profile, get_profile_config, upload_key, Entity, Profile,
};

/// The sentences CloudFront sends on the wire for these refusals, spelled out here rather than
/// taken from the crate, so a reworded refusal fails this suite.
const NO_SUCH_CONFIG: &str = "The specified field level encryption config does not exist.";
const NO_SUCH_PROFILE: &str = "The specified field level encryption profile does not exist.";

/// The sentence every one of these operations answers a value outside the identifier alphabet,
/// naming the member that carried it.
pub(crate) const UNREADABLE_IDENTIFIER: &str = "Id format is invalid";

/// The caller references the two seeded resources are held under.
const CONFIG_REFERENCE: &str = "fle-id-width-config";
const PROFILE_REFERENCE: &str = "fle-id-width-profile";

/// A namespace letter no kind of this service mints an identifier under, so a value carrying it
/// is in the identifier form without being any other kind's identifier.
const UNMINTED_NAMESPACE: char = 'W';

/// Which field-level encryption resource an operation's path identifier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Config,
    Profile,
}

impl Kind {
    /// The modelled error code every operation addressed by an identifier of this kind answers
    /// an identifier it cannot resolve.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Config => "NoSuchFieldLevelEncryptionConfig",
            Self::Profile => "NoSuchFieldLevelEncryptionProfile",
        }
    }

    /// The sentence this kind answers an identifier it holds no resource for.
    pub(crate) fn absent(self) -> &'static str {
        match self {
            Self::Config => NO_SUCH_CONFIG,
            Self::Profile => NO_SUCH_PROFILE,
        }
    }

    /// The path segment the kind's resources live under, from each operation's `http` binding.
    pub(crate) fn segment(self) -> &'static str {
        match self {
            Self::Config => "field-level-encryption",
            Self::Profile => "field-level-encryption-profile",
        }
    }
}

/// The two resources this emulator really stored, with the documents and version tokens the
/// updates and the deletes below submit, so every refusal is about the identifier rather than
/// about a payload or a precondition.
pub(crate) struct Seeded {
    pub(crate) config_id: String,
    config_etag: String,
    config_document: String,
    pub(crate) profile_id: String,
    profile_etag: String,
    profile_document: String,
}

impl Seeded {
    /// The identifier this emulator minted for `kind` — the row every width below is derived
    /// from.
    pub(crate) fn minted(&self, kind: Kind) -> &str {
        match kind {
            Kind::Config => &self.config_id,
            Kind::Profile => &self.profile_id,
        }
    }

    /// The version token the kind's stored resource is currently at.
    pub(crate) fn etag(&self, kind: Kind) -> &str {
        match kind {
            Kind::Config => &self.config_etag,
            Kind::Profile => &self.profile_etag,
        }
    }

    /// A document the kind's update accepts, so an update is never refused for its payload.
    fn document(&self, kind: Kind) -> &str {
        match kind {
            Kind::Config => &self.config_document,
            Kind::Profile => &self.profile_document,
        }
    }
}

/// Store one profile (encrypting with a freshly uploaded public key) and one configuration
/// naming it.
pub(crate) async fn seed(router: &MultiServiceRouter) -> Seeded {
    let key = upload_key(router, 0).await;
    let profile_document = Profile::new(
        "the seeded profile",
        PROFILE_REFERENCE,
        None,
        vec![Entity::new(&key, "provider-one", &["CreditCard"])],
    )
    .xml();
    let created_profile = create_profile(router, &profile_document).await;
    assert_eq!(
        created_profile.status, 201,
        "case seeding the profile: expected HTTP 201, got {} with {}",
        created_profile.status, created_profile.raw
    );
    let profile_id = created_profile
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    let config_document = simple("the seeded configuration", CONFIG_REFERENCE, &profile_id).xml();
    let created_config = create_config(router, &config_document).await;
    assert_eq!(
        created_config.status, 201,
        "case seeding the configuration: expected HTTP 201, got {} with {}",
        created_config.status, created_config.raw
    );
    let config_id = created_config
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    Seeded {
        config_etag: created_config.header("etag").to_string(),
        config_id,
        config_document,
        profile_etag: created_profile.header("etag").to_string(),
        profile_id,
        profile_document,
    }
}

/// Send every operation addressed by an identifier of `kind` at `id`, each answer labelled with
/// the operation's name.
pub(crate) async fn address(
    router: &MultiServiceRouter,
    kind: Kind,
    id: &str,
    seeded: &Seeded,
) -> Vec<(&'static str, Answer)> {
    let segment = kind.segment();
    let etag = seeded.etag(kind);
    let document = seeded.document(kind);
    let (resource_reader, config_reader) = match kind {
        Kind::Config => ("GetFieldLevelEncryption", "GetFieldLevelEncryptionConfig"),
        Kind::Profile => (
            "GetFieldLevelEncryptionProfile",
            "GetFieldLevelEncryptionProfileConfig",
        ),
    };
    let (update, delete) = match kind {
        Kind::Config => (
            "UpdateFieldLevelEncryptionConfig",
            "DeleteFieldLevelEncryptionConfig",
        ),
        Kind::Profile => (
            "UpdateFieldLevelEncryptionProfile",
            "DeleteFieldLevelEncryptionProfile",
        ),
    };

    let resource = match kind {
        Kind::Config => get_config(router, id).await,
        Kind::Profile => get_profile(router, id).await,
    };
    let configuration = match kind {
        Kind::Config => get_config_document(router, id).await,
        Kind::Profile => get_profile_config(router, id).await,
    };
    let updated = call(
        router,
        "PUT",
        &format!("/{API_VERSION}/{segment}/{id}/config"),
        Some(document),
        Some(etag),
    )
    .await;
    let deleted = call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/{segment}/{id}"),
        None,
        Some(etag),
    )
    .await;

    vec![
        (resource_reader, resource),
        (config_reader, configuration),
        (update, updated),
        (delete, deleted),
    ]
}

/// Assert one refusal: its status, its code and — the point of this suite — its message.
pub(crate) fn assert_refusal(answer: &Answer, case: &str, code: &str, message: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected HTTP 404, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected code {code}, got {}",
        answer.raw
    );
    let actual = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        actual, message,
        "case {case:?}: expected message {message:?}, got {actual:?}"
    );
}

/// The widths this suite submits a body at, around the width this service issues (`minted`):
/// one character, two, one short of the issued width, the issued width itself, one past it,
/// twice it and three times it.
fn widths(minted: usize) -> Vec<usize> {
    vec![1, 2, minted - 1, minted, minted + 1, minted * 2, minted * 3]
}

/// A body of `width` characters of the identifier alphabet that no identifier this emulator
/// minted carries: the digits and uppercase letters repeat, so every width is a value of the
/// form without being a value the store ever issued.
fn never_minted_body(width: usize) -> String {
    "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        .chars()
        .cycle()
        .take(width)
        .collect()
}

/// Every identifier of the CloudFront identifier alphabet this suite submits where an
/// identifier of `kind` belongs: the addressed kind's own namespace letter, one no kind of this
/// service issues an identifier under, the letter every other entity id of this service is
/// drawn from and a leading digit, each at every width in [`widths`] — none of them a value the
/// store holds, so each is the record reported absent.
fn identifiers_of_every_width_and_namespace(kind: Kind, seeded: &Seeded) -> Vec<(String, String)> {
    let minted = seeded.minted(kind);
    let namespace = minted.chars().next().unwrap_or(UNMINTED_NAMESPACE);
    let minted_width = minted.len() - 1;

    let mut rows = Vec::new();
    for letter in [namespace, UNMINTED_NAMESPACE, 'E', '0'] {
        for width in widths(minted_width) {
            let id = format!("{letter}{}", never_minted_body(width));
            assert_ne!(
                id, minted,
                "case the generated identifier {id:?}: it must not be the one the store holds"
            );
            rows.push((
                format!("the namespace letter {letter} with a body of {width} characters"),
                id,
            ));
        }
    }
    rows
}

/// The identifiers of the NEIGHBOURING kinds this emulator really minted, submitted where an
/// identifier of `kind` belongs: each is of the identifier alphabet, so each is the addressed
/// record reported absent — which kind issued an identifier is a property of the store, not of
/// the value.
fn neighbouring_minted_identifiers(
    kind: Kind,
    seeded: &Seeded,
    public_key: &str,
) -> Vec<(String, String)> {
    let sibling = match kind {
        Kind::Config => seeded.minted(Kind::Profile),
        Kind::Profile => seeded.minted(Kind::Config),
    };
    vec![
        (
            "the sibling field-level encryption kind's minted identifier".to_string(),
            sibling.to_string(),
        ),
        (
            "a public key identifier this service minted".to_string(),
            public_key.to_string(),
        ),
    ]
}

/// Every value this suite submits where an identifier of `kind` belongs that is outside the
/// identifier alphabet: the minted identifier in lower case, the kind's own letter over a
/// lower-cased body, a UUID, the resource's caller reference, a hyphenated word and an
/// identifier body carrying a separator.
fn unreadable_identifiers(kind: Kind, seeded: &Seeded) -> Vec<(String, String)> {
    let minted = seeded.minted(kind);
    let reference = match kind {
        Kind::Config => CONFIG_REFERENCE,
        Kind::Profile => PROFILE_REFERENCE,
    };
    let (namespace, body) = minted.split_at(1);

    vec![
        (
            "the minted identifier in lower case".to_string(),
            minted.to_lowercase(),
        ),
        (
            "the kind's own namespace letter over a lower-cased body".to_string(),
            format!("{namespace}{}", body.to_lowercase()),
        ),
        (
            "a UUID, the namespace the policy kinds are keyed by".to_string(),
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "the resource's caller reference rather than its identifier".to_string(),
            reference.to_string(),
        ),
        ("a hyphenated word".to_string(), "not-an-identifier".to_string()),
        (
            "an identifier body carrying a separator".to_string(),
            format!("{namespace}{}_{}", &body[..1], &body[1..]),
        ),
    ]
}

/// Neither the WIDTH nor the NAMESPACE LETTER an identifier carries decides the sentence: every
/// value of the identifier alphabet the store holds no record for — at any width, under any
/// letter, including the identifiers the neighbouring kinds were really minted under — is the
/// ABSENT record, through every operation addressed by one.
#[tokio::test]
async fn an_identifier_of_the_alphabet_the_store_cannot_resolve_reads_as_absent() {
    let router = emulator();
    let seeded = seed(&router).await;
    let public_key = upload_key(&router, 1).await;

    for kind in [Kind::Config, Kind::Profile] {
        let rows = identifiers_of_every_width_and_namespace(kind, &seeded)
            .into_iter()
            .chain(neighbouring_minted_identifiers(kind, &seeded, &public_key));
        for (case, id) in rows {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    kind.absent(),
                );
            }
        }
    }

    // Every refusal above left both seeded resources where they were.
    assert_eq!(
        get_config(&router, &seeded.config_id).await.status,
        200,
        "case the seeded configuration: expected it to survive every refusal"
    );
    assert_eq!(
        get_profile(&router, &seeded.profile_id).await.status,
        200,
        "case the seeded profile: expected it to survive every refusal"
    );
}

/// A value OUTSIDE the identifier alphabet — a lower-cased identifier, a UUID, a caller
/// reference, a word — was never an identifier this service issued, and is the identifier
/// reported UNREADABLE by every operation addressed by one.
#[tokio::test]
async fn a_value_outside_the_identifier_alphabet_is_reported_unreadable() {
    let router = emulator();
    let seeded = seed(&router).await;

    for kind in [Kind::Config, Kind::Profile] {
        for (case, id) in unreadable_identifiers(kind, &seeded) {
            for (operation, answer) in address(&router, kind, &id, &seeded).await {
                assert_refusal(
                    &answer,
                    &format!("{operation} with {case} ({id:?})"),
                    kind.code(),
                    UNREADABLE_IDENTIFIER,
                );
            }
        }
    }

    // And both seeded resources are still readable at their own identifiers afterwards.
    assert_eq!(
        get_config(&router, &seeded.config_id).await.status,
        200,
        "case the seeded configuration: expected it to survive every refusal"
    );
    assert_eq!(
        get_profile(&router, &seeded.profile_id).await.status,
        200,
        "case the seeded profile: expected it to survive every refusal"
    );
}
