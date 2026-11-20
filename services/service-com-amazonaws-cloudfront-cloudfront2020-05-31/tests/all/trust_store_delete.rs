// SPDX-License-Identifier: CC0-1.0

//! `DeleteTrustStore`, end to end over HTTP.
//!
//! The rule under test: deleting a trust store is a CONDITIONAL write of the one record the
//! create persisted. The `Id` has to name a stored trust store (`EntityNotFound`, HTTP 404);
//! the `If-Match` the request carries has to be that record's current version token, where a
//! value that is no version token at all is `InvalidIfMatchVersion` (HTTP 400) and a
//! well-formed token that is not the current one is `PreconditionFailed` (HTTP 412); and a
//! trust store a stored distribution still validates viewer certificates against is
//! `CannotDeleteEntityWhileInUse` (HTTP 409). A REFUSED delete writes nothing — the trust
//! store still resolves, with the same identity and the same version token, so a caller that
//! re-reads the ETag and retries lands. With the current token the delete answers the modelled
//! HTTP 204 and an empty body, and afterwards NONE of the three identifiers a trust store
//! answers to resolves: the id, the name and the ARN are all `EntityNotFound`, and the name is
//! free for a later create.
//!
//! The rows span what the rule generalises over: the three HTTP spellings of one entity tag
//! (bare, strong-quoted and weak), every way a value can fail to be a version token (too
//! long, too short, the right width in the wrong alphabet, prose, the trust store's own ARN,
//! its id), a well-formed token nothing holds, ids of forms no trust store answers to, and a
//! trust store that is referenced versus one that is not — with the reference taken off again
//! so the same trust store then deletes.
//!
//! The expected sentences are LITERALS here rather than the crate's own constants, so a
//! constant edited to some other wording fails these rows. The fixture is the one
//! [`crate::trust_store_lifecycle`] owns — CloudFront and Amazon S3 over one engine, with a
//! real CA bundle staged through Amazon S3 — and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, update_distribution, Answer, API_VERSION,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, get_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

// The sentences this service answers these codes with, spelled out as literals.
/// `InvalidIfMatchVersion`: a property of the HEADER the request carried, so the sentence
/// names nothing about the request or the resource.
const INVALID_IF_MATCH_VERSION: &str =
    "The If-Match version is missing or not valid for the resource.";

/// `PreconditionFailed` on the surface a trust store belongs to — the resource DOCUMENTS a
/// caller conditionally rewrites, which is the family the key value stores, the functions and
/// the distributions themselves are worded with.
const PRECONDITION_FAILED: &str = "The request failed because it didn't meet the preconditions \
     in one or more request-header fields.";

/// `EntityNotFound` for an identifier no trust store answers to.
const TRUST_STORE_NOT_FOUND: &str = "The specified trust store does not exist.";

/// `CannotDeleteEntityWhileInUse`, as the model documents the shape.
const ENTITY_IN_USE: &str = "The entity cannot be deleted while it is in use.";

/// A version token that is well formed — `E` + 13 characters of the shape this service mints
/// — but that nothing in a fresh emulator holds, so a precondition carrying it is STALE
/// rather than unreadable.
const UNHELD_VERSION_TOKEN: &str = "E1ABCDEFGHIJKL";

/// One trust store this fixture holds.
#[derive(Debug, Clone)]
struct Store {
    name: String,
    id: String,
    arn: String,
    etag: String,
}

/// Create one trust store from a freshly staged CA bundle and answer the identity and version
/// token its create minted.
async fn trust_store(emulator: &Emulator, name: &str, certificates: usize) -> Store {
    let key = format!("{name}.pem");
    emulator.stage(&key, &ca_bundle(certificates)).await;
    let body = request_xml(name, BUNDLE_BUCKET, &key, REGION, None, &[]);
    let answer = create_trust_store(&emulator.router, &body).await;
    assert_eq!(
        answer.status, 201,
        "case {name}: expected HTTP 201 from CreateTrustStore, got {} with {}",
        answer.status, answer.raw
    );
    let created = answer.root();
    let id = created
        .text_at("Id")
        .unwrap_or_else(|| panic!("case {name}: the create carries an Id: {}", answer.raw))
        .to_string();
    let arn = created
        .text_at("Arn")
        .unwrap_or_else(|| panic!("case {name}: the create carries an Arn: {}", answer.raw))
        .to_string();
    Store {
        name: name.to_string(),
        id,
        arn,
        etag: answer.header("etag").to_string(),
    }
}

/// `DELETE /2020-05-31/trust-store/{Id}` with the version token in the `If-Match` header.
async fn delete_trust_store(
    router: &MultiServiceRouter,
    id: &str,
    if_match: Option<&str>,
) -> Answer {
    call(
        router,
        "DELETE",
        // The id travels as ONE path label, encoded the way an SDK client encodes one, so a
        // value carrying separators is addressed rather than reshaping the route.
        &format!("/{API_VERSION}/trust-store/{}", encode_label(id)),
        None,
        if_match,
    )
    .await
}

/// Assert `store` still resolves through every identifier it answers to, reporting the same id
/// and the same version token — what a REFUSED delete must leave behind.
async fn assert_still_there(router: &MultiServiceRouter, case: &str, store: &Store) {
    for (addressed, identifier) in [
        ("by id", store.id.as_str()),
        ("by name", store.name.as_str()),
        ("by ARN", store.arn.as_str()),
    ] {
        let read = get_trust_store(router, identifier).await;
        assert_eq!(
            read.status, 200,
            "case {case:?} ({addressed}): the refused delete must leave the trust store \
             readable, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.root().text_at("Id").unwrap_or_default(),
            store.id,
            "case {case:?} ({addressed}): expected the trust store {:?}, got {}",
            store.id,
            read.raw
        );
        assert_eq!(
            read.header("etag"),
            store.etag,
            "case {case:?} ({addressed}): a refusal never advances the version token, got {:?}",
            read.header("etag")
        );
    }
}

/// Assert no identifier of `store` resolves any more, each with the one sentence this service
/// states for a trust store that is not there.
async fn assert_gone(router: &MultiServiceRouter, case: &str, store: &Store) {
    for (addressed, identifier) in [
        ("by id", store.id.as_str()),
        ("by name", store.name.as_str()),
        ("by ARN", store.arn.as_str()),
    ] {
        let read = get_trust_store(router, identifier).await;
        assert_eq!(
            read.status, 404,
            "case {case:?} ({addressed}): expected HTTP 404 after the delete, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.error_code(),
            "EntityNotFound",
            "case {case:?} ({addressed}): expected EntityNotFound, got {}",
            read.raw
        );
        let stated = read.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, TRUST_STORE_NOT_FOUND,
            "case {case:?} ({addressed}): expected the message {TRUST_STORE_NOT_FOUND:?}, got \
             {stated:?}"
        );
    }
}

/// The `DistributionConfig` of a distribution whose viewer mTLS configuration validates
/// client certificates against `trust_store_id` when the row names one.
fn config_xml(caller_reference: &str, trust_store_id: Option<&str>) -> String {
    let viewer_mtls = trust_store_id
        .map(|id| {
            format!(
                "<ViewerMtlsConfig><Mode>required</Mode><TrustStoreConfig>\
                 <TrustStoreId>{id}</TrustStoreId>\
                 </TrustStoreConfig></ViewerMtlsConfig>"
            )
        })
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>a.example.com</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{caller_reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>{viewer_mtls}\
         </DistributionConfig>"
    )
}

/// RULE: with the trust store's CURRENT version token the delete answers the modelled HTTP 204
/// and an empty body, and afterwards none of the three identifiers resolves — whichever of
/// HTTP's three spellings of that one entity tag the caller sent. The name the deleted trust
/// store held is free again, which a second create under it proves.
#[tokio::test]
async fn the_current_version_token_deletes_the_trust_store_in_every_spelling() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;

    // The rows differ in the SPELLING of one value: bare as CloudFront's own `ETag` header
    // reports it, quoted as an HTTP entity tag, and quoted with the weak-comparison prefix.
    /// One row of the spelling table: the case it stands for, and how it writes a version
    /// token into the `If-Match` header.
    type Spelling = (&'static str, fn(&str) -> String);
    let spellings: [Spelling; 3] = [
        ("the bare version token", |etag| etag.to_string()),
        ("the version token quoted", |etag| format!("\"{etag}\"")),
        ("the version token as a weak tag", |etag| {
            format!("W/\"{etag}\"")
        }),
    ];

    for (index, (case, spell)) in spellings.into_iter().enumerate() {
        // A different certificate count per row, so a row answering from another row's record
        // would be visible.
        let store = trust_store(&emulator, &format!("deleted-{index}"), index + 1).await;

        let deleted = delete_trust_store(&emulator.router, &store.id, Some(&spell(&store.etag)))
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: expected HTTP 204 from DeleteTrustStore, got {} with {}",
            deleted.status, deleted.raw
        );
        assert!(
            deleted.raw.trim().is_empty(),
            "case {case:?}: expected an empty body, got {:?}",
            deleted.raw
        );

        assert_gone(&emulator.router, case, &store).await;

        // The name is free again: a second create under it is accepted rather than refused as
        // a name the account already holds.
        let again = trust_store(&emulator, &store.name, index + 1).await;
        assert_ne!(
            again.id, store.id,
            "case {case:?}: the re-create must mint a new id, got {:?}",
            again.id
        );
    }
}

/// RULE: an `If-Match` that is not the trust store's current version token refuses the delete
/// and writes NOTHING — a value that is no version token at all with `InvalidIfMatchVersion`
/// (400), a well-formed token that is not the current one with `PreconditionFailed` (412).
/// After every refusal the trust store still resolves, with the same id and the same version
/// token, and the delete then lands with the token a read reports.
#[tokio::test]
async fn an_if_match_that_is_not_the_current_version_refuses_the_delete() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let store = trust_store(&emulator, "guarded-trust-store", 2).await;

    // (case, the `If-Match` the row sends, status, code, message). Each value is derived from
    // the row rather than from the recording: what makes a row a 400 is its FORM, and what
    // makes one a 412 is that it is a token the record does not hold.
    let rows: [(&str, String, u16, &str, &str); 8] = [
        (
            "a well-formed token nothing holds",
            UNHELD_VERSION_TOKEN.to_string(),
            412,
            "PreconditionFailed",
            PRECONDITION_FAILED,
        ),
        (
            "that same token quoted",
            format!("\"{UNHELD_VERSION_TOKEN}\""),
            412,
            "PreconditionFailed",
            PRECONDITION_FAILED,
        ),
        (
            "a value wider than a version token",
            "E000000000FAKEBOGUS".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "a value narrower than a version token",
            "E1ABCDEF".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "the right width in the wrong alphabet",
            UNHELD_VERSION_TOKEN.to_lowercase(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "prose",
            "not a version at all".to_string(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "the trust store's own ARN",
            store.arn.clone(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
        (
            "the trust store's own id",
            store.id.clone(),
            400,
            "InvalidIfMatchVersion",
            INVALID_IF_MATCH_VERSION,
        ),
    ];

    for (case, if_match, status, code, message) in rows {
        let refused = delete_trust_store(&emulator.router, &store.id, Some(&if_match)).await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status} for If-Match {if_match:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected {code}, got {}",
            refused.raw
        );
        let stated = refused.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, message,
            "case {case:?}: expected the message {message:?}, got {stated:?}"
        );

        assert_still_there(&emulator.router, case, &store).await;
    }

    // The version token a read reports is the one that lands, after every one of those
    // refusals: nothing about the record moved.
    let read = get_trust_store(&emulator.router, &store.id).await;
    let current = read.header("etag").to_string();
    assert_eq!(
        current, store.etag,
        "the refusals must leave the create's version token current, got {current:?}"
    );
    let deleted = delete_trust_store(&emulator.router, &store.id, Some(&current)).await;
    assert_eq!(
        deleted.status, 204,
        "expected HTTP 204 with the current version token, got {} with {}",
        deleted.status, deleted.raw
    );
    assert_gone(&emulator.router, "after the refusals", &store).await;
}

/// RULE: the `Id` is resolved against the store BEFORE the precondition is evaluated, so an id
/// no trust store answers to is `EntityNotFound` (404) whatever the request's `If-Match`
/// carries — there is no stored version for a precondition to be checked against.
#[tokio::test]
async fn an_id_no_trust_store_answers_to_is_not_found() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    // One trust store that DOES exist, so a refusal below cannot be the store being empty.
    let present = trust_store(&emulator, "present-trust-store", 1).await;

    let absent: [(&str, String); 3] = [
        (
            "an id of the minted shape nothing holds",
            "ts_0123456789ABC".to_string(),
            ),
        ("a name never created", "absent-trust-store".to_string()),
        (
            "an id of no form this service mints",
            "1234".to_string(),
        ),
    ];

    // The `If-Match` forms a row sends: the current token of the trust store that does exist
    // (which is a version token, just not this id's), one that is no version token at all, and
    // a well-formed token nothing holds. Not one of them changes the answer.
    for (case, id) in absent {
        for (spelling, if_match) in [
            ("a token another record holds", present.etag.clone()),
            ("no version token at all", "bogus".to_string()),
            ("a token nothing holds", UNHELD_VERSION_TOKEN.to_string()),
        ] {
            let refused = delete_trust_store(&emulator.router, &id, Some(&if_match)).await;
            assert_eq!(
                refused.status, 404,
                "case {case:?} ({spelling}): expected HTTP 404 for {id:?}, got {} with {}",
                refused.status, refused.raw
            );
            assert_eq!(
                refused.error_code(),
                "EntityNotFound",
                "case {case:?} ({spelling}): expected EntityNotFound, got {}",
                refused.raw
            );
            let stated = refused.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                stated, TRUST_STORE_NOT_FOUND,
                "case {case:?} ({spelling}): expected the message {TRUST_STORE_NOT_FOUND:?}, got \
                 {stated:?}"
            );
        }
    }

    // The trust store that does exist is untouched by every one of those refusals.
    assert_still_there(&emulator.router, "the trust store that exists", &present).await;
}

/// RULE: a trust store a stored distribution still validates viewer certificates against
/// cannot be deleted (`CannotDeleteEntityWhileInUse`, 409) and stays readable; a trust store no
/// stored configuration names deletes. Taking the reference off the distribution's
/// configuration is what makes the same trust store deletable, so what the refusal reads is the
/// STORED configuration rather than a flag the association set.
#[tokio::test]
async fn a_trust_store_a_distribution_validates_against_cannot_be_deleted() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let referenced = trust_store(&emulator, "referenced-trust-store", 2).await;
    let unreferenced = trust_store(&emulator, "unreferenced-trust-store", 3).await;

    let created = create_distribution(
        &emulator.router,
        &config_xml("ref-mtls-distribution", Some(&referenced.id)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let distribution = created.root().text_at("Id").unwrap_or_default().to_string();
    let distribution_etag = created.header("etag").to_string();

    // The trust store the distribution names is refused, with the current version token: what
    // refuses it is the reference, not the precondition.
    let refused =
        delete_trust_store(&emulator.router, &referenced.id, Some(&referenced.etag)).await;
    assert_eq!(
        refused.status, 409,
        "expected HTTP 409 for a trust store in use, got {} with {}",
        refused.status, refused.raw
    );
    assert_eq!(
        refused.error_code(),
        "CannotDeleteEntityWhileInUse",
        "expected CannotDeleteEntityWhileInUse, got {}",
        refused.raw
    );
    let stated = refused.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        stated, ENTITY_IN_USE,
        "expected the message {ENTITY_IN_USE:?}, got {stated:?}"
    );
    assert_still_there(&emulator.router, "the referenced trust store", &referenced).await;

    // The trust store nothing names deletes under the very same conditions.
    let deleted =
        delete_trust_store(&emulator.router, &unreferenced.id, Some(&unreferenced.etag)).await;
    assert_eq!(
        deleted.status, 204,
        "expected HTTP 204 for a trust store nothing names, got {} with {}",
        deleted.status, deleted.raw
    );
    assert_gone(&emulator.router, "the unreferenced trust store", &unreferenced).await;

    // Take the reference off the stored configuration: the same trust store then deletes.
    let updated = update_distribution(
        &emulator.router,
        &distribution,
        &config_xml("ref-mtls-distribution", None),
        Some(&distribution_etag),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "expected HTTP 200 from UpdateDistribution, got {} with {}",
        updated.status, updated.raw
    );

    let deleted =
        delete_trust_store(&emulator.router, &referenced.id, Some(&referenced.etag)).await;
    assert_eq!(
        deleted.status, 204,
        "expected HTTP 204 once nothing names the trust store, got {} with {}",
        deleted.status, deleted.raw
    );
    assert_gone(&emulator.router, "the formerly referenced trust store", &referenced).await;
}
