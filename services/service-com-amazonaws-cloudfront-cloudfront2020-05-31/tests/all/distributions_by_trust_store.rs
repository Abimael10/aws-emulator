// SPDX-License-Identifier: CC0-1.0

//! `ListDistributionsByTrustStore`, end to end over HTTP.
//!
//! The rule under test: a distribution is listed under the trust store its STORED
//! configuration names through `ViewerMtlsConfig$TrustStoreConfig$TrustStoreId` — the trust
//! store's own minted `Id` — and under no other, so a distribution that configures no viewer
//! mTLS, or whose viewer mTLS configuration names no trust store, is listed by no trust store
//! at all, and an edit to the stored configuration moves the distribution between the
//! listings. The trust store the request names is resolved the way every read of that
//! resource kind resolves one (by id, by the name it was created under, or by its ARN), an
//! identifier the store holds none for is `EntityNotFound` rather than an empty page, and
//! each listing pages over its own matching set with `Marker` and `MaxItems` exactly as the
//! unfiltered listing does.
//!
//! The rows below span what the rule generalises over: the number of distributions pointing
//! at one trust store (none, one, several, so a page can cut inside the set), a distribution
//! that names no trust store at all and one whose viewer mTLS configuration carries no trust
//! store configuration, each `ViewerMtlsMode` the model declares, each of the three
//! identifier forms a trust store answers to, every admissible page size, and the association
//! reached both by the create and by a later update.
//!
//! The trust store fixture is the one [`crate::trust_store_lifecycle`] owns — CloudFront and
//! Amazon S3 over one engine, with a real CA bundle staged through Amazon S3 — and the wire
//! harness (the request helpers and the element scanner) the one
//! [`crate::distribution_lifecycle`] builds.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::VIEWER_MTLS_MODE_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_ITEMS, TRUST_STORE_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, is_distribution_id, list_distributions, update_distribution, Answer,
    Node, API_VERSION,
};
use crate::distributions_by_web_acl_id::encode_label;
use crate::trust_store_lifecycle::{
    ca_bundle, create_trust_store, request_xml, Emulator, BUNDLE_BUCKET, REGION,
};

/// A trust store this service holds: the three identifiers it answers to.
#[derive(Debug, Clone)]
struct Store {
    name: String,
    id: String,
    arn: String,
}

impl Store {
    /// The three forms the identifier of one trust store takes, each labelled, so every row
    /// below is asserted over all of them.
    fn identifiers(&self) -> [(&'static str, &str); 3] {
        [
            ("by-id", self.id.as_str()),
            ("by-name", self.name.as_str()),
            ("by-arn", self.arn.as_str()),
        ]
    }
}

/// Create one trust store from a freshly staged CA bundle and answer the identity its create
/// minted.
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
    }
}

/// What a row's configuration states about viewer mTLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mtls {
    /// No `ViewerMtlsConfig` element at all.
    None,
    /// A `ViewerMtlsConfig` carrying the mode but NO `TrustStoreConfig`, so it names no
    /// trust store.
    ModeOnly(&'static str),
    /// A `ViewerMtlsConfig` whose `TrustStoreConfig` names the fixture's trust store at this
    /// index, under this mode.
    TrustStore(usize, &'static str),
}

impl Mtls {
    /// The fixture trust store index this configuration names, if any.
    fn attaches(self) -> Option<usize> {
        match self {
            Mtls::TrustStore(index, _) => Some(index),
            _ => None,
        }
    }
}

/// The `DistributionConfig` a row submits: one origin and a default cache behaviour, plus the
/// viewer mTLS configuration the row states, with the trust store id it names.
fn config_xml(caller_reference: &str, mtls: Mtls, trust_store_id: Option<&str>) -> String {
    let viewer_mtls = match mtls {
        Mtls::None => String::new(),
        Mtls::ModeOnly(mode) => {
            format!("<ViewerMtlsConfig><Mode>{mode}</Mode></ViewerMtlsConfig>")
        }
        Mtls::TrustStore(_, mode) => format!(
            "<ViewerMtlsConfig><Mode>{mode}</Mode><TrustStoreConfig>\
             <TrustStoreId>{}</TrustStoreId>\
             </TrustStoreConfig></ViewerMtlsConfig>",
            trust_store_id.unwrap_or_default()
        ),
    };
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

/// One `ListDistributionsByTrustStore` call: `GET /2020-05-31/distributionsByTrustStore` with
/// the identifier and the paging query the caller names.
async fn list_by_trust_store(
    router: &MultiServiceRouter,
    identifier: &str,
    query: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/{API_VERSION}/distributionsByTrustStore?TrustStoreIdentifier={}{query}",
            // The identifier travels as one query value, so an ARN's separators are encoded
            // the way an SDK client encodes them.
            encode_label(identifier)
        ),
        None,
        None,
    )
    .await
}

/// The `DistributionSummary` elements of a `DistributionList` answer, in document order.
fn summaries(list: &Node) -> Vec<&Node> {
    list.child("Items")
        .map(|items| items.children_named("DistributionSummary"))
        .unwrap_or_default()
}

/// The ids of a `DistributionList` answer, in document order.
fn listed_ids(list: &Node) -> Vec<String> {
    summaries(list)
        .into_iter()
        .map(|summary| summary.text_at("Id").unwrap_or_default().to_string())
        .collect()
}

/// One distribution a row creates: the case it stands for, and what its configuration states
/// about viewer mTLS.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    mtls: Mtls,
}

/// Several distributions on the first trust store so a page can cut inside its set, exactly
/// one on the second, nothing on the third, one distribution configuring no viewer mTLS at
/// all and one whose viewer mTLS configuration names no trust store. The modes are the values
/// `ViewerMtlsMode` declares (`smithy_query(query="shape",shape="ViewerMtlsMode")`:
/// `required`, `optional`, `passthrough`), so the association is asserted under every one of
/// them.
const ROWS: &[Row] = &[
    Row {
        case: "first-on-the-first-trust-store-required",
        mtls: Mtls::TrustStore(0, "required"),
    },
    Row {
        case: "second-on-the-first-trust-store-optional",
        mtls: Mtls::TrustStore(0, "optional"),
    },
    Row {
        case: "third-on-the-first-trust-store-passthrough",
        mtls: Mtls::TrustStore(0, "passthrough"),
    },
    Row {
        case: "only-one-on-the-second-trust-store",
        mtls: Mtls::TrustStore(1, "required"),
    },
    Row {
        case: "viewer-mtls-naming-no-trust-store",
        mtls: Mtls::ModeOnly("passthrough"),
    },
    Row {
        case: "no-viewer-mtls-at-all",
        mtls: Mtls::None,
    },
];

/// What one emulator the rows were created in holds: the trust stores, and each created
/// distribution with the trust store index it names and its current version token.
struct Fixture {
    stores: Vec<Store>,
    /// `(case, attaches, id, etag)` per created distribution.
    created: Vec<(&'static str, Option<usize>, String, String)>,
}

impl Fixture {
    /// The ids of the distributions pointing at the trust store `index` names, in id order —
    /// what the listing for that trust store must report.
    fn expected(&self, index: usize) -> Vec<String> {
        let mut ids: Vec<String> = self
            .created
            .iter()
            .filter(|(_, attaches, _, _)| *attaches == Some(index))
            .map(|(_, _, id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Create the emulator's bundle bucket, three trust stores and every row's distribution.
async fn fixture(emulator: &Emulator) -> Fixture {
    emulator.bundle_bucket().await;
    let mut stores = Vec::new();
    // The certificate counts differ per trust store so no two records are the same bundle.
    for (name, certificates) in [
        ("first-trust-store", 1),
        ("second-trust-store", 2),
        ("third-trust-store", 3),
    ] {
        stores.push(trust_store(emulator, name, certificates).await);
    }

    let mut created = Vec::new();
    for row in ROWS {
        let named = row.mtls.attaches().map(|index| stores[index].id.as_str());
        let body = config_xml(&format!("ref-{}", row.case), row.mtls, named);
        let answer = create_distribution(&emulator.router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case {}: expected HTTP 201 from CreateDistribution, got {} with {}",
            row.case, answer.status, answer.raw
        );
        let id = answer.root().text_at("Id").unwrap_or_default().to_string();
        assert!(
            is_distribution_id(&id),
            "case {}: expected a minted distribution id, got {id:?}",
            row.case
        );
        let etag = answer.header("etag").to_string();
        created.push((row.case, row.mtls.attaches(), id, etag));
    }

    Fixture { stores, created }
}

/// Walk every page of one trust store's listing, returning the ids in the order they were
/// served.
async fn walk(
    router: &MultiServiceRouter,
    identifier: &str,
    label: &str,
    page_size: Option<i32>,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    // At most one page per distribution plus one, so a listing that never stops truncating
    // fails here rather than looping forever.
    for page in 0..=ROWS.len() + 1 {
        let query = match (page_size, marker.as_deref()) {
            (None, None) => String::new(),
            (None, Some(value)) => format!("&Marker={value}"),
            (Some(size), None) => format!("&MaxItems={size}"),
            (Some(size), Some(value)) => format!("&MaxItems={size}&Marker={value}"),
        };
        let answer = list_by_trust_store(router, identifier, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {label} page {page}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let list = answer.root();
        assert_eq!(
            list.text_at("Marker"),
            Some(marker.as_deref().unwrap_or_default()),
            "case {label} page {page}: expected the Marker the request carried, got {:?}",
            list.text_at("Marker")
        );
        // "The value you provided for the MaxItems request parameter."
        assert_eq!(
            list.text_at("MaxItems"),
            Some(page_size.unwrap_or(MAX_ITEMS).to_string().as_str()),
            "case {label} page {page}: expected MaxItems to echo the page size, got {:?}",
            list.text_at("MaxItems")
        );
        let ids = listed_ids(list);
        assert_eq!(
            list.text_at("Quantity"),
            Some(ids.len().to_string().as_str()),
            "case {label} page {page}: expected Quantity to state the items served ({}), got {:?}",
            ids.len(),
            list.text_at("Quantity")
        );
        if let Some(size) = page_size {
            assert!(
                i32::try_from(ids.len()).unwrap_or(i32::MAX) <= size.min(MAX_ITEMS),
                "case {label} page {page}: expected at most {} items, got {}",
                size.min(MAX_ITEMS),
                ids.len()
            );
        }
        collected.extend(ids.iter().cloned());

        let next_marker = list.text_at("NextMarker").map(str::to_string);
        let is_truncated = list.text_at("IsTruncated").unwrap_or_default().to_string();
        match next_marker {
            Some(next) => {
                assert_eq!(
                    is_truncated, "true",
                    "case {label} page {page}: a NextMarker means IsTruncated true, got \
                     {is_truncated:?}"
                );
                assert_eq!(
                    ids.last().map(String::as_str),
                    Some(next.as_str()),
                    "case {label} page {page}: expected NextMarker to be the id this page ended \
                     at, got {next:?}"
                );
                marker = Some(next);
            }
            None => {
                assert_eq!(
                    is_truncated, "false",
                    "case {label} page {page}: no NextMarker means IsTruncated false, got \
                     {is_truncated:?}"
                );
                return collected;
            }
        }
    }
    panic!("case {label}: the listing never stopped truncating");
}

/// Each distribution is listed under the trust store its configuration names, under every
/// identifier form that trust store answers to, and the summaries are the stored records the
/// unfiltered listing reports.
#[tokio::test]
async fn each_distribution_is_listed_under_the_trust_store_it_names() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let unfiltered = list_distributions(&emulator.router, "").await;

    for (index, store) in fixture.stores.iter().enumerate() {
        let expected = fixture.expected(index);
        for (form, identifier) in store.identifiers() {
            let label = format!("{}/{form}", store.name);
            let answer = list_by_trust_store(&emulator.router, identifier, "").await;
            assert_eq!(
                answer.status, 200,
                "case {label}: expected HTTP 200, got {} with {}",
                answer.status, answer.raw
            );
            let list = answer.root();
            let ids = listed_ids(list);
            assert_eq!(
                ids, expected,
                "case {label}: expected the distributions naming this trust store in id order, \
                 got {ids:?}"
            );

            // The summaries are the stored records, not something this listing invented:
            // each one matches the summary the unfiltered listing reports for the same id.
            for id in &ids {
                let filtered_summary = summaries(list)
                    .into_iter()
                    .find(|summary| summary.text_at("Id") == Some(id.as_str()))
                    .cloned()
                    .unwrap_or_default();
                let plain_summary = summaries(unfiltered.root())
                    .into_iter()
                    .find(|summary| summary.text_at("Id") == Some(id.as_str()))
                    .cloned()
                    .unwrap_or_default();
                for member in ["ARN", "Status", "DomainName", "ETag", "Comment"] {
                    assert_eq!(
                        filtered_summary.text_at(member),
                        plain_summary.text_at(member),
                        "case {label} {id}: expected {member} to be the stored record's {:?}, \
                         got {:?}",
                        plain_summary.text_at(member),
                        filtered_summary.text_at(member)
                    );
                }
                // The summary reports the very trust store the listing selected it by, under
                // the mode its configuration stated.
                let named = filtered_summary
                    .at("ViewerMtlsConfig")
                    .and_then(|mtls| mtls.at("TrustStoreConfig"))
                    .and_then(|config| config.text_at("TrustStoreId"))
                    .unwrap_or_default()
                    .to_string();
                assert_eq!(
                    named, store.id,
                    "case {label} {id}: expected the summary to name the trust store it was \
                     listed under, got {named:?}"
                );
            }
        }
    }

    // A distribution naming no trust store is listed by none of them, and no listing ever
    // reports a distribution twice or one associated elsewhere.
    let attached_nowhere: Vec<String> = fixture
        .created
        .iter()
        .filter(|(_, attaches, _, _)| attaches.is_none())
        .map(|(_, _, id, _)| id.clone())
        .collect();
    assert_eq!(
        attached_nowhere.len(),
        2,
        "the table must cover a distribution with no viewer mTLS and one whose viewer mTLS \
         names no trust store, got {attached_nowhere:?}"
    );
    let mut union: Vec<String> = Vec::new();
    for store in &fixture.stores {
        union.extend(listed_ids(
            list_by_trust_store(&emulator.router, &store.id, "")
                .await
                .root(),
        ));
    }
    for id in &attached_nowhere {
        assert!(
            !union.contains(id),
            "expected {id} to be listed under no trust store, got {union:?}"
        );
    }
    let mut sorted = union.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        union.len(),
        "expected each distribution under at most one trust store, got {union:?}"
    );
}

/// Every mode `ViewerMtlsMode` declares is covered by the table, so the association is the
/// trust store the configuration names rather than the mode beside it.
#[tokio::test]
async fn the_association_holds_under_every_viewer_mtls_mode() {
    let modes: Vec<&str> = ROWS
        .iter()
        .filter_map(|row| match row.mtls {
            Mtls::TrustStore(_, mode) => Some(mode),
            _ => None,
        })
        .collect();
    for mode in VIEWER_MTLS_MODE_VALUES {
        assert!(
            modes.contains(mode),
            "case {mode}: the table must associate a trust store under this mode, got {modes:?}"
        );
    }

    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let expected = fixture.expected(0);
    let listed = listed_ids(
        list_by_trust_store(&emulator.router, &fixture.stores[0].id, "")
            .await
            .root(),
    );
    assert_eq!(
        listed, expected,
        "case every-mode: expected the distributions naming the first trust store whatever \
         their mode, got {listed:?}"
    );
}

/// Walking the pages at ANY admissible page size enumerates one trust store's distributions
/// exactly once, in id order, under every identifier form.
#[tokio::test]
async fn walking_the_pages_enumerates_each_trust_store_exactly_once() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;

    for (index, store) in fixture.stores.iter().enumerate() {
        let expected = fixture.expected(index);
        for (form, identifier) in store.identifiers() {
            for page_size in [None, Some(1), Some(2), Some(3), Some(MAX_ITEMS), Some(i32::MAX)] {
                let label = match page_size {
                    None => format!("{}/{form}/default", store.name),
                    Some(size) => format!("{}/{form}/MaxItems={size}", store.name),
                };
                let collected = walk(&emulator.router, identifier, &label, page_size).await;
                assert_eq!(
                    collected, expected,
                    "case {label}: expected every distribution naming this trust store exactly \
                     once in id order, got {collected:?}"
                );
            }
        }
    }
}

/// A trust store no distribution names lists nothing — no `Items` element, `Quantity` zero
/// and no `NextMarker` — rather than falling back to the whole store.
#[tokio::test]
async fn a_trust_store_nothing_names_lists_nothing() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let idle = &fixture.stores[2];

    for (form, identifier) in idle.identifiers() {
        let answer = list_by_trust_store(&emulator.router, identifier, "").await;
        assert_eq!(
            answer.status, 200,
            "case {form}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let list = answer.root();
        assert_eq!(
            list.text_at("Quantity"),
            Some("0"),
            "case {form}: expected Quantity 0, got {:?}",
            list.text_at("Quantity")
        );
        assert!(
            list.child("Items").is_none(),
            "case {form}: expected no Items element at all, got {}",
            answer.raw
        );
        assert_eq!(
            list.text_at("IsTruncated"),
            Some("false"),
            "case {form}: expected IsTruncated false, got {:?}",
            list.text_at("IsTruncated")
        );
        assert!(
            list.text_at("NextMarker").is_none(),
            "case {form}: expected no NextMarker, got {:?}",
            list.text_at("NextMarker")
        );
    }
}

/// An identifier no stored trust store answers to is `EntityNotFound` (HTTP 404) rather than
/// a page of nothing.
#[tokio::test]
async fn an_identifier_naming_no_trust_store_is_not_found() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let present = &fixture.stores[0];

    // Rows: a name never created, a prefix of a name that exists, the same name in another
    // case (identifiers are compared as given), an id in the shape this service mints that
    // was never allocated, and the ARN of such an id.
    let absent: Vec<(&str, String)> = vec![
        ("a-name-never-created", "no-such-trust-store".to_string()),
        (
            "a-prefix-of-a-name-that-exists",
            present.name[..present.name.len() - 1].to_string(),
        ),
        ("the-name-in-another-case", present.name.to_uppercase()),
        ("an-id-never-allocated", "ts_ABCDEFGHIJKLM".to_string()),
        (
            "the-arn-of-an-id-never-allocated",
            present.arn.replace(&present.id, "ts_ABCDEFGHIJKLM"),
        ),
    ];

    for (case, identifier) in &absent {
        let answer = list_by_trust_store(&emulator.router, identifier, "").await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case {case}: expected EntityNotFound, got {}",
            answer.raw
        );
        let message = answer
            .root()
            .find_text("Message")
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message, TRUST_STORE_NOT_FOUND_MESSAGE,
            "case {case}: expected the fixed not-found sentence, got {message:?}"
        );
        assert!(
            !message.contains(identifier.as_str()),
            "case {case}: the refusal must not name the identifier {identifier:?}, got \
             {message:?}"
        );
    }
}

/// A page size that names no page, and a marker this service never issued, are refused as
/// invalid arguments — the same constraint the unfiltered listing states.
#[tokio::test]
async fn an_unusable_page_size_or_marker_is_refused() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;

    let refusals: &[(&str, &str)] = &[
        ("&MaxItems=-7", "a negative page size"),
        ("&MaxItems=not-a-number", "a page size that is no integer"),
        ("&Marker=not-an-id", "a marker that is no distribution id"),
    ];
    for store in &fixture.stores {
        for (query, description) in refusals {
            let answer = list_by_trust_store(&emulator.router, &store.id, query).await;
            assert_eq!(
                answer.status, 400,
                "case {} with {description}: expected HTTP 400, got {} with {}",
                store.name, answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {} with {description}: expected InvalidArgument, got {:?}",
                store.name,
                answer.error_code()
            );
        }
    }
}

/// The listing answers from the STORED configuration at this instant: moving a distribution's
/// viewer mTLS configuration to another trust store, and dropping it altogether, move the
/// distribution between the listings.
#[tokio::test]
async fn the_listing_follows_the_stored_configuration() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;
    let first = fixture.stores[0].id.clone();
    let second = fixture.stores[1].id.clone();

    // The distribution this row moves: the first one naming the first trust store.
    let (case, _, moved_id, etag) = fixture
        .created
        .iter()
        .find(|(_, attaches, _, _)| *attaches == Some(0))
        .cloned()
        .expect("the table names the first trust store from a distribution");

    // Move it onto the second trust store, under the version the create minted.
    let body = config_xml(
        &format!("ref-{case}"),
        Mtls::TrustStore(1, "required"),
        Some(&second),
    );
    let updated = update_distribution(&emulator.router, &moved_id, &body, Some(&etag)).await;
    assert_eq!(
        updated.status, 200,
        "case {case} moved: expected HTTP 200 from UpdateDistribution, got {} with {}",
        updated.status, updated.raw
    );
    let moved_etag = updated.header("etag").to_string();

    let on_first = listed_ids(list_by_trust_store(&emulator.router, &first, "").await.root());
    assert!(
        !on_first.contains(&moved_id),
        "case {case} moved: expected {moved_id} to leave the first trust store's listing, got \
         {on_first:?}"
    );
    let on_second = listed_ids(
        list_by_trust_store(&emulator.router, &second, "")
            .await
            .root(),
    );
    assert!(
        on_second.contains(&moved_id),
        "case {case} moved: expected {moved_id} in the second trust store's listing, got \
         {on_second:?}"
    );

    // Drop the viewer mTLS configuration entirely: the distribution is then listed under no
    // trust store at all, while still existing in the unfiltered listing.
    let body = config_xml(&format!("ref-{case}"), Mtls::None, None);
    let cleared = update_distribution(&emulator.router, &moved_id, &body, Some(&moved_etag)).await;
    assert_eq!(
        cleared.status, 200,
        "case {case} cleared: expected HTTP 200 from UpdateDistribution, got {} with {}",
        cleared.status, cleared.raw
    );
    for store in &fixture.stores {
        let ids = listed_ids(
            list_by_trust_store(&emulator.router, &store.id, "")
                .await
                .root(),
        );
        assert!(
            !ids.contains(&moved_id),
            "case {case} cleared: expected {moved_id} under no trust store, got {ids:?} for {}",
            store.name
        );
    }
    let all_ids = listed_ids(list_distributions(&emulator.router, "").await.root());
    assert!(
        all_ids.contains(&moved_id),
        "case {case} cleared: expected {moved_id} to still exist, got {all_ids:?}"
    );
}

/// Listing is a READ: calling it never changes what a later caller sees.
#[tokio::test]
async fn listing_changes_nothing() {
    let emulator = Emulator::new();
    let fixture = fixture(&emulator).await;

    for store in &fixture.stores {
        let first = list_by_trust_store(&emulator.router, &store.id, "").await;
        let before = list_distributions(&emulator.router, "").await;
        let second = list_by_trust_store(&emulator.router, &store.id, "").await;
        let after = list_distributions(&emulator.router, "").await;
        assert_eq!(
            listed_ids(first.root()),
            listed_ids(second.root()),
            "case {}: expected a repeated listing to report the same distributions",
            store.name
        );
        assert_eq!(
            listed_ids(before.root()),
            listed_ids(after.root()),
            "case {}: expected ListDistributions to be unchanged by the filtered listing",
            store.name
        );
    }
}
