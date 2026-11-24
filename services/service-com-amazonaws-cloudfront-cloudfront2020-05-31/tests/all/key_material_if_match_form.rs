// SPDX-License-Identifier: CC0-1.0

//! What a conditional delete on CloudFront's KEY-MATERIAL surface evaluates of the `If-Match`
//! header it carries, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The surface is the key material a field-level encryption configuration
//! reaches: the signed-URL public keys and the profiles that encrypt with them. The rule under
//! test is the one every conditional request of this service follows:
//!
//! * a header that carries no version token at all is "the If-Match version is missing or not
//!   valid", so an absent header, and one carrying anything that is not a version token (a
//!   word, an empty value, a token in lower case, one of another width, a UUID), is refused
//!   HTTP 400 and the record stays exactly where it was, readable at the version it already
//!   had;
//! * a header carrying the version token the record is CURRENTLY at admits the delete, which
//!   answers HTTP 204 with an empty body and removes the record, whichever of the three ways
//!   HTTP and CloudFront's own `ETag` header spell a token it is written in;
//! * a header carrying a version token that is not the addressed record's current one is the
//!   precondition that evaluated to false — HTTP 412 `PreconditionFailed` — whether the token
//!   is one this service issued to ANOTHER record of the account or one it never issued at
//!   all, and the record stays where it was: a caller holding a version it has since replaced
//!   has to read the record back before deleting it;
//! * the header is read where each route reads it: the public key delete answers a header
//!   carrying no version token before it resolves the `Id` — so an absent id is answered on the
//!   header — while the profile delete resolves the `Id` first and answers the absent record
//!   (HTTP 404), each as that route's own recorded answers report. An admissible header always
//!   reaches the lookup, so an id the store holds nothing for is the absent record.
//!
//! The rows span what the rule generalises over: both resource kinds of the surface, the
//! PROVENANCE of the token presented (the record's own, another record's, one never issued) and
//! each of the three ways a token is spelled (bare, quoted, weak), and the forms that carry no
//! version token at all (absent, empty, blank, a word, the record's own token lower-cased, one
//! character short, one character long, a UUID).
//!
//! The resources a caller conditionally REWRITES are held to the same comparison — the
//! field-level encryption configuration among them — which is tabled in
//! [`crate::if_match_is_the_entitys_own_version`].
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the creates and readers are the
//! public key and profile suites' own, and both deletes go over those suites' delete helpers —
//! reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, is_version_token, Answer};
use crate::field_level_encryption_profile_delete::delete_profile;
use crate::field_level_encryption_profile_lifecycle::{
    create_profile, get_profile, upload_key, Entity, Profile,
};
use crate::public_key_delete::delete_key;
use crate::public_key_lifecycle::get_key;

/// `InvalidIfMatchVersion`'s sentence, spelled out here rather than taken from the crate, so a
/// reworded refusal fails this suite.
const INVALID_IF_MATCH_VERSION_MESSAGE: &str =
    "The If-Match version is missing or not valid for the resource.";

/// `PreconditionFailed`'s sentence on this surface, spelled out here for the same reason.
const PRECONDITION_FAILED_MESSAGE: &str =
    "The request failed because it didn't meet the preconditions in one or more \
     request-header fields.";

/// A version token written the way this service writes one — `E` followed by 13 characters of
/// the identifier alphabet — that a fresh emulator never issued to anything, so what a row
/// carrying it shows is that a well-formed token is not the record's version just by being
/// well formed.
const UNISSUED_TOKEN: &str = "EZZZZZZZZZZZZ9";

/// Which record of the surface a row addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    PublicKey,
    Profile,
}

/// Every kind of the surface, so each row runs against both.
const KINDS: [Kind; 2] = [Kind::PublicKey, Kind::Profile];

/// One record of the surface as it stands: the id a delete addresses it by and the version
/// token the service last reported for it.
#[derive(Debug, Clone)]
struct Held {
    id: String,
    etag: String,
}

/// The surface as a row finds it: a public key nothing references and a profile no
/// configuration maps to, each with its own version token, plus a second public key the profile
/// encrypts with.
///
/// The subject public key is NOT the one the profile names, so neither record is referenced by
/// anything and each is deletable on its own — what a row then varies is only the header.
async fn surface(router: &MultiServiceRouter) -> (Held, Held) {
    let subject_key = upload_key(router, 0).await;
    let subject_key_etag = read_token(router, Kind::PublicKey, &subject_key).await;

    let encrypting_key = upload_key(router, 1).await;
    let row = Profile::new(
        "the profile of the key-material surface",
        "if-match-form-profile",
        None,
        vec![Entity::new(&encrypting_key, "provider-one", &["CreditCard"])],
    );
    let created = create_profile(router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "case standing the surface up: expected HTTP 201 from the profile create, got {} with {}",
        created.status, created.raw
    );
    let profile = created.root().text_at("Id").unwrap_or_default().to_string();
    let profile_etag = created.header("etag").to_string();
    assert!(
        is_version_token(&profile_etag),
        "case standing the surface up: expected a minted version token for the profile, got \
         {profile_etag:?}"
    );

    (
        Held {
            id: subject_key,
            etag: subject_key_etag,
        },
        Held {
            id: profile,
            etag: profile_etag,
        },
    )
}

/// The version token the service reports for `id` right now, read through that kind's own
/// reader.
async fn read_token(router: &MultiServiceRouter, kind: Kind, id: &str) -> String {
    let answer = read(router, kind, id).await;
    assert_eq!(
        answer.status, 200,
        "case reading {kind:?} {id}: expected HTTP 200, got {} with {}",
        answer.status, answer.raw
    );
    answer.header("etag").to_string()
}

/// Read the record `id` names through the reader of its kind.
async fn read(router: &MultiServiceRouter, kind: Kind, id: &str) -> Answer {
    match kind {
        Kind::PublicKey => get_key(router, id).await,
        Kind::Profile => get_profile(router, id).await,
    }
}

/// `DELETE` the record `id` names, under the `If-Match` header `if_match`.
async fn delete(
    router: &MultiServiceRouter,
    kind: Kind,
    id: &str,
    if_match: Option<&str>,
) -> Answer {
    match kind {
        Kind::PublicKey => delete_key(router, id, if_match).await,
        Kind::Profile => delete_profile(router, id, if_match).await,
    }
}

/// An id of the shape this service mints for `kind` that a fresh emulator holds no record
/// under, so a delete addressing it reaches the lookup and nothing else.
fn absent_id(kind: Kind) -> &'static str {
    match kind {
        Kind::PublicKey => "K9ZZZZZZZZZZZZ",
        Kind::Profile => "P9ZZZZZZZZZZZZ",
    }
}

/// The error code a delete of `kind` answers an id the store holds no record under.
fn absent_code(kind: Kind) -> &'static str {
    match kind {
        Kind::PublicKey => "NoSuchPublicKey",
        Kind::Profile => "NoSuchFieldLevelEncryptionProfile",
    }
}

/// What a delete of `kind` answers an ABSENT id under a header that carries no version token:
/// the status and code of whichever of the two refusals that route reaches first.
///
/// The public key delete reads the header before it resolves the `Id`; the profile delete
/// resolves the `Id` first, which is why a malformed profile id is answered on the id. Each
/// route's order is its own recorded contract.
fn absent_id_under_an_unreadable_header(kind: Kind) -> (u16, &'static str) {
    match kind {
        Kind::PublicKey => (400, "InvalidIfMatchVersion"),
        Kind::Profile => (404, absent_code(Kind::Profile)),
    }
}

/// Where a row takes the token it presents from.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// The token this service issued to the OTHER record of the surface.
    Other,
    /// A token this service never issued to anything ([`UNISSUED_TOKEN`]).
    Unissued,
}

/// How a row spells the token in the header: bare, as CloudFront's own `ETag` header reports
/// it, or quoted or weak, as HTTP spells an entity tag.
type Spelling = fn(&str) -> String;

/// RULE: the version token the record is CURRENTLY at admits the delete, whichever way the
/// header spells it, so the record is removed and the id afterwards names nothing.
#[tokio::test]
async fn the_records_own_version_token_admits_the_delete() {
    let rows: &[(&str, Spelling)] = &[
        ("its own token, bare", |token| token.to_string()),
        ("its own token, quoted", |token| format!("\"{token}\"")),
        ("its own token, weak", |token| format!("W/\"{token}\"")),
    ];

    for kind in KINDS {
        for (description, spell) in rows {
            let case = format!("{kind:?} deleted under {description}");
            // A fresh emulator per row: the delete under test removes the record, so each row
            // starts from a surface no earlier row has touched.
            let router = emulator();
            let (key, profile) = surface(&router).await;
            let (addressed, other) = match kind {
                Kind::PublicKey => (&key, &profile),
                Kind::Profile => (&profile, &key),
            };

            let answer = delete(&router, kind, &addressed.id, Some(&spell(&addressed.etag))).await;
            assert_eq!(
                answer.status, 204,
                "case {case}: expected HTTP 204, got {} with {}",
                answer.status, answer.raw
            );
            assert!(
                answer.raw.trim().is_empty(),
                "case {case}: expected an empty body, got {}",
                answer.raw
            );

            // Gone means gone: the record every read answered from is removed.
            let fetched = read(&router, kind, &addressed.id).await;
            assert_eq!(
                fetched.status, 404,
                "case {case}: expected the deleted record to be gone, got {} with {}",
                fetched.status, fetched.raw
            );
            assert_eq!(
                fetched.error_code(),
                absent_code(kind),
                "case {case}: expected {}, got {}",
                absent_code(kind),
                fetched.raw
            );

            // The OTHER record of the surface is untouched, still at the version it held: the
            // delete removed the record it addressed and nothing else.
            let reported = read_token(&router, other_kind(kind), &other.id).await;
            assert_eq!(
                reported, other.etag,
                "case {case}: expected the other record to keep its version {:?}, got \
                 {reported:?}",
                other.etag
            );
        }
    }
}

/// RULE: a well-formed version token that is NOT the addressed record's current one is the
/// precondition that evaluated to false — HTTP 412 — whoever it was issued to, and the record
/// stays readable at the version it already had.
#[tokio::test]
async fn a_version_token_that_is_not_the_records_own_fails_the_precondition() {
    let rows: &[(&str, Source, Spelling)] = &[
        (
            "a token this service issued to the other record",
            Source::Other,
            |token| token.to_string(),
        ),
        (
            "a token this service issued to the other record, quoted",
            Source::Other,
            |token| format!("\"{token}\""),
        ),
        (
            "a token this service never issued",
            Source::Unissued,
            |token| token.to_string(),
        ),
        (
            "a token this service never issued, weak",
            Source::Unissued,
            |token| format!("W/\"{token}\""),
        ),
    ];

    for kind in KINDS {
        for (description, source, spell) in rows {
            let case = format!("{kind:?} deleted under {description}");
            let router = emulator();
            let (key, profile) = surface(&router).await;
            let (addressed, other) = match kind {
                Kind::PublicKey => (&key, &profile),
                Kind::Profile => (&profile, &key),
            };
            let token = match source {
                Source::Other => other.etag.clone(),
                Source::Unissued => UNISSUED_TOKEN.to_string(),
            };

            let answer = delete(&router, kind, &addressed.id, Some(&spell(&token))).await;
            assert_eq!(
                answer.status, 412,
                "case {case}: expected HTTP 412, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "PreconditionFailed",
                "case {case}: expected PreconditionFailed, got {}",
                answer.raw
            );
            assert_eq!(
                answer.root().find_text("Message").unwrap_or_default(),
                PRECONDITION_FAILED_MESSAGE,
                "case {case}: expected the service's sentence for PreconditionFailed, got {}",
                answer.raw
            );

            // The refusal removed nothing and advanced nothing, so the caller that reads the
            // record back and presents the version it reports gets the delete served.
            let reported = read_token(&router, kind, &addressed.id).await;
            assert_eq!(
                reported, addressed.etag,
                "case {case}: expected the record to keep its version {:?}, got {reported:?}",
                addressed.etag
            );
            let served = delete(&router, kind, &addressed.id, Some(&reported)).await;
            assert_eq!(
                served.status, 204,
                "case {case}: expected the delete under the reported version to be served, got \
                 {} with {}",
                served.status, served.raw
            );
        }
    }
}

/// The kind a row's OTHER record is, which is the one it is read back through.
fn other_kind(kind: Kind) -> Kind {
    match kind {
        Kind::PublicKey => Kind::Profile,
        Kind::Profile => Kind::PublicKey,
    }
}

/// RULE: a header that carries no version token at all is "missing or not valid" — refused
/// HTTP 400 `InvalidIfMatchVersion`, on the header rather than on the id, with the record left
/// exactly where it was.
#[tokio::test]
async fn a_header_carrying_no_version_token_is_refused() {
    for kind in KINDS {
        let router = emulator();
        let (key, profile) = surface(&router).await;
        let addressed = match kind {
            Kind::PublicKey => key,
            Kind::Profile => profile,
        };

        // The forms that carry no version token: the header absent, empty or blank, a word, the
        // record's own token in lower case, and one of the wrong width in either direction —
        // a version token is `E` followed by 13 characters of the identifier alphabet.
        let shortened = addressed.etag[..addressed.etag.len() - 1].to_string();
        let lengthened = format!("{}7", addressed.etag);
        let rows: &[(&str, Option<&str>)] = &[
            ("no If-Match at all", None),
            ("an empty If-Match", Some("")),
            ("a blank If-Match", Some("   ")),
            ("an If-Match that is a word", Some("not-a-version")),
            (
                "an If-Match carrying a UUID",
                Some("7f1c3a4e-5b6d-4f70-8a91-2c3d4e5f6a7b"),
            ),
            (
                "an If-Match carrying its own token in lower case",
                Some(&addressed.etag.to_lowercase()),
            ),
            (
                "an If-Match carrying a token one character short",
                Some(&shortened),
            ),
            (
                "an If-Match carrying a token one character long",
                Some(&lengthened),
            ),
        ];

        for (description, if_match) in rows {
            let case = format!("{kind:?} addressed with {description}");
            let answer = delete(&router, kind, &addressed.id, *if_match).await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidIfMatchVersion",
                "case {case}: expected InvalidIfMatchVersion, got {}",
                answer.raw
            );
            assert_eq!(
                answer.root().find_text("Message").unwrap_or_default(),
                INVALID_IF_MATCH_VERSION_MESSAGE,
                "case {case}: expected the service's sentence for InvalidIfMatchVersion, got {}",
                answer.raw
            );

            // The refusal removed nothing and advanced nothing: the record is still readable at
            // the version it already had.
            let reported = read_token(&router, kind, &addressed.id).await;
            assert_eq!(
                reported, addressed.etag,
                "case {case}: expected the record to keep its version {:?}, got {reported:?}",
                addressed.etag
            );

            // The same header against an id the store holds nothing for: answered by whichever
            // refusal this route reaches first.
            let (expected_status, expected_code) = absent_id_under_an_unreadable_header(kind);
            let unknown = delete(&router, kind, absent_id(kind), *if_match).await;
            assert_eq!(
                unknown.status, expected_status,
                "case {case} on an absent id: expected HTTP {expected_status}, got {} with {}",
                unknown.status, unknown.raw
            );
            assert_eq!(
                unknown.error_code(),
                expected_code,
                "case {case} on an absent id: expected {expected_code}, got {}",
                unknown.raw
            );
        }

        // An admissible header against an id the store holds nothing for reaches the lookup and
        // is answered on the id.
        for (description, token) in [
            ("its own token", addressed.etag.as_str()),
            ("a token this service never issued", UNISSUED_TOKEN),
        ] {
            let case = format!("{kind:?}'s absent id addressed with {description}");
            let unknown = delete(&router, kind, absent_id(kind), Some(token)).await;
            assert_eq!(
                unknown.status, 404,
                "case {case}: expected HTTP 404, got {} with {}",
                unknown.status, unknown.raw
            );
            assert_eq!(
                unknown.error_code(),
                absent_code(kind),
                "case {case}: expected {}, got {}",
                absent_code(kind),
                unknown.raw
            );
        }
    }
}
