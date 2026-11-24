// SPDX-License-Identifier: CC0-1.0

//! What a LANDED `DeleteTrustStore` leaves behind, end to end over HTTP.
//!
//! The rule under test: the delete removes the one stored record every surface of this
//! service reads a trust store out of, so once it lands no surface reports that trust store
//! any more — the listing of trust stores does not list it, the listing of the distributions
//! it served does not resolve it under any identifier it used to answer to, the tagging
//! surface holds none of the tags it carried, and no later conditional write of it (a second
//! delete, an update) finds anything to write, not even with the very version token that
//! landed. Every OTHER stored trust store is untouched by it: same identity, same tags, same
//! version token, and the collection keeps its order.
//!
//! [`crate::trust_store_delete`] pins the delete's own refusal order and that the id it
//! addressed stops resolving through `GetTrustStore`. This suite pins the EFFECT ON THE STORE
//! across the other readers of that record, which is what makes the removal a write every
//! reader answers from rather than one read answering differently.
//!
//! The rows span what the rule generalises over: which of the stored trust stores goes (the
//! first, a middle one and the last of the collection, in the id order the listing reports),
//! how many certificates its bundle held, whether it carried tags and how many, and the ORDER
//! a whole collection is deleted in.
//!
//! The expected sentences are LITERALS here rather than the crate's own constants, so a
//! constant edited to some other wording fails these rows. The fixture is the one
//! [`crate::trust_store_lifecycle`] owns — CloudFront and Amazon S3 over one engine, with
//! real CA bundles staged through Amazon S3 — and the wire harness is
//! [`crate::distribution_lifecycle`]'s.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, Answer, Node, API_VERSION};
use crate::distribution_tags::{list_tags_for_resource, reported_tags, ReportedTags};
use crate::distributions_by_web_acl_id::encode_label;
use crate::key_value_store_lifecycle::member;
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, get_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

/// `EntityNotFound` for an identifier no trust store answers to.
const TRUST_STORE_NOT_FOUND: &str = "The specified trust store does not exist.";

/// `NoSuchResource` for an ARN the tagging surface holds no resource under: the sentence
/// states the condition and names no part of the request.
const NO_SUCH_RESOURCE: &str = "The specified resource doesn't exist. Update your request and \
     try again.";

/// The tags one fixture row's create carries.
type TagRow = &'static [(&'static str, &'static str)];

/// The trust stores every row of this suite stands up.
///
/// `(case, Name, certificates in the staged bundle, the tags the create carries)` — the rows
/// differ along how many certificates the bundle holds and how many tags the record carries,
/// so a row answering from another row's record, or a tag set read off the wrong one, is
/// visible.
const STORES: [(&str, &str, usize, TagRow); 3] = [
    ("one certificate, no tags", "edge-clients", 1, &[]),
    (
        "two certificates, one tag",
        "partner-cas",
        2,
        &[("stage", "prod")],
    ),
    (
        "three certificates, two tags",
        "fleet.trust-store_1",
        3,
        &[("stage", "beta"), ("owner", "edge-team")],
    ),
];

/// One trust store this fixture holds, as the create that persisted it reported it.
#[derive(Debug, Clone)]
struct Store {
    case: &'static str,
    name: &'static str,
    id: String,
    arn: String,
    etag: String,
    /// The tags the create carried, in the form the tagging surface reports them.
    tags: ReportedTags,
}

/// CloudFront and Amazon S3 over one engine, holding one trust store per row of [`STORES`],
/// each built from a bundle really staged through Amazon S3.
async fn fixture() -> (Emulator, Vec<Store>) {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let mut stores: Vec<Store> = Vec::new();
    for (case, name, certificates, tags) in STORES {
        let key = format!("{name}.pem");
        emulator.stage(&key, &ca_bundle(certificates)).await;
        let submitted: Vec<(String, String)> = tags
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let body = request_xml(name, BUNDLE_BUCKET, &key, REGION, None, &submitted);
        let answer = create_trust_store(&emulator.router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201 from CreateTrustStore, got {} with {}",
            answer.status, answer.raw
        );
        stores.push(Store {
            case,
            name,
            id: member(answer.root(), "Id", case, &answer.raw),
            arn: member(answer.root(), "Arn", case, &answer.raw),
            etag: answer.header("etag").to_string(),
            tags: tags
                .iter()
                .map(|(key, value)| (key.to_string(), Some(value.to_string())))
                .collect(),
        });
    }
    (emulator, stores)
}

// ---------------------------------------------------------------------------
// The surfaces that read a trust store
// ---------------------------------------------------------------------------

/// `POST /2020-05-31/trust-stores`, the whole collection in one page.
async fn list_trust_stores(router: &MultiServiceRouter) -> Answer {
    let body = format!(
        "<ListTrustStoresRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\"/>"
    );
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/trust-stores"),
        Some(&body),
        None,
    )
    .await
}

/// The `TrustStoreSummary` elements one listing answered with, in document order.
fn summaries(answer: &Answer) -> Vec<&Node> {
    match answer.root().at("TrustStoreList") {
        Some(wrapper) => wrapper.children_named("TrustStoreSummary"),
        None => Vec::new(),
    }
}

/// The ids one listing reported, in the order it reported them.
fn listed_ids(answer: &Answer) -> Vec<String> {
    summaries(answer)
        .iter()
        .map(|summary| summary.text_at("Id").unwrap_or_default().to_string())
        .collect()
}

/// The version token the listing reports for each id it listed.
fn listed_etags(answer: &Answer) -> BTreeMap<String, String> {
    summaries(answer)
        .iter()
        .map(|summary| {
            (
                summary.text_at("Id").unwrap_or_default().to_string(),
                summary.text_at("ETag").unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The ids of `stores` in the one order the listing reports them: id order, for a collection
/// keyed by id.
fn ids_in_order(stores: &[Store]) -> Vec<String> {
    let mut ids: Vec<String> = stores.iter().map(|store| store.id.clone()).collect();
    ids.sort();
    ids
}

/// `DELETE /2020-05-31/trust-store/{Id}` with the version token in the `If-Match` header.
async fn delete_trust_store(router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
    call(
        router,
        "DELETE",
        // The id travels as ONE path label, encoded the way an SDK client encodes one.
        &format!("/{API_VERSION}/trust-store/{}", encode_label(id)),
        None,
        Some(if_match),
    )
    .await
}

/// `PUT /2020-05-31/trust-store/{Id}`, naming the bundle staged at `key` as its payload.
///
/// The bundle source is the request's payload and a union, so a well-formed update states one
/// — what the row exercises is which RECORD the conditional write resolves, not what the
/// request document says, so the bundle it names is a perfectly readable one.
async fn update_trust_store(
    router: &MultiServiceRouter,
    id: &str,
    if_match: &str,
    key: &str,
) -> Answer {
    let body = format!(
        "<CaCertificatesBundleSource xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CaCertificatesBundleS3Location><Bucket>{BUNDLE_BUCKET}</Bucket><Key>{key}</Key>\
         <Region>{REGION}</Region></CaCertificatesBundleS3Location>\
         </CaCertificatesBundleSource>"
    );
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/trust-store/{}", encode_label(id)),
        Some(&body),
        Some(if_match),
    )
    .await
}

/// `GET /2020-05-31/distributionsByTrustStore?TrustStoreIdentifier=...`.
async fn list_by_trust_store(router: &MultiServiceRouter, identifier: &str) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/{API_VERSION}/distributionsByTrustStore?TrustStoreIdentifier={}",
            // The identifier travels as one query value, encoded the way an SDK client
            // encodes one.
            encode_label(identifier)
        ),
        None,
        None,
    )
    .await
}

/// Assert `answer` is the modelled `EntityNotFound` a trust store identifier that resolves
/// nothing is refused with.
fn assert_trust_store_not_found(answer: &Answer, case: &str, surface: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?} ({surface}): expected HTTP 404 after the delete, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "EntityNotFound",
        "case {case:?} ({surface}): expected EntityNotFound, got {}",
        answer.raw
    );
    let stated = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        stated, TRUST_STORE_NOT_FOUND,
        "case {case:?} ({surface}): expected the message {TRUST_STORE_NOT_FOUND:?}, got \
         {stated:?}"
    );
}

/// Assert every surface still reports `store` exactly as the create minted it — what a trust
/// store another one's delete must be untouched by.
async fn assert_untouched(router: &MultiServiceRouter, case: &str, store: &Store) {
    let read = get_trust_store(router, &store.id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?} ({}): another trust store's delete must leave this one readable, got {} \
         with {}",
        store.case, read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        store.etag,
        "case {case:?} ({}): another trust store's delete never advances this one's version \
         token, got {:?}",
        store.case,
        read.header("etag")
    );
    for (surface, identifier) in [
        ("by id", store.id.as_str()),
        ("by name", store.name),
        ("by ARN", store.arn.as_str()),
    ] {
        let listed = list_by_trust_store(router, identifier).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?} ({}, {surface}): the surviving trust store must still resolve, got \
             {} with {}",
            store.case, listed.status, listed.raw
        );
    }
    let tags = list_tags_for_resource(router, &store.arn).await;
    assert_eq!(
        tags.status, 200,
        "case {case:?} ({}): the surviving trust store must still carry its tags, got {} with \
         {}",
        store.case, tags.status, tags.raw
    );
    assert_eq!(
        reported_tags(tags.root()),
        store.tags,
        "case {case:?} ({}): expected the tags its create carried {:?}, got {}",
        store.case,
        store.tags,
        tags.raw
    );
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// RULE: a landed delete removes the one record every surface reads the trust store out of,
/// so afterwards the listing does not list it, the listing of the distributions it served
/// refuses every identifier it answered to, the tagging surface holds none of its tags, and a
/// second conditional write of it — another delete or an update, carrying the very token that
/// landed — finds nothing to write. The other trust stores are untouched: same identity, same
/// tags, same version token, same order.
#[tokio::test]
async fn a_landed_delete_erases_the_trust_store_from_every_surface_that_reads_one() {
    for index in 0..STORES.len() {
        let (emulator, stores) = fixture().await;
        let gone = stores[index].clone();
        let case = gone.case;
        let survivors: Vec<Store> = stores
            .iter()
            .enumerate()
            .filter(|(position, _)| *position != index)
            .map(|(_, store)| store.clone())
            .collect();

        // Before the delete, every trust store the fixture created is listed.
        let before = list_trust_stores(&emulator.router).await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: expected HTTP 200 from ListTrustStores, got {} with {}",
            before.status, before.raw
        );
        assert_eq!(
            listed_ids(&before),
            ids_in_order(&stores),
            "case {case:?}: expected every created trust store listed once, got {}",
            before.raw
        );

        let deleted = delete_trust_store(&emulator.router, &gone.id, &gone.etag).await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: expected HTTP 204 from DeleteTrustStore, got {} with {}",
            deleted.status, deleted.raw
        );

        // 1. The listing: the deleted record is gone from it, the survivors are all still
        //    there in the same order, each reporting the version token it already had.
        let after = list_trust_stores(&emulator.router).await;
        assert_eq!(
            after.status, 200,
            "case {case:?}: expected HTTP 200 from ListTrustStores, got {} with {}",
            after.status, after.raw
        );
        assert_eq!(
            listed_ids(&after),
            ids_in_order(&survivors),
            "case {case:?}: expected only the surviving trust stores, in id order, got {}",
            after.raw
        );
        let reported = listed_etags(&after);
        for store in &survivors {
            assert_eq!(
                reported.get(&store.id).map(String::as_str),
                Some(store.etag.as_str()),
                "case {case:?} ({}): expected the token the create minted, got {:?}",
                store.case,
                reported.get(&store.id)
            );
        }

        // 2. Every identifier the deleted trust store answered to resolves nothing, on the
        //    single read and on the listing of the distributions it served alike.
        for (surface, identifier) in [
            ("by id", gone.id.as_str()),
            ("by name", gone.name),
            ("by ARN", gone.arn.as_str()),
        ] {
            let read = get_trust_store(&emulator.router, identifier).await;
            assert_trust_store_not_found(&read, case, &format!("GetTrustStore {surface}"));
            let listed = list_by_trust_store(&emulator.router, identifier).await;
            assert_trust_store_not_found(
                &listed,
                case,
                &format!("ListDistributionsByTrustStore {surface}"),
            );
        }

        // 3. The tagging surface: the tags were held on the record, so the ARN now names no
        //    resource at all rather than one carrying an empty tag set.
        let tags = list_tags_for_resource(&emulator.router, &gone.arn).await;
        assert_eq!(
            tags.status, 404,
            "case {case:?}: expected HTTP 404 from the tag read of a deleted trust store, got \
             {} with {}",
            tags.status, tags.raw
        );
        assert_eq!(
            tags.error_code(),
            "NoSuchResource",
            "case {case:?}: expected NoSuchResource, got {}",
            tags.raw
        );
        let stated = tags.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            stated, NO_SUCH_RESOURCE,
            "case {case:?}: expected the message {NO_SUCH_RESOURCE:?}, got {stated:?}"
        );

        // 4. A second conditional write finds nothing to write, with the very token that
        //    landed: the record the precondition would be checked against is gone, so the
        //    answer is the absent entity rather than a failed precondition.
        let again = delete_trust_store(&emulator.router, &gone.id, &gone.etag).await;
        assert_trust_store_not_found(&again, case, "a second DeleteTrustStore");
        let staged = format!("{}.pem", gone.name);
        let updated =
            update_trust_store(&emulator.router, &gone.id, &gone.etag, &staged).await;
        assert_trust_store_not_found(&updated, case, "UpdateTrustStore");

        // 5. The trust stores the delete did not name are untouched by it.
        for store in &survivors {
            assert_untouched(&emulator.router, case, store).await;
        }
    }
}

/// RULE, over a whole collection: each delete removes exactly the one record it names, so
/// whatever ORDER a collection is deleted in, the listing shrinks by that one id each time,
/// the records still there keep their version tokens, and the account that holds no trust
/// store any more is served an empty listing — with every name free for a later create.
#[tokio::test]
async fn deleting_a_collection_removes_exactly_one_record_per_call_in_any_order() {
    // The rows differ in the ORDER the same collection is deleted in: the order it was
    // created in, the reverse of it, and one starting in the middle.
    let orders: [(&str, [usize; 3]); 3] = [
        ("in the order they were created", [0, 1, 2]),
        ("in the reverse of that order", [2, 1, 0]),
        ("starting in the middle", [1, 0, 2]),
    ];

    for (case, order) in orders {
        let (emulator, stores) = fixture().await;
        let mut left = stores.clone();

        for position in order {
            let gone = stores[position].clone();
            let deleted = delete_trust_store(&emulator.router, &gone.id, &gone.etag).await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?} ({}): expected HTTP 204 from DeleteTrustStore, got {} with {}",
                gone.case, deleted.status, deleted.raw
            );
            left.retain(|store| store.id != gone.id);

            let listed = list_trust_stores(&emulator.router).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?} ({}): expected HTTP 200 from ListTrustStores, got {} with {}",
                gone.case, listed.status, listed.raw
            );
            assert_eq!(
                listed_ids(&listed),
                ids_in_order(&left),
                "case {case:?} ({}): expected exactly the trust stores not yet deleted, got {}",
                gone.case,
                listed.raw
            );
            for store in &left {
                assert_untouched(&emulator.router, case, store).await;
            }
        }

        // Nothing is left: the listing is empty and reports no resume point.
        let empty = list_trust_stores(&emulator.router).await;
        assert_eq!(
            empty.status, 200,
            "case {case:?}: expected HTTP 200 from the listing of an empty collection, got {} \
             with {}",
            empty.status, empty.raw
        );
        assert!(
            listed_ids(&empty).is_empty(),
            "case {case:?}: expected an empty listing, got {}",
            empty.raw
        );
        assert!(
            empty.root().find_text("NextMarker").is_none(),
            "case {case:?}: an empty listing names no resume point, got {}",
            empty.raw
        );

        // Every name the deleted trust stores held is free again, and a re-create mints a new
        // identity rather than resurrecting the old one.
        for (row, store) in STORES.iter().zip(stores.iter()) {
            let (row_case, name, certificates, _) = row;
            let key = format!("recreated-{name}.pem");
            emulator.stage(&key, &ca_bundle(*certificates)).await;
            let body = request_xml(name, BUNDLE_BUCKET, &key, REGION, None, &[]);
            let created = create_trust_store(&emulator.router, &body).await;
            assert_eq!(
                created.status, 201,
                "case {case:?} ({row_case}): expected HTTP 201 re-creating {name:?}, got {} \
                 with {}",
                created.status, created.raw
            );
            let minted = member(created.root(), "Id", row_case, &created.raw);
            assert_ne!(
                minted, store.id,
                "case {case:?} ({row_case}): the re-create must mint a new id, got {minted:?}"
            );
        }
    }
}
