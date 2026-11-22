// SPDX-License-Identifier: CC0-1.0

//! A field-level encryption identifier in the REQUEST PATH earns the sentence its own
//! FORM decides, end to end over HTTP.
//!
//! The rule under test: an operation ADDRESSED by a field-level encryption identifier answers
//! its one modelled not-found code (404) for every identifier it cannot resolve — these
//! operations model no argument error
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryptionConfig")` reports
//! `errors: [AccessDenied, NoSuchFieldLevelEncryptionConfig]`) — so the MESSAGE is what tells
//! the two conditions apart, and what decides it is how much of the identifier's FORM that
//! route reads, which is the route's own recorded contract: the `/config` reads, the profile's
//! reads, the updates and the deletes read which NAMESPACE the value was issued in, so a value
//! written under a letter this service issues identifiers under that is not the addressed
//! kind's own identifier form is the identifier the service reports it cannot READ ("Id format
//! is invalid"), while `GetFieldLevelEncryption`, the configuration's resource read, reads the
//! identifier ALPHABET alone (digits and uppercase letters, carrying no separator and no lower
//! case). A value either route admits that the store holds nothing for is the ABSENT resource
//! ("The specified field level encryption config/profile does not exist."), and a value outside
//! the alphabet is unreadable on every route. Both sentences are fixed text: neither echoes the
//! value submitted.
//!
//! So the rows span what the rule generalises over:
//!
//! * both RESOURCE KINDS of this surface — the configuration (`C`-prefixed) and the profile
//!   (`P`-prefixed) — each with its own sentence;
//! * every OPERATION that addresses one by a path identifier: both readers of each kind, its
//!   update and its delete;
//! * the values of the kind's own minted form the store holds nothing for: one of the
//!   kind's own namespace that was never minted and the minted id with its last character
//!   changed — each the ABSENT resource on every route;
//! * the values of the identifier alphabet written under a namespace letter this service
//!   issues identifiers under that the addressed kind's own form rules out: bodies one
//!   character short of and one character past the width the kind is issued at, the sibling
//!   kind's own minted id, a public key id and the `E`-prefixed namespace the other CloudFront
//!   entity ids are drawn from;
//! * the values of the alphabet under a letter that identifies NO namespace of this service,
//!   which say nothing about the kind that issued them and are therefore carried through to the
//!   store: an unminted namespace letter at the issued width and over a far longer body;
//! * and the values written outside the alphabet: a minted id in lower case, a body outside the
//!   id alphabet, a UUID, a bare word and the resource's own caller reference — each the
//!   identifier reported UNREADABLE on every route.
//!
//! Each kind's width and alphabet are read off an id this emulator really MINTED rather than
//! written out here, so the table cannot drift from what the service issues.
//!
//! The counterpart case: an identifier borne by a submitted DOCUMENT — a mapping's `ProfileId`
//! on a configuration create — addresses nothing, so it is resolved against the store the same
//! way whatever its shape and reports the absent profile.
//!
//! The wire harness, the configuration and profile builders and their create and delete paths
//! are the crate's own, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::field_level_encryption_config_delete::delete_config;
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

/// The sentence these operations answer a value written outside the identifier alphabet,
/// naming the member that carried it.
const UNREADABLE_IDENTIFIER: &str = "Id format is invalid";

/// The caller references the two stored resources of each case are held under.
const CONFIG_REFERENCE: &str = "fle-id-format-config";
const PROFILE_REFERENCE: &str = "fle-id-format-profile";

// ---------------------------------------------------------------------------
// The surface this suite drives
// ---------------------------------------------------------------------------

/// Which field-level encryption resource an operation's path identifier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Config,
    Profile,
}

/// One operation addressed by a field-level encryption identifier in its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    GetFieldLevelEncryption,
    GetFieldLevelEncryptionConfig,
    UpdateFieldLevelEncryptionConfig,
    DeleteFieldLevelEncryptionConfig,
    GetFieldLevelEncryptionProfile,
    GetFieldLevelEncryptionProfileConfig,
    UpdateFieldLevelEncryptionProfile,
    DeleteFieldLevelEncryptionProfile,
}

/// Every operation of this surface that addresses its resource by a path identifier.
const OPS: &[Op] = &[
    Op::GetFieldLevelEncryption,
    Op::GetFieldLevelEncryptionConfig,
    Op::UpdateFieldLevelEncryptionConfig,
    Op::DeleteFieldLevelEncryptionConfig,
    Op::GetFieldLevelEncryptionProfile,
    Op::GetFieldLevelEncryptionProfileConfig,
    Op::UpdateFieldLevelEncryptionProfile,
    Op::DeleteFieldLevelEncryptionProfile,
];

impl Op {
    /// The resource kind this operation's path identifier names.
    fn kind(self) -> Kind {
        match self {
            Self::GetFieldLevelEncryption
            | Self::GetFieldLevelEncryptionConfig
            | Self::UpdateFieldLevelEncryptionConfig
            | Self::DeleteFieldLevelEncryptionConfig => Kind::Config,
            _ => Kind::Profile,
        }
    }

    /// The modelled error code the operation answers an identifier it cannot resolve.
    fn code(self) -> &'static str {
        match self.kind() {
            Kind::Config => "NoSuchFieldLevelEncryptionConfig",
            Kind::Profile => "NoSuchFieldLevelEncryptionProfile",
        }
    }

    /// The sentence the operation answers a value written in the identifier alphabet that the
    /// store holds no resource for.
    fn absent(self) -> &'static str {
        match self.kind() {
            Kind::Config => NO_SUCH_CONFIG,
            Kind::Profile => NO_SUCH_PROFILE,
        }
    }

    /// Whether this operation reads which NAMESPACE the value was issued in before it resolves
    /// it — its own recorded contract. The configuration's RESOURCE read is the one operation
    /// of this surface that reads the alphabet alone.
    fn reads_the_namespace(self) -> bool {
        !matches!(self, Self::GetFieldLevelEncryption)
    }

    /// Send this operation at `id`, carrying the stored resource's current version token so
    /// every refusal below is about the identifier rather than about a precondition, and a
    /// well-formed document on the updates so none of them is refused for its payload.
    async fn send(self, router: &MultiServiceRouter, id: &str, stored: &Stored) -> Answer {
        match self {
            Self::GetFieldLevelEncryption => get_config(router, id).await,
            Self::GetFieldLevelEncryptionConfig => get_config_document(router, id).await,
            Self::UpdateFieldLevelEncryptionConfig => {
                call(
                    router,
                    "PUT",
                    &format!("/{API_VERSION}/field-level-encryption/{id}/config"),
                    Some(&stored.config_document),
                    Some(&stored.config_etag),
                )
                .await
            }
            Self::DeleteFieldLevelEncryptionConfig => {
                delete_config(router, id, Some(&stored.config_etag)).await
            }
            Self::GetFieldLevelEncryptionProfile => get_profile(router, id).await,
            Self::GetFieldLevelEncryptionProfileConfig => get_profile_config(router, id).await,
            Self::UpdateFieldLevelEncryptionProfile => {
                call(
                    router,
                    "PUT",
                    &format!("/{API_VERSION}/field-level-encryption-profile/{id}/config"),
                    Some(&stored.profile_document),
                    Some(&stored.profile_etag),
                )
                .await
            }
            Self::DeleteFieldLevelEncryptionProfile => {
                call(
                    router,
                    "DELETE",
                    &format!("/{API_VERSION}/field-level-encryption-profile/{id}"),
                    None,
                    Some(&stored.profile_etag),
                )
                .await
            }
        }
    }
}

/// The two resources this emulator really stored for a case, and the documents and version
/// tokens the updates above submit against them.
struct Stored {
    config_id: String,
    config_etag: String,
    config_document: String,
    profile_id: String,
    profile_etag: String,
    profile_document: String,
}

impl Stored {
    /// The id this emulator minted for `kind` — the row every shape below is derived from.
    fn minted(&self, kind: Kind) -> &str {
        match kind {
            Kind::Config => &self.config_id,
            Kind::Profile => &self.profile_id,
        }
    }
}

/// Store one profile (encrypting with a freshly uploaded public key) and one configuration
/// naming it, reporting both resources with the documents and tokens the updates need.
async fn seed(router: &MultiServiceRouter) -> Stored {
    let key = upload_key(router, 0).await;
    let profile_document = Profile::new(
        "the stored profile",
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

    let config_document = simple("the stored configuration", CONFIG_REFERENCE, &profile_id).xml();
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

    Stored {
        config_etag: created_config.header("etag").to_string(),
        config_id,
        config_document,
        profile_etag: created_profile.header("etag").to_string(),
        profile_id,
        profile_document,
    }
}

/// One identifier a case submits where an identifier of the addressed kind belongs, and what
/// its FORM says about it — which, together with how much of the form the route reads, decides
/// the sentence it earns.
struct Shape {
    case: String,
    id: String,
    in_the_identifier_alphabet: bool,
    could_be_issued_for_the_kind: bool,
}

impl Shape {
    /// A value of the form the addressed kind's identifiers are MINTED in: every route of this
    /// surface resolves it, so the store holding nothing for it is the absent resource.
    fn own(case: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            case: case.into(),
            id: id.into(),
            in_the_identifier_alphabet: true,
            could_be_issued_for_the_kind: true,
        }
    }

    /// A value written in the identifier alphabet under a namespace letter this service issues
    /// identifiers under that the addressed kind's own form rules out — another width under the
    /// kind's own letter, or another kind's namespace letter.
    fn another_namespace(case: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            case: case.into(),
            id: id.into(),
            in_the_identifier_alphabet: true,
            could_be_issued_for_the_kind: false,
        }
    }

    /// A value written in the identifier alphabet under a letter that identifies NO namespace of
    /// this service: nothing about it says which kind issued it, so every route carries it
    /// through to the store.
    fn no_namespace(case: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            case: case.into(),
            id: id.into(),
            in_the_identifier_alphabet: true,
            could_be_issued_for_the_kind: true,
        }
    }

    /// A value written outside that alphabet, which no identifier CloudFront issues is: the
    /// identifier the service reports it cannot read, on every route.
    fn foreign(case: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            case: case.into(),
            id: id.into(),
            in_the_identifier_alphabet: false,
            could_be_issued_for_the_kind: false,
        }
    }

    /// The sentence this identifier earns from `op`: the absent resource where the form `op`
    /// reads admits the value, and the unreadable identifier where it does not.
    fn expected(&self, op: Op) -> &'static str {
        let readable = if op.reads_the_namespace() {
            self.could_be_issued_for_the_kind
        } else {
            self.in_the_identifier_alphabet
        };
        if readable {
            op.absent()
        } else {
            UNREADABLE_IDENTIFIER
        }
    }
}

/// The identifiers a case submits where an identifier of `kind` belongs, derived from the ids
/// this emulator really minted: the body's width and alphabet are read off the minted id, so
/// nothing here is a width written out by hand.
fn shapes(kind: Kind, stored: &Stored, public_key: &str) -> Vec<Shape> {
    let minted = stored.minted(kind);
    let (prefix, body) = minted.split_at(1);
    let sibling = match kind {
        Kind::Config => stored.minted(Kind::Profile),
        Kind::Profile => stored.minted(Kind::Config),
    };
    let reference = match kind {
        Kind::Config => CONFIG_REFERENCE,
        Kind::Profile => PROFILE_REFERENCE,
    };

    let mut changed: Vec<char> = minted.chars().collect();
    if let Some(last) = changed.last_mut() {
        *last = if *last == 'A' { 'B' } else { 'A' };
    }
    let changed: String = changed.into_iter().collect();

    vec![
        // Values of the form this kind's identifiers are minted in, which the store holds
        // nothing for.
        Shape::own(
            "an id of this kind that was never minted",
            format!("{prefix}{}", "0".repeat(body.len())),
        ),
        Shape::own("the minted id with its last character changed", changed),
        // Values of the identifier alphabet written under a namespace letter this service
        // issues identifiers under that this kind's own form rules out.
        Shape::another_namespace(
            "this kind's prefix with a body one character short",
            format!("{prefix}{}", &body[..body.len() - 1]),
        ),
        Shape::another_namespace(
            "this kind's prefix with a body one character too long",
            format!("{prefix}{body}A"),
        ),
        Shape::another_namespace(
            "the sibling field-level encryption kind's own minted id",
            sibling,
        ),
        Shape::another_namespace("a public key id this service minted", public_key),
        Shape::another_namespace(
            "the namespace every other CloudFront entity id is drawn from",
            format!("E{body}"),
        ),
        // And the values of the alphabet under a letter that identifies no namespace of this
        // service, which say nothing about the kind that issued them.
        Shape::no_namespace(
            "a namespace letter no kind of this service mints",
            format!("W{body}"),
        ),
        Shape::no_namespace(
            "a namespace letter no kind of this service mints and a far longer body",
            format!("W{body}{body}"),
        ),
        // And every value written outside that alphabet.
        Shape::foreign("the minted id in lower case", minted.to_lowercase()),
        Shape::foreign(
            "this kind's prefix with a body outside the id alphabet",
            format!("{prefix}{}", body.to_lowercase()),
        ),
        Shape::foreign(
            "a UUID, the namespace the policy kinds are keyed by",
            "00000000-0000-0000-0000-000000000000",
        ),
        Shape::foreign("a bare word", "not-an-identifier"),
        Shape::foreign(
            "the resource's caller reference rather than its id",
            reference,
        ),
    ]
}

/// Assert one refusal: its status, its code and — the point of this suite — its message.
fn assert_refusal(answer: &Answer, case: &str, code: &str, message: &str) {
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

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// Every operation addressed by a field-level encryption identifier answers its one not-found
/// code for an identifier it cannot resolve, and the identifier's FORM — judged by as much of
/// it as the route reads — decides which of that code's two sentences: the absent-resource
/// sentence where the form admits the value, and the unreadable-identifier sentence where it
/// does not.
#[tokio::test]
async fn a_path_identifier_that_resolves_to_nothing_earns_the_sentence_its_form_decides() {
    let router = emulator();
    let stored = seed(&router).await;
    let public_key = upload_key(&router, 1).await;

    for op in OPS {
        for shape in shapes(op.kind(), &stored, &public_key) {
            let answer = op.send(&router, &shape.id, &stored).await;
            assert_refusal(
                &answer,
                &format!("{op:?} with {}", shape.case),
                op.code(),
                shape.expected(*op),
            );
        }
    }

    // Every refusal above left both resources where they were: each is still readable at its
    // own id, so the sentences answered an identifier rather than a store this suite broke.
    let config = get_config(&router, &stored.config_id).await;
    assert_eq!(
        config.status, 200,
        "case the stored configuration: expected it to survive every refusal, got {} with {}",
        config.status, config.raw
    );
    let profile = get_profile(&router, &stored.profile_id).await;
    assert_eq!(
        profile.status, 200,
        "case the stored profile: expected it to survive every refusal, got {} with {}",
        profile.status, profile.raw
    );
}

/// A well-formed identifier of the addressed kind that the store DOES hold is served, so
/// nothing above gates what a caller may legitimately ask for: both readers of each kind
/// answer 200 at the minted id, at the version token its create handed back.
#[tokio::test]
async fn a_minted_identifier_is_still_served_by_every_reader() {
    let router = emulator();
    let stored = seed(&router).await;

    let readers = [
        (
            "GetFieldLevelEncryption",
            get_config(&router, &stored.config_id).await,
            stored.config_etag.clone(),
        ),
        (
            "GetFieldLevelEncryptionConfig",
            get_config_document(&router, &stored.config_id).await,
            stored.config_etag.clone(),
        ),
        (
            "GetFieldLevelEncryptionProfile",
            get_profile(&router, &stored.profile_id).await,
            stored.profile_etag.clone(),
        ),
        (
            "GetFieldLevelEncryptionProfileConfig",
            get_profile_config(&router, &stored.profile_id).await,
            stored.profile_etag.clone(),
        ),
    ];

    for (reader, answer, etag) in readers {
        assert_eq!(
            answer.status, 200,
            "case {reader}: expected HTTP 200 at the minted id, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.header("etag"),
            etag,
            "case {reader}: expected the version token the create handed back, got {:?}",
            answer.header("etag")
        );
    }
}

/// An identifier borne by a submitted DOCUMENT is resolved the same way: a mapping's
/// `ProfileId` on a configuration create reports the ABSENT profile whatever shape that id
/// takes.
#[tokio::test]
async fn a_document_borne_profile_id_reports_the_absent_profile_whatever_its_shape() {
    let router = emulator();
    let stored = seed(&router).await;
    let public_key = upload_key(&router, 1).await;

    for shape in shapes(Kind::Profile, &stored, &public_key) {
        let created = create_config(
            &router,
            &simple(
                "a configuration naming a profile the store does not hold",
                &format!("fle-id-format-document-{}", shape.id.to_lowercase()),
                &shape.id,
            )
            .xml(),
        )
        .await;
        assert_refusal(
            &created,
            &format!("CreateFieldLevelEncryptionConfig with {}", shape.case),
            "NoSuchFieldLevelEncryptionProfile",
            NO_SUCH_PROFILE,
        );
    }

    // And the configuration the account really holds is untouched by those refusals.
    assert_eq!(
        get_config(&router, &stored.config_id).await.status,
        200,
        "case the stored configuration: expected it to survive every refused create"
    );
}
