// SPDX-License-Identifier: CC0-1.0

//! `ListTrustStores`, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The fixture is the one [`crate::trust_store_lifecycle`] owns (CloudFront
//! and Amazon S3 over one engine, with real CA bundles staged through Amazon S3), and the
//! wire harness is [`crate::distribution_lifecycle`]'s.
//!
//! The rules under test:
//!
//! 1. the listing reports every PERSISTED trust store as a summary read from the one record
//!    the create persisted — its identity, the certificate count read out of its bundle, its
//!    status at this instant and its version token — so a listing and a `GetTrustStore` can
//!    never disagree about one trust store;
//! 2. the collection is served in ONE stable total order, cut to at most `MaxItems` entries
//!    beginning after the `Marker` id, with a `NextMarker` exactly when trust stores remain
//!    — so walking the pages at ANY admissible page size enumerates the collection exactly
//!    once;
//! 3. a `Marker` this service never handed out is no resume point (`InvalidArgument`), and a
//!    well-formed one naming no stored trust store is an entity that does not exist
//!    (`EntityNotFound`);
//! 4. a `MaxItems` no page size can be taken from is refused (`InvalidArgument`);
//! 5. the listing is a REPORT: it is unchanged by being read, and an account holding no
//!    trust store is served an empty listing rather than a refusal.
//!
//! The rows differ along what these rules generalise over: how many certificates each
//! bundle holds, the shape of the object key, whether the create names the OCSP member and
//! which value it names, every page size from one up to past the collection's size plus
//! both ends of the admissible `MaxItems` range and one value past each, and every way a
//! marker can fail to be a resume point this service handed out.
//!
//! The expected sentences are LITERALS here rather than the crate's own constants, so a
//! constant edited to some other wording fails these rows.

use std::collections::BTreeSet;
use std::time::Instant;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_ITEMS, TRUST_STORE_ID_PREFIX, TRUST_STORE_STATUS_ACTIVE, TRUST_STORE_STATUS_PENDING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, Answer, Node, ACCOUNT_ID, API_VERSION, POLL_INTERVAL, READY_DEADLINE,
};
use crate::key_value_store_lifecycle::{is_version_token, member};
use crate::response_headers_policy_lifecycle::assert_absent;
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, get_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

/// `InvalidArgument` for a `MaxItems` no page size can be taken from: a property of the
/// parameter, so the sentence names nothing from the request.
const MAX_ITEMS_INVALID: &str = "MaxItems must be a non-zero positive integer.";

/// `InvalidArgument` for a `Marker` that is no trust store id this service handed out: the
/// constraint on the parameter and the kind of identifier it names, and nothing else.
const MARKER_INVALID: &str = "Marker must be a valid CF trust store Id";

/// `EntityNotFound` for an identifier no trust store answers to — a marker included.
const TRUST_STORE_NOT_FOUND: &str = "The specified trust store does not exist.";

// ---------------------------------------------------------------------------
// Calling the listing
// ---------------------------------------------------------------------------

/// `POST /2020-05-31/trust-stores` with the request document `body`.
async fn list_trust_stores(router: &MultiServiceRouter, body: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/trust-stores"),
        Some(body),
        None,
    )
    .await
}

/// A `ListTrustStoresRequest` document naming whichever of the two input members the case
/// under test names.
fn list_body(max_items: Option<i32>, marker: Option<&str>) -> String {
    let mut out = format!(
        "<ListTrustStoresRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">"
    );
    if let Some(marker) = marker {
        out.push_str(&format!("<Marker>{marker}</Marker>"));
    }
    if let Some(max_items) = max_items {
        out.push_str(&format!("<MaxItems>{max_items}</MaxItems>"));
    }
    out.push_str("</ListTrustStoresRequest>");
    out
}

/// The `TrustStoreSummary` elements one listing answered with.
///
/// `TrustStoreList`'s member carries `xmlName` `TrustStoreSummary`
/// (`smithy_query(query="shape",shape="TrustStoreList")`), so each entry travels in an
/// element of that name inside the `TrustStoreList` wrapper.
fn summaries(answer: &Answer) -> Vec<&Node> {
    match answer.root().at("TrustStoreList") {
        Some(wrapper) => wrapper.children_named("TrustStoreSummary"),
        None => Vec::new(),
    }
}

/// The ids one listing reported, in the order it reported them.
fn summary_ids(answer: &Answer) -> Vec<String> {
    summaries(answer)
        .iter()
        .map(|summary| {
            summary
                .text_at("Id")
                .expect("every summary carries an Id")
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The fixture: the trust stores this suite lists
// ---------------------------------------------------------------------------

/// One trust store this fixture holds, as the create that persisted it reported it.
#[derive(Debug, Clone)]
struct Store {
    case: &'static str,
    name: &'static str,
    certificates: usize,
    id: String,
    arn: String,
    etag: String,
}

/// The creates the fixture stands up: `(case, Name, object key, certificates in the bundle,
/// UseClientCertificateOCSPEndpoint or None to leave the member out)`.
///
/// The rows differ along how many certificates the bundle holds, the shape of the object
/// key, and whether the create names the optional OCSP member and which value it names.
const CREATES: [(&str, &str, &str, usize, Option<bool>); 5] = [
    ("one certificate, no OCSP member", "edge-clients", "single.pem", 1, None),
    (
        "two certificates, OCSP off, a prefixed key",
        "partner-cas",
        "bundles/2026/partner-cas.pem",
        2,
        Some(false),
    ),
    (
        "three certificates, OCSP on, a key carrying a space",
        "fleet.trust-store_1",
        "fleet bundle.pem",
        3,
        Some(true),
    ),
    ("one certificate, OCSP on", "second-party", "second.pem", 1, Some(true)),
    ("four certificates, no OCSP member", "zz-last", "zz.pem", 4, None),
];

/// CloudFront and Amazon S3 over one engine, holding one trust store per row of
/// [`CREATES`], each built from a bundle really staged through Amazon S3.
async fn fixture() -> (Emulator, Vec<Store>) {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    let mut stores: Vec<Store> = Vec::new();
    for (case, name, key, certificates, ocsp) in CREATES {
        emulator.stage(key, &ca_bundle(certificates)).await;
        let body = request_xml(name, BUNDLE_BUCKET, key, REGION, ocsp, &[]);
        let answer = create_trust_store(&emulator.router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201 from CreateTrustStore, got {} with {}",
            answer.status, answer.raw
        );
        let created = answer.root();
        stores.push(Store {
            case,
            name,
            certificates,
            id: member(created, "Id", case, &answer.raw),
            arn: member(created, "Arn", case, &answer.raw),
            etag: answer.header("etag").to_string(),
        });
    }
    (emulator, stores)
}

/// The fixture's ids in the order the listing reports them: one stable total order, which
/// for a store keyed by id is id order.
fn ids_in_order(stores: &[Store]) -> Vec<String> {
    let mut ids: Vec<String> = stores.iter().map(|store| store.id.clone()).collect();
    ids.sort();
    ids
}

/// Look again until no trust store the listing reports is still [`TRUST_STORE_STATUS_PENDING`]
/// — the transition is the one the CLOCK owes the records, and the read is what materialises
/// it — or [`READY_DEADLINE`] passes, which fails the case.
async fn awaited_active(router: &MultiServiceRouter, case: &str) -> Answer {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let answer = list_trust_stores(router, &list_body(None, None)).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from ListTrustStores, got {} with {}",
            answer.status, answer.raw
        );
        let still_pending: Vec<String> = summaries(&answer)
            .iter()
            .filter(|summary| summary.text_at("Status") != Some(TRUST_STORE_STATUS_ACTIVE))
            .map(|summary| summary.text_at("Id").unwrap_or_default().to_string())
            .collect();
        if still_pending.is_empty() {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: {still_pending:?} never reached {TRUST_STORE_STATUS_ACTIVE}, last \
             answer {}",
            answer.raw
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 1: every summary is the record the single read reports
// ---------------------------------------------------------------------------

/// The listing reports every persisted trust store, and each summary carries the record
/// `GetTrustStore` answers from: the same identity, the same certificate count, the same
/// status and the same version token — with `Reason` absent, which no stored trust store has
/// one of.
#[tokio::test]
async fn every_persisted_trust_store_is_listed_as_the_record_its_get_reports() {
    let (emulator, stores) = fixture().await;
    let listed = awaited_active(&emulator.router, "the whole collection").await;

    assert_eq!(
        summary_ids(&listed),
        ids_in_order(&stores),
        "case the whole collection: expected every created trust store exactly once in id \
         order, got {}",
        listed.raw
    );
    assert_absent(
        listed.root(),
        "NextMarker",
        "the whole collection",
        "the last page",
    );

    for store in &stores {
        let case = store.case;
        let summary = summaries(&listed)
            .into_iter()
            .find(|summary| summary.text_at("Id") == Some(store.id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports {}: {}",
                    store.id, listed.raw
                )
            });

        let read = get_trust_store(&emulator.router, &store.id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetTrustStore, got {} with {}",
            read.status, read.raw
        );
        let single = read.root();

        for field in ["Id", "Arn", "Name", "Status", "NumberOfCaCertificates"] {
            let listed_value = member(summary, field, case, &listed.raw);
            let single_value = member(single, field, case, &read.raw);
            assert_eq!(
                listed_value, single_value,
                "case {case}: the listing and the read must agree about {field}, got \
                 {listed_value:?} and {single_value:?}"
            );
        }
        assert_eq!(
            member(summary, "Name", case, &listed.raw),
            store.name,
            "case {case}: expected the submitted name, got {}",
            listed.raw
        );
        assert_eq!(
            member(summary, "Arn", case, &listed.raw),
            store.arn,
            "case {case}: expected the ARN the create minted, got {}",
            listed.raw
        );
        assert_eq!(
            member(summary, "NumberOfCaCertificates", case, &listed.raw),
            store.certificates.to_string(),
            "case {case}: expected the certificates the staged bundle holds, got {}",
            listed.raw
        );
        assert_eq!(
            member(summary, "Status", case, &listed.raw),
            TRUST_STORE_STATUS_ACTIVE,
            "case {case}: expected the status the clock owes a propagated trust store, got {}",
            listed.raw
        );
        // The summary's own `ETag` is "the version identifier for the current version of the
        // trust store": the token the create handed back, which is the one the single read
        // answers with too.
        let listed_etag = member(summary, "ETag", case, &listed.raw);
        assert!(
            is_version_token(&listed_etag),
            "case {case}: expected a version token, got {listed_etag:?}"
        );
        assert_eq!(
            listed_etag, store.etag,
            "case {case}: expected the token the create minted, got {listed_etag:?}"
        );
        assert_eq!(
            listed_etag,
            read.header("etag"),
            "case {case}: the listing and the read must agree about the version token, got \
             {listed_etag:?} and {:?}",
            read.header("etag")
        );
        assert_eq!(
            member(summary, "LastModifiedTime", case, &listed.raw),
            member(single, "LastModifiedTime", case, &read.raw),
            "case {case}: the listing and the read must agree about LastModifiedTime, got {}",
            listed.raw
        );
        assert_absent(summary, "Reason", case, "a trust store built from a read bundle");
    }

    // Rule 5: reading the listing changes nothing a later caller sees.
    let again = list_trust_stores(&emulator.router, &list_body(None, None)).await;
    assert_eq!(
        summary_ids(&again),
        ids_in_order(&stores),
        "case a second listing: a read changes nothing, got {}",
        again.raw
    );
}

/// A trust store CloudFront has just accepted is reported `pending` by the listing too, and
/// the CLOCK — not a later call — owes it `active`: the listing reports the status the record
/// has at the instant it is read, never one frozen at creation.
#[tokio::test]
async fn the_listing_reports_the_status_the_clock_owes_each_trust_store() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;
    emulator.stage("just-created.pem", &ca_bundle(2)).await;
    let body = request_xml(
        "just-created",
        BUNDLE_BUCKET,
        "just-created.pem",
        REGION,
        None,
        &[],
    );
    let created = create_trust_store(&emulator.router, &body).await;
    assert_eq!(
        created.status, 201,
        "case a just-created trust store: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );

    let immediate = list_trust_stores(&emulator.router, &list_body(None, None)).await;
    assert_eq!(
        summaries(&immediate)
            .first()
            .map(|summary| member(summary, "Status", "a just-created trust store", &immediate.raw)),
        Some(TRUST_STORE_STATUS_PENDING.to_string()),
        "case a just-created trust store: expected the transient status, got {}",
        immediate.raw
    );

    let settled = awaited_active(&emulator.router, "a propagated trust store").await;
    assert_eq!(
        summaries(&settled).len(),
        1,
        "case a propagated trust store: expected the one trust store, got {}",
        settled.raw
    );
}

// ---------------------------------------------------------------------------
// Rule 2: the page walk
// ---------------------------------------------------------------------------

/// Walk every page of the listing at `page_size` and answer the ids in the order they were
/// reported.
async fn walk_pages(router: &MultiServiceRouter, page_size: i32) -> Vec<String> {
    let case = format!("page size {page_size}");
    let mut ids: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..64 {
        let answer = list_trust_stores(router, &list_body(Some(page_size), marker.as_deref())).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let page = summary_ids(&answer);
        assert!(
            page.len() <= page_size as usize,
            "case {case}: a page holds at most {page_size} trust stores, got {page:?}"
        );
        ids.extend(page.iter().cloned());
        match answer.root().text_at("NextMarker") {
            Some(next) => {
                assert_eq!(
                    Some(next.to_string()),
                    page.last().cloned(),
                    "case {case}: the NextMarker is the id the page ended at, got {}",
                    answer.raw
                );
                marker = Some(next.to_string());
            }
            // No `NextMarker`: this was the last page.
            None => return ids,
        }
    }
    panic!("case {case}: the page walk did not terminate");
}

/// The listing cuts a page at `MaxItems`, resumes after the `Marker`, and hands back a
/// `NextMarker` exactly when trust stores remain — so walking the pages at ANY admissible
/// page size enumerates the whole collection exactly once, in one stable order.
#[tokio::test]
async fn the_listing_pages_at_max_items_and_resumes_from_the_marker() {
    let (emulator, stores) = fixture().await;
    let all = ids_in_order(&stores);

    // One row per page size from the smallest admissible one to one past the collection's
    // size, plus both the largest size this service serves and one past it.
    for page_size in [1, 2, 3, 4, 5, 6, MAX_ITEMS, MAX_ITEMS + 1] {
        let walked = walk_pages(&emulator.router, page_size).await;
        assert_eq!(
            walked, all,
            "case page size {page_size}: expected every trust store exactly once in id order, \
             got {walked:?}"
        );
    }

    // A listing that names no `MaxItems` at all serves the whole collection: it is smaller
    // than this service's own page ceiling.
    let answer = list_trust_stores(&emulator.router, &list_body(None, None)).await;
    assert_eq!(
        summary_ids(&answer),
        all,
        "case no MaxItems: expected the whole collection, got {}",
        answer.raw
    );
    assert_absent(answer.root(), "NextMarker", "no MaxItems", "the last page");

    // Each id resumes the listing AFTER the trust store it names, so the page a marker opens
    // is exactly the tail of the order.
    for (index, marker) in all.iter().enumerate() {
        let answer = list_trust_stores(&emulator.router, &list_body(None, Some(marker))).await;
        assert_eq!(
            answer.status, 200,
            "case the marker {marker}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            summary_ids(&answer),
            all[index + 1..].to_vec(),
            "case the marker {marker}: expected the tail after it, got {}",
            answer.raw
        );
    }
}

// ---------------------------------------------------------------------------
// Rules 3 and 4: what a listing refuses
// ---------------------------------------------------------------------------

/// A `MaxItems` that names no page is refused, a `Marker` this service never handed out is
/// no resume point, and a well-formed marker naming no stored trust store is an entity that
/// does not exist — and none of the refusals changes what the listing reports.
#[tokio::test]
async fn a_listing_that_names_no_page_or_no_resume_point_is_refused() {
    let (emulator, stores) = fixture().await;
    let all = ids_in_order(&stores);

    // Below the admissible `MaxItems` range: the member bounds the page, so a negative count
    // bounds nothing and names no page at all.
    for (case, max_items) in [("negative", -1), ("far negative", i32::MIN)] {
        let answer = list_trust_stores(&emulator.router, &list_body(Some(max_items), None)).await;
        assert_eq!(
            answer.status, 400,
            "case MaxItems {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case MaxItems {case}: expected InvalidArgument, got {}",
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(MAX_ITEMS_INVALID),
            "case MaxItems {case}: expected the sentence {MAX_ITEMS_INVALID:?}, got {}",
            answer.raw
        );
    }

    // A marker that is no trust store id at all was never a resume point this service handed
    // out: every way a value can fail to be one.
    let absent = format!("{TRUST_STORE_ID_PREFIX}{}", "Z".repeat(13));
    for (case, marker) in [
        ("a word", "next-please".to_string()),
        ("a distribution id", "E1234567890ABC".to_string()),
        ("the prefix alone", TRUST_STORE_ID_PREFIX.to_string()),
        ("a short body", format!("{TRUST_STORE_ID_PREFIX}ABC")),
        (
            "a long body",
            format!("{TRUST_STORE_ID_PREFIX}{}", "A".repeat(14)),
        ),
        (
            "a body outside the id alphabet",
            format!("{TRUST_STORE_ID_PREFIX}{}", "a".repeat(13)),
        ),
        ("a trust store name", stores[0].name.to_string()),
        ("a trust store ARN", stores[0].arn.clone()),
        ("an empty marker", String::new()),
    ] {
        let answer = list_trust_stores(&emulator.router, &list_body(None, Some(&marker))).await;
        assert_eq!(
            answer.status, 400,
            "case marker {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case marker {case}: expected InvalidArgument, got {}",
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(MARKER_INVALID),
            "case marker {case}: expected the sentence {MARKER_INVALID:?}, got {}",
            answer.raw
        );
    }

    // A marker in the shape this service mints, naming no stored trust store: the entity is
    // not there.
    assert!(
        !all.contains(&absent),
        "case an absent marker: the fixture must not hold {absent}"
    );
    for (case, marker) in [
        ("an absent id", absent.clone()),
        (
            "another absent id",
            format!("{TRUST_STORE_ID_PREFIX}{}", "0".repeat(13)),
        ),
    ] {
        let answer = list_trust_stores(&emulator.router, &list_body(None, Some(&marker))).await;
        assert_eq!(
            answer.status, 404,
            "case marker {case}: expected HTTP 404, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case marker {case}: expected EntityNotFound, got {}",
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(TRUST_STORE_NOT_FOUND),
            "case marker {case}: expected the sentence {TRUST_STORE_NOT_FOUND:?}, got {}",
            answer.raw
        );
    }

    // No refusal wrote anything: the collection is exactly what the creates stored.
    let answer = list_trust_stores(&emulator.router, &list_body(None, None)).await;
    assert_eq!(
        summary_ids(&answer).into_iter().collect::<BTreeSet<String>>(),
        all.iter().cloned().collect::<BTreeSet<String>>(),
        "case after the refusals: the store holds exactly what the creates stored, got {}",
        answer.raw
    );
}

/// Rule 5: an account holding no trust store is served an EMPTY listing — not a refusal, and
/// not a resume point for a page that does not exist. A trust store the account then creates
/// is listed, and the one it deletes is not.
#[tokio::test]
async fn an_account_holding_no_trust_store_is_served_an_empty_listing() {
    let emulator = Emulator::new();
    emulator.bundle_bucket().await;

    for (case, body) in [
        ("no MaxItems", list_body(None, None)),
        ("MaxItems one", list_body(Some(1), None)),
        ("MaxItems at the ceiling", list_body(Some(MAX_ITEMS), None)),
    ] {
        let answer = list_trust_stores(&emulator.router, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        assert!(
            summaries(&answer).is_empty(),
            "case {case}: expected no trust store, got {}",
            answer.raw
        );
        assert_absent(answer.root(), "NextMarker", case, "an empty collection");
    }

    // A marker of the shape this service mints names no stored trust store here either.
    let answer = list_trust_stores(
        &emulator.router,
        &list_body(None, Some(&format!("{TRUST_STORE_ID_PREFIX}{}", "Z".repeat(13)))),
    )
    .await;
    assert_eq!(
        answer.status, 404,
        "case a marker against an empty collection: expected HTTP 404, got {} with {}",
        answer.status, answer.raw
    );

    // What the account creates is listed, addressed by the id the create minted — and the
    // ARN the summary carries is the account's own.
    emulator.stage("only.pem", &ca_bundle(1)).await;
    let created = create_trust_store(
        &emulator.router,
        &request_xml("only", BUNDLE_BUCKET, "only.pem", REGION, None, &[]),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case the first trust store: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = member(created.root(), "Id", "the first trust store", &created.raw);
    let answer = list_trust_stores(&emulator.router, &list_body(None, None)).await;
    assert_eq!(
        summary_ids(&answer),
        vec![id.clone()],
        "case the first trust store: expected the one trust store, got {}",
        answer.raw
    );
    assert_eq!(
        summaries(&answer)
            .first()
            .map(|summary| member(summary, "Arn", "the first trust store", &answer.raw)),
        Some(format!("arn:aws:cloudfront::{ACCOUNT_ID}:trust-store/{id}")),
        "case the first trust store: expected the ARN of this account's trust store, got {}",
        answer.raw
    );
}
