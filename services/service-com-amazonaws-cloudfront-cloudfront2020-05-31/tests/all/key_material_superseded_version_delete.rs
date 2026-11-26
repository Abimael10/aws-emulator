// SPDX-License-Identifier: CC0-1.0

//! A conditional delete on CloudFront's KEY-MATERIAL surface under a version the record itself
//! has since replaced, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The surface is the key material a field-level encryption configuration reaches:
//! the signed-URL public keys and the profiles that encrypt with them. The rule under test is
//! the one the modelled `PreconditionFailed` states ("the precondition in one or more of the
//! request fields evaluated to false"): a conditional delete compares the `If-Match` header
//! against the version token the addressed record is CURRENTLY at, so
//!
//! * a token the record ITSELF carried before a later write is no longer its version: the delete
//!   is refused HTTP 412 `PreconditionFailed` — in the restXml `ErrorResponse` envelope, with the
//!   fault `Sender`, the fixed sentence for this error class and the `RequestId` of the call —
//!   and the refusal is made BEFORE anything is removed, so the record stays readable at the
//!   version it is now at, however many writes ago the presented token was spent;
//! * because the refusal removes nothing, the conventional sequence a client follows — attempt,
//!   be refused, re-read the record's `ETag`, retry conditionally — completes: the retry
//!   carrying the version the read reports is served HTTP 204 with an empty body, and only then
//!   does the id name no record (HTTP 404).
//!
//! The rows span what the rule generalises over: both resource kinds of the surface, the NUMBER
//! of writes standing between the presented token and the current one (so the token under test
//! is the one the create minted as well as the one the last write replaced), and each of the
//! three ways a token is spelled in the header (bare, as CloudFront's own `ETag` reports it, and
//! quoted or weak, as HTTP spells an entity tag).
//!
//! What a delete reads of the header's FORM, and a token issued to ANOTHER record of the
//! account, are tabled in [`crate::key_material_if_match_form`]; this suite is about a token the
//! addressed record's own history holds. The refusal's body is asserted as the document a client
//! parses — the envelope the transport then frames with a content length, which an in-process
//! dispatch does not add.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the creates, readers and deletes are
//! the public key and profile suites' own helpers — reused rather than copied — and only the two
//! config-rewrite routes are spelled out here.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, emulator, is_minted_uuid, is_version_token, Answer, Node, API_VERSION,
};
use crate::field_level_encryption_profile_delete::delete_profile;
use crate::field_level_encryption_profile_lifecycle::{
    create_profile, get_profile, upload_key, Entity, Profile,
};
use crate::public_key_delete::delete_key;
use crate::public_key_lifecycle::{create_key, get_key, pem, Key};

/// `PreconditionFailed`'s sentence, spelled out here rather than taken from the crate, so a
/// reworded refusal fails this suite. Every word of it is fixed for the error class: no
/// identifier, name or limit of the request is substituted in.
const PRECONDITION_FAILED_MESSAGE: &str =
    "The request failed because it didn't meet the preconditions in one or more \
     request-header fields.";

/// The fault a failed precondition carries in the envelope: the condition the CALLER stated did
/// not hold, so the error is the sender's.
const SENDER_FAULT: &str = "Sender";

/// Which record of the surface a row addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    PublicKey,
    Profile,
}

/// Every kind of the surface, so each row runs against both.
const KINDS: [Kind; 2] = [Kind::PublicKey, Kind::Profile];

/// How many writes a case puts between the create and the delete it attempts: with one write the
/// token under test is the one the create minted, and with more the rows cover every token the
/// record has left behind, not only the one the last write replaced.
const WRITES: [usize; 3] = [1, 2, 3];

/// How a row spells the token in the header: bare, as CloudFront's own `ETag` header reports it,
/// or quoted or weak, as HTTP spells an entity tag.
type Spelling = fn(&str) -> String;

/// Each spelling, with the name every assertion of that row carries.
const SPELLINGS: &[(&str, Spelling)] = &[
    ("bare", |token| token.to_string()),
    ("quoted", |token| format!("\"{token}\"")),
    ("weak", |token| format!("W/\"{token}\"")),
];

// ---------------------------------------------------------------------------
// Standing a record up, and moving it on
// ---------------------------------------------------------------------------

/// `PUT /2020-05-31/public-key/{Id}/config` — the rewrite that moves a public key on, under the
/// version token the key is currently at.
async fn update_key(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/public-key/{id}/config"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// `PUT /2020-05-31/field-level-encryption-profile/{Id}/config` — the same for a profile.
async fn update_profile(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/field-level-encryption-profile/{id}/config"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// One record of the surface and every version token it has held.
#[derive(Debug)]
struct Held {
    /// The id a delete addresses the record by.
    id: String,
    /// The token the record is at now — the only one a conditional delete satisfies.
    current: String,
    /// Every token the record held before `current`, oldest first.
    superseded: Vec<String>,
}

/// Fold a write's answer into `held`: the token it minted becomes the current one, and the token
/// it replaced joins the versions the record has left behind.
fn advance(held: &mut Held, answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200 from the rewrite, got {} with {}",
        answer.status, answer.raw
    );
    let minted = answer.header("etag").to_string();
    assert!(
        is_version_token(&minted),
        "case {case}: expected a minted version token from the rewrite, got {minted:?}"
    );
    assert_ne!(
        minted, held.current,
        "case {case}: expected the rewrite to mint a version token the record did not already \
         hold, got {minted:?} again"
    );
    assert!(
        !held.superseded.contains(&minted),
        "case {case}: expected a version token the record never held, got {minted:?}, already \
         among {:?}",
        held.superseded
    );
    let replaced = std::mem::replace(&mut held.current, minted);
    held.superseded.push(replaced);
}

/// The `PublicKeyConfig` a case submits: the ONE member a rewrite may move is the comment, so
/// every other member is resubmitted exactly as the create stated it.
fn key_config(reference: &str, comment: Option<&str>) -> Key {
    Key {
        case: format!("public key {reference}"),
        caller_reference: reference.to_string(),
        name: reference.to_string(),
        // The PEM-enveloped RSA public key the public key suite writes: the material is
        // immaterial to this rule, and `EncodedKey` is fixed for the key's life.
        encoded_key: pem(2, 'K'),
        comment: comment.map(str::to_string),
    }
}

/// Upload a public key and move it on `writes` times, reporting it with every version it held.
async fn stand_up_key(router: &MultiServiceRouter, reference: &str, writes: usize) -> Held {
    let created = create_key(router, &key_config(reference, None).xml()).await;
    assert_eq!(
        created.status, 201,
        "case {reference}: expected HTTP 201 from the public key create, got {} with {}",
        created.status, created.raw
    );
    let mut held = Held {
        id: created.root().text_at("Id").unwrap_or_default().to_string(),
        current: created.header("etag").to_string(),
        superseded: Vec::new(),
    };
    assert!(
        is_version_token(&held.current),
        "case {reference}: expected a minted version token from the create, got {:?}",
        held.current
    );

    for write in 1..=writes {
        let comment = format!("the public key after write {write}");
        let body = key_config(reference, Some(&comment)).xml();
        let answer = update_key(router, &held.id, &body, &held.current).await;
        advance(
            &mut held,
            &answer,
            &format!("public key {reference}, write {write}"),
        );
    }
    held
}

/// Create a field-level encryption profile and move it on `writes` times, reporting it with
/// every version it held.
async fn stand_up_profile(router: &MultiServiceRouter, reference: &str, writes: usize) -> Held {
    // A profile's encryption entity names a public key that must already be stored, so the key
    // comes from the public key surface itself. It is NOT the record under test, and nothing
    // names the profile, so the profile is deletable on its own throughout.
    let encrypting_key = upload_key(router, 0).await;
    let row = Profile::new(
        "the profile under the superseded-version gate",
        reference,
        None,
        vec![Entity::new(&encrypting_key, "provider-one", &["CreditCard"])],
    );
    let created = create_profile(router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "case {reference}: expected HTTP 201 from the profile create, got {} with {}",
        created.status, created.raw
    );
    let mut held = Held {
        id: created.root().text_at("Id").unwrap_or_default().to_string(),
        current: created.header("etag").to_string(),
        superseded: Vec::new(),
    };
    assert!(
        is_version_token(&held.current),
        "case {reference}: expected a minted version token from the create, got {:?}",
        held.current
    );

    for write in 1..=writes {
        // The caller reference identifies one profile for its whole life, so a rewrite
        // resubmits it and moves the comment.
        let comment = format!("the profile after write {write}");
        let mut next = row.clone();
        next.comment = Some(comment);
        let answer = update_profile(router, &held.id, &next.xml(), &held.current).await;
        advance(
            &mut held,
            &answer,
            &format!("profile {reference}, write {write}"),
        );
    }
    held
}

/// Stand a record of `kind` up under `reference` and move it on `writes` times.
async fn stand_up(
    router: &MultiServiceRouter,
    kind: Kind,
    reference: &str,
    writes: usize,
) -> Held {
    match kind {
        Kind::PublicKey => stand_up_key(router, reference, writes).await,
        Kind::Profile => stand_up_profile(router, reference, writes).await,
    }
}

// ---------------------------------------------------------------------------
// Reading and deleting, through each kind's own route
// ---------------------------------------------------------------------------

/// Read the record `id` names through the reader of its kind.
async fn read(router: &MultiServiceRouter, kind: Kind, id: &str) -> Answer {
    match kind {
        Kind::PublicKey => get_key(router, id).await,
        Kind::Profile => get_profile(router, id).await,
    }
}

/// `DELETE` the record `id` names, carrying the `If-Match` header `if_match`.
async fn delete(
    router: &MultiServiceRouter,
    kind: Kind,
    id: &str,
    if_match: &str,
) -> Answer {
    match kind {
        Kind::PublicKey => delete_key(router, id, Some(if_match)).await,
        Kind::Profile => delete_profile(router, id, Some(if_match)).await,
    }
}

/// The error code a read or delete of `kind` answers an id the store holds no record under.
fn absent_code(kind: Kind) -> &'static str {
    match kind {
        Kind::PublicKey => "NoSuchPublicKey",
        Kind::Profile => "NoSuchFieldLevelEncryptionProfile",
    }
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// RULE: a conditional delete compares the `If-Match` header against the version the addressed
/// record is CURRENTLY at, so a token the record itself held before a later write is a
/// precondition that evaluated to false — HTTP 412 in the restXml error envelope — and the
/// record is left exactly where it was, still readable at the version it is now at.
#[tokio::test]
async fn a_version_the_record_has_since_replaced_fails_the_precondition() {
    for kind in KINDS {
        for writes in WRITES {
            // A fresh emulator per case: the record is moved on a different number of times in
            // each, and nothing an earlier case left behind takes part.
            let router = emulator();
            let reference = format!("superseded-delete-{writes}");
            let held = stand_up(&router, kind, &reference, writes).await;

            for (age, stale) in held.superseded.iter().enumerate() {
                let writes_since = held.superseded.len() - age;
                for (spelling, spell) in SPELLINGS {
                    let case = format!(
                        "{kind:?} moved on {writes} time(s), deleted under the version it left \
                         behind {writes_since} write(s) ago, spelled {spelling}"
                    );
                    let answer = delete(&router, kind, &held.id, &spell(stale)).await;
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

                    // The envelope a client parses: the fault, the code and the sentence under
                    // `Error`, with the request named beside it.
                    let root = answer.root();
                    assert_eq!(
                        root.text_at("Error/Type").unwrap_or_default(),
                        SENDER_FAULT,
                        "case {case}: expected the fault {SENDER_FAULT}, got {}",
                        answer.raw
                    );
                    assert_eq!(
                        root.text_at("Error/Message").unwrap_or_default(),
                        PRECONDITION_FAILED_MESSAGE,
                        "case {case}: expected the service's sentence for PreconditionFailed, \
                         got {}",
                        answer.raw
                    );
                    let request_id = root.child("RequestId").map(Node::text).unwrap_or_default();
                    assert!(
                        is_minted_uuid(request_id),
                        "case {case}: expected a RequestId beside Error, got {request_id:?} in {}",
                        answer.raw
                    );

                    // The refusal was made before anything was removed, and minted no version:
                    // the record is still readable at exactly the version it was at.
                    let read_back = read(&router, kind, &held.id).await;
                    assert_eq!(
                        read_back.status, 200,
                        "case {case}: expected the refused delete to leave the record readable, \
                         got {} with {}",
                        read_back.status, read_back.raw
                    );
                    assert_eq!(
                        read_back.header("etag"),
                        held.current,
                        "case {case}: expected the record to keep the version {:?}, got {:?}",
                        held.current,
                        read_back.header("etag")
                    );
                }
            }
        }
    }
}

/// RULE: because the refusal removes nothing, the sequence a client follows around a failed
/// precondition completes — the delete retried under the version the record's own read reports
/// is served HTTP 204 with an empty body, and only then does the id name no record.
#[tokio::test]
async fn the_delete_retried_under_the_re_read_version_is_served() {
    for kind in KINDS {
        for writes in WRITES {
            let router = emulator();
            let reference = format!("superseded-retry-{writes}");
            let held = stand_up(&router, kind, &reference, writes).await;
            let case = format!("{kind:?} moved on {writes} time(s) and then deleted");

            // The caller comes in with the version it read before the writes: refused, with the
            // record left in place.
            let stale = held
                .superseded
                .first()
                .expect("a case moves the record on at least once");
            let refused = delete(&router, kind, &held.id, stale).await;
            assert_eq!(
                refused.status, 412,
                "case {case}: expected HTTP 412 for the version the writes left behind, got {} \
                 with {}",
                refused.status, refused.raw
            );

            // It reads the record back and retries under the version that read reports.
            let read_back = read(&router, kind, &held.id).await;
            assert_eq!(
                read_back.status, 200,
                "case {case}: expected the record to survive the refusal, got {} with {}",
                read_back.status, read_back.raw
            );
            let reported = read_back.header("etag").to_string();
            assert_eq!(
                reported, held.current,
                "case {case}: expected the read to report the version the last write minted \
                 ({:?}), got {reported:?}",
                held.current
            );

            let served = delete(&router, kind, &held.id, &reported).await;
            assert_eq!(
                served.status, 204,
                "case {case}: expected HTTP 204 for the retry under the reported version, got {} \
                 with {}",
                served.status, served.raw
            );
            assert!(
                served.raw.trim().is_empty(),
                "case {case}: expected an empty body from the served delete, got {}",
                served.raw
            );

            // Gone means gone: the record the reads answered from is removed.
            let gone = read(&router, kind, &held.id).await;
            assert_eq!(
                gone.status, 404,
                "case {case}: expected the deleted record to be gone, got {} with {}",
                gone.status, gone.raw
            );
            assert_eq!(
                gone.error_code(),
                absent_code(kind),
                "case {case}: expected {}, got {}",
                absent_code(kind),
                gone.raw
            );

            // And the version the delete consumed satisfies nothing afterwards: the id names no
            // record, so the same call again is the absent record rather than a second removal.
            let again = delete(&router, kind, &held.id, &reported).await;
            assert_eq!(
                again.status, 404,
                "case {case}: expected HTTP 404 for the delete repeated after the removal, got \
                 {} with {}",
                again.status, again.raw
            );
        }
    }
}
