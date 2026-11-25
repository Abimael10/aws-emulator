// SPDX-License-Identifier: CC0-1.0

//! The ORDER the trust store's conditional writes judge a request in, end to end over HTTP.
//!
//! RULE: `InvalidIfMatchVersion` is "the If-Match version is missing or not valid" — a
//! property of the `If-Match` header the request carried and of nothing else — so a trust
//! store's conditional writes answer it (HTTP 400, the service's one fixed sentence) BEFORE
//! they resolve the path `Id` against stored trust stores: a header that carries no version
//! token at all is refused identically whether the id names a stored trust store or nothing
//! at all. Only a header that DOES carry a version token reaches the record, and then an id
//! nothing answers to is `EntityNotFound` (404) and a token that is not that record's current
//! one is a failed precondition (412).
//!
//! The rows span what the rule generalises over: every way a value can fail to be a version
//! token (wider than one, narrower, the right width in the wrong alphabet, prose, a UUID, the
//! trust store's own id, its ARN, a malformed value spelled as a quoted entity tag), crossed
//! with the ids a request can carry (the live trust store's own id, an id of the minted shape
//! nothing holds, a name never created, a value of no form this service mints), crossed with
//! the two operations of this resource that take an `If-Match` — `DeleteTrustStore` and
//! `UpdateTrustStore`. The 412 rows assert only the status and the code, since the wording of
//! that refusal is the business of the suites that own it.
//!
//! The expected sentence is a LITERAL here rather than the crate's own constant, so a
//! constant edited to some other wording fails these rows. The fixture is the one
//! [`crate::trust_store_lifecycle`] owns — CloudFront and Amazon S3 over one engine, with a
//! real CA bundle staged through Amazon S3 — and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, Answer, API_VERSION};
use crate::distributions_by_web_acl_id::encode_label;
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, get_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

/// The sentence this service answers `InvalidIfMatchVersion` with: fixed text that names
/// nothing about the request or the resource.
const INVALID_IF_MATCH_VERSION: &str =
    "The If-Match version is missing or not valid for the resource.";

/// `EntityNotFound` for an id no trust store answers to.
const TRUST_STORE_NOT_FOUND: &str = "The specified trust store does not exist.";

/// A version token that is well formed — `E` + 13 characters of the shape this service mints
/// — but that nothing in a fresh emulator holds, so a precondition carrying it is READABLE
/// and simply not current.
const UNHELD_VERSION_TOKEN: &str = "E1ABCDEFGHIJKL";

/// The object key the update rows name, staged with a readable bundle so the only thing at
/// fault in a refusal below is the header or the id.
const UPDATE_BUNDLE_KEY: &str = "if-match-form.pem";

/// One conditional operation of this resource, as a request this suite can send.
#[derive(Debug, Clone, Copy)]
enum Conditional {
    /// `DELETE /2020-05-31/trust-store/{Id}`.
    Delete,
    /// `PUT /2020-05-31/trust-store/{Id}`.
    Update,
}

impl Conditional {
    /// The name a case reports this operation under.
    fn case(self) -> &'static str {
        match self {
            Conditional::Delete => "DeleteTrustStore",
            Conditional::Update => "UpdateTrustStore",
        }
    }

    /// Send the request, addressing `id` under the `If-Match` header `if_match`.
    async fn send(self, router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
        // The id travels as ONE path label, encoded the way an SDK client encodes one.
        let path = format!("/{API_VERSION}/trust-store/{}", encode_label(id));
        match self {
            Conditional::Delete => call(router, "DELETE", &path, None, Some(if_match)).await,
            Conditional::Update => {
                let body = format!(
                    "<CaCertificatesBundleSource \
                     xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                     <CaCertificatesBundleS3Location><Bucket>{BUNDLE_BUCKET}</Bucket>\
                     <Key>{UPDATE_BUNDLE_KEY}</Key><Region>{REGION}</Region>\
                     </CaCertificatesBundleS3Location></CaCertificatesBundleSource>"
                );
                call(router, "PUT", &path, Some(&body), Some(if_match)).await
            }
        }
    }
}

/// The two operations every table below runs over.
const CONDITIONALS: [Conditional; 2] = [Conditional::Delete, Conditional::Update];

/// One trust store this fixture holds.
struct Store {
    name: String,
    id: String,
    arn: String,
    etag: String,
}

/// Create one trust store from a freshly staged CA bundle and report the identity and version
/// token its create minted.
async fn trust_store(emulator: &Emulator, name: &str, certificates: usize) -> Store {
    let key = format!("{name}.pem");
    emulator.stage(&key, &ca_bundle(certificates)).await;
    let answer = create_trust_store(
        &emulator.router,
        &request_xml(name, BUNDLE_BUCKET, &key, REGION, None, &[]),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case {name}: expected HTTP 201 from CreateTrustStore, got {} with {}",
        answer.status, answer.raw
    );
    let created = answer.root();
    Store {
        name: name.to_string(),
        id: created
            .text_at("Id")
            .unwrap_or_else(|| panic!("case {name}: the create carries an Id: {}", answer.raw))
            .to_string(),
        arn: created
            .text_at("Arn")
            .unwrap_or_else(|| panic!("case {name}: the create carries an Arn: {}", answer.raw))
            .to_string(),
        etag: answer.header("etag").to_string(),
    }
}

/// Every way a value a request puts in `If-Match` can fail to be a version token this service
/// mints: `(case, value)`.
///
/// `If-Match` is a plain `string` in the model — no pattern and no length — so what makes a
/// value unreadable is the shape of the tokens this service hands out (`E` plus 13 characters
/// of the entity-identifier alphabet, the shape `ETag` reports); each row misses it in one
/// way, and the last row spells a miss as the quoted entity tag HTTP writes one as. The
/// resource's own identifiers are included because they are values a confused client really
/// sends, and a trust store id carries a namespace prefix rather than that shape.
fn no_version_token(store: &Store) -> Vec<(String, String)> {
    vec![
        (
            "a value wider than a version token".to_string(),
            "E000000000FAKEBOGUS".to_string(),
        ),
        (
            "a value narrower than a version token".to_string(),
            "E1ABCDEF".to_string(),
        ),
        (
            "the right width in the wrong alphabet".to_string(),
            UNHELD_VERSION_TOKEN.to_lowercase(),
        ),
        ("prose".to_string(), "not a version at all".to_string()),
        (
            "a UUID, the shape of another resource kind's id".to_string(),
            "f9a6a0b4-8d39-4a1e-9f67-1b0f7f5d2c3e".to_string(),
        ),
        ("the trust store's own id".to_string(), store.id.clone()),
        ("the trust store's own ARN".to_string(), store.arn.clone()),
        (
            "an unreadable value quoted as an entity tag".to_string(),
            "\"not a version at all\"".to_string(),
        ),
    ]
}

/// The ids a row addresses: the live trust store's own id, and the forms an id can take that
/// name no stored trust store — `(case, id)`.
fn addressed(store: &Store) -> Vec<(String, String)> {
    vec![
        ("the live trust store's own id".to_string(), store.id.clone()),
        (
            "an id of the minted shape nothing holds".to_string(),
            "ts_0123456789ABC".to_string(),
        ),
        (
            "a name never created".to_string(),
            "absent-trust-store".to_string(),
        ),
        (
            "an id of no form this service mints".to_string(),
            "1234".to_string(),
        ),
    ]
}

/// Assert the live trust store still reads back under the version token its create minted —
/// what every refusal above must have left behind.
async fn assert_untouched(router: &MultiServiceRouter, case: &str, store: &Store) {
    let read = get_trust_store(router, &store.id).await;
    assert_eq!(
        read.status, 200,
        "case {case}: the refusals must leave the trust store readable, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        store.etag,
        "case {case}: a refusal never advances the version token, got {:?}",
        read.header("etag")
    );
}

/// RULE: a header carrying no version token at all is `InvalidIfMatchVersion` (400) with the
/// service's fixed sentence, for every id a request can address — the live trust store's own
/// id included — so the refusal is decided before the id is resolved.
#[tokio::test]
async fn a_header_that_carries_no_version_token_is_refused_before_the_id_is_resolved() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let store = trust_store(&emulator, "if-match-form-trust-store", 2).await;
    // A bundle that READS, so an update row's refusal is never about the bundle.
    emulator.stage(UPDATE_BUNDLE_KEY, &ca_bundle(3)).await;

    for operation in CONDITIONALS {
        for (header_case, if_match) in no_version_token(&store) {
            for (id_case, id) in addressed(&store) {
                let case = format!("{} / {header_case} / {id_case}", operation.case());
                let refused = operation.send(&emulator.router, &id, &if_match).await;
                assert_eq!(
                    refused.status, 400,
                    "case {case}: expected HTTP 400 for If-Match {if_match:?}, got {} with {}",
                    refused.status, refused.raw
                );
                assert_eq!(
                    refused.error_code(),
                    "InvalidIfMatchVersion",
                    "case {case}: expected InvalidIfMatchVersion, got {}",
                    refused.raw
                );
                let stated = refused.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    stated, INVALID_IF_MATCH_VERSION,
                    "case {case}: expected the message {INVALID_IF_MATCH_VERSION:?}, got \
                     {stated:?}"
                );
            }
        }
    }

    assert_untouched(&emulator.router, "after the header refusals", &store).await;
}

/// RULE: a header that DOES carry a version token is judged against the record, so the same
/// request that was a 400 above is `EntityNotFound` (404) for an id nothing answers to and a
/// failed precondition (412) for the live trust store at a token it is not at — the ordering
/// the rule above is one half of.
#[tokio::test]
async fn a_readable_token_reaches_the_record_the_id_names() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let store = trust_store(&emulator, "readable-token-trust-store", 1).await;
    emulator.stage(UPDATE_BUNDLE_KEY, &ca_bundle(2)).await;

    // The readable tokens a row sends: one no record holds, and one another record does hold
    // (this store's own, which is a version token for the live id and not for any other).
    let readable: [(&str, String); 3] = [
        ("a token nothing holds", UNHELD_VERSION_TOKEN.to_string()),
        (
            "that same token quoted",
            format!("\"{UNHELD_VERSION_TOKEN}\""),
        ),
        ("the live trust store's current token", store.etag.clone()),
    ];

    for operation in CONDITIONALS {
        for (header_case, if_match) in &readable {
            for (id_case, id) in addressed(&store) {
                let case = format!("{} / {header_case} / {id_case}", operation.case());
                // The live id at its current token is the request this operation SERVES, and
                // the suites that own each operation assert what it then does; here it is
                // only kept out of the refusal rows.
                let live = id == store.id;
                if live && if_match == &store.etag {
                    continue;
                }
                let refused = operation.send(&emulator.router, &id, if_match).await;
                let (status, code) = if live {
                    (412, "PreconditionFailed")
                } else {
                    (404, "EntityNotFound")
                };
                assert_eq!(
                    refused.status, status,
                    "case {case}: expected HTTP {status} for If-Match {if_match:?}, got {} with \
                     {}",
                    refused.status, refused.raw
                );
                assert_eq!(
                    refused.error_code(),
                    code,
                    "case {case}: expected {code}, got {}",
                    refused.raw
                );
                if !live {
                    let stated = refused.root().find_text("Message").unwrap_or_default();
                    assert_eq!(
                        stated, TRUST_STORE_NOT_FOUND,
                        "case {case}: expected the message {TRUST_STORE_NOT_FOUND:?}, got \
                         {stated:?}"
                    );
                }
            }
        }
    }

    assert_untouched(&emulator.router, "after the record refusals", &store).await;
}

/// RULE: the ordering refuses nothing the service serves — the live trust store at its current
/// version token still updates and then deletes, after every refusal above.
#[tokio::test]
async fn the_current_version_token_still_serves_both_operations() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let mut store = trust_store(&emulator, "served-trust-store", 1).await;
    emulator.stage(UPDATE_BUNDLE_KEY, &ca_bundle(4)).await;

    // One unreadable header first, so the served calls below follow a refusal.
    let refused = Conditional::Delete
        .send(&emulator.router, &store.id, "not a version at all")
        .await;
    assert_eq!(
        refused.status, 400,
        "case the refusal before the served calls: expected HTTP 400, got {} with {}",
        refused.status, refused.raw
    );

    let updated = Conditional::Update
        .send(&emulator.router, &store.id, &store.etag)
        .await;
    assert_eq!(
        updated.status, 200,
        "case the update at the current version: expected HTTP 200, got {} with {}",
        updated.status, updated.raw
    );
    store.etag = updated.header("etag").to_string();
    assert_ne!(
        store.etag, "",
        "case the update at the current version: the update reports a version token, got {}",
        updated.raw
    );

    let deleted = Conditional::Delete
        .send(&emulator.router, &store.id, &store.etag)
        .await;
    assert_eq!(
        deleted.status, 204,
        "case the delete at the version the update minted: expected HTTP 204, got {} with {}",
        deleted.status, deleted.raw
    );

    let read = get_trust_store(&emulator.router, &store.name).await;
    assert_eq!(
        read.status, 404,
        "case after the delete: expected HTTP 404 for {:?}, got {} with {}",
        store.name, read.status, read.raw
    );
}
