// SPDX-License-Identifier: CC0-1.0

//! What an invalidation batch may NAME, and when its own consistency is answered, end to end
//! over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. Two rules:
//!
//! * an item of `Paths` names either an OBJECT of the distribution — "the path is relative to
//!   the distribution and must begin with a slash (/)" — or, on a distribution whose
//!   configuration carries a `CacheTagConfig` ("CloudFront reads the specified header from
//!   origin responses, parses the comma-separated tag values, and stores them with the cached
//!   object. You can then invalidate cached objects by tag using the `CreateInvalidation`
//!   API"), a CACHE TAG, written as `#` and one tag value. Accepted items are stored and read
//!   back verbatim, and when the batch finishes the objects the origin tagged leave that
//!   distribution's edge cache — exactly those, the objects under other tags staying put;
//! * the batch's own `Paths` wrapper states a length beside the list it carries, and that
//!   agreement is a property of the REQUEST DOCUMENT alone, so it is answered BEFORE the
//!   resource the batch is submitted against is resolved — on both surfaces that take an
//!   invalidation batch.
//!
//! The rows span what each rule generalises over: the FORM of an item (an object path, a cache
//! tag alone, a cache tag beside an object path, two cache tags, a tag carrying a colon and a
//! tag carrying none, and the forms that are no item at all — a bare `#`, a tag carrying
//! whitespace, a tag carrying the comma that separates two tags in the header, and a value
//! that begins with neither `/` nor `#`), whether the distribution extracts cache tags at all,
//! and — for the ordering rule — the surface the batch is submitted on, whether the resource
//! exists and whether the wrapper's `Quantity` agrees with its `Items`.
//!
//! The harness (the router, the request helpers and the element scanner) is
//! [`crate::distribution_lifecycle`]'s, the tenant surface is reached through
//! [`crate::tenant_invalidations`]'s helpers, and the viewer leg uses the one the edge suites
//! use: a distribution over an origin this emulator itself serves.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::ComAmazonawsCloudfrontCloudfront20200531State;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, batch_xml, batch_xml_with_quantity, call, create_distribution,
    create_invalidation, emulator, get_invalidation, inconsistent_quantities_message, Answer,
    API_VERSION, HOST,
};
use crate::tenant_invalidations::{create_invalidation_for_tenant, distribution_with_tenant};

/// The sentence a batch naming something that is no invalidation item is refused with, as a
/// literal rather than read back from the crate: the WORDING is what this refusal is held to.
const INVALID_PATHS: &str = "Your request contains one or more invalid invalidation paths.";

/// The header this suite's distributions have CloudFront extract their cache tags from. Mixed
/// case, because the header name is matched as HTTP matches one — the origin states it in
/// whatever case it likes.
const TAG_HEADER: &str = "X-Cache-Tags";

/// A distribution id of the form this service mints that names no distribution of the account.
const ABSENT_DISTRIBUTION: &str = "EABSENT1234567";

/// A distribution tenant id of the form CloudFront writes one in that names no tenant of the
/// account.
const ABSENT_TENANT: &str = "dt_absent000000000000000000";

/// A `DistributionConfig` over one custom origin, with `extra` appended inside the element —
/// which is where `CacheTagConfig` goes, after `Enabled`.
fn config_xml(reference: &str, extra: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>{reference}</Comment><Enabled>true</Enabled>{extra}\
         </DistributionConfig>"
    )
}

/// The `CacheTagConfig` member a distribution that extracts cache tags carries.
fn cache_tag_config() -> String {
    format!("<CacheTagConfig><HeaderName>{TAG_HEADER}</HeaderName></CacheTagConfig>")
}

/// Create one distribution, with or without a `CacheTagConfig`, and report its id.
async fn distribution(router: &MultiServiceRouter, reference: &str, tagged: bool) -> String {
    let extra = if tagged { cache_tag_config() } else { String::new() };
    let answer = create_distribution(router, &config_xml(reference, &extra)).await;
    assert_eq!(
        answer.status, 201,
        "case {reference}: expected HTTP 201 from CreateDistribution, got {} with {}",
        answer.status, answer.raw
    );
    answer
        .root()
        .text_at("Id")
        .unwrap_or_else(|| panic!("case {reference}: the create carries no Id: {}", answer.raw))
        .to_string()
}

/// The `Paths$Items` an invalidation payload reports, in document order.
fn reported_paths(answer: &Answer) -> Vec<String> {
    answer
        .root()
        .at("InvalidationBatch/Paths/Items")
        .map(|items| {
            items
                .children_named("Path")
                .into_iter()
                .map(|path| path.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// An item naming a CACHE TAG is admitted exactly where the distribution extracts cache tags
/// from its origin responses, beside the object paths every distribution admits — and the
/// stored batch reports back exactly the items that were submitted, in their submitted order.
#[tokio::test]
async fn a_cache_tag_item_is_admitted_where_the_distribution_extracts_cache_tags() {
    let router = emulator();
    let tagged = distribution(&router, "ref-cache-tags", true).await;
    let untagged = distribution(&router, "ref-no-cache-tags", false).await;

    // (case, the items the batch names, whether a distribution that extracts cache tags
    // admits it, whether one that does not admits it)
    let rows: [(&str, &[&str], bool, bool); 10] = [
        ("one object path", &["/index.html"], true, true),
        ("one cache tag", &["#product:123"], true, false),
        (
            "a cache tag beside an object path",
            &["#category:electronics", "/index.html"],
            true,
            false,
        ),
        ("two cache tags", &["#brand:example", "#category:tv"], true, false),
        ("a tag carrying no colon", &["#clearance"], true, false),
        ("a tag carrying a dot and a dash", &["#v1.2-beta"], true, false),
        ("a bare prefix naming no tag", &["#"], false, false),
        ("a tag carrying whitespace", &["#product 123"], false, false),
        (
            "a tag carrying the separator the header uses",
            &["#product:123,category:tv"],
            false,
            false,
        ),
        ("a value that is neither", &["index.html"], false, false),
    ];

    for (index, (case, paths, tagged_admits, untagged_admits)) in rows.iter().enumerate() {
        for (surface, id, admits) in [
            ("a distribution that extracts cache tags", &tagged, *tagged_admits),
            (
                "a distribution that extracts none",
                &untagged,
                *untagged_admits,
            ),
        ] {
            let label = format!("{case} / {surface}");
            let reference = format!("tags-{index}-{}", id);
            let answer = create_invalidation(&router, id, &batch_xml(&reference, paths)).await;

            if !admits {
                assert_eq!(
                    answer.status, 400,
                    "case {label}: expected HTTP 400, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    "InvalidArgument",
                    "case {label}: expected InvalidArgument, got {:?} in {}",
                    answer.error_code(),
                    answer.raw
                );
                let message = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    message, INVALID_PATHS,
                    "case {label}: expected the sentence {INVALID_PATHS:?}, got {message:?}"
                );
                continue;
            }

            assert_eq!(
                answer.status, 201,
                "case {label}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
            let expected: Vec<String> = paths.iter().map(|path| path.to_string()).collect();
            let created = reported_paths(&answer);
            assert_eq!(
                created, expected,
                "case {label}: expected the create to report the submitted items, got \
                 {created:?}"
            );
            let invalidation_id = answer
                .root()
                .find_text("Id")
                .unwrap_or_default()
                .to_string();

            // A read answers from the stored batch, so it reports the same items.
            let read = get_invalidation(&router, id, &invalidation_id).await;
            assert_eq!(
                read.status, 200,
                "case {label}: expected GetInvalidation to serve the invalidation, got {} with \
                 {}",
                read.status, read.raw
            );
            let served = reported_paths(&read);
            assert_eq!(
                served, expected,
                "case {label}: expected the read to report the submitted items, got {served:?}"
            );
            let quantity = read.root().text_at("InvalidationBatch/Paths/Quantity");
            assert_eq!(
                quantity,
                Some(expected.len().to_string().as_str()),
                "case {label}: expected Paths.Quantity {}, got {quantity:?}",
                expected.len()
            );
        }
    }
}

/// Which surface a batch is submitted on.
#[derive(Debug, Clone, Copy)]
enum Surface {
    /// `CreateInvalidation` — the batch names objects of a distribution.
    Distribution,
    /// `CreateInvalidationForDistributionTenant` — the batch names objects of a tenant.
    Tenant,
}

impl Surface {
    fn label(self) -> &'static str {
        match self {
            Surface::Distribution => "CreateInvalidation",
            Surface::Tenant => "CreateInvalidationForDistributionTenant",
        }
    }

    /// The error code a resource this surface holds no record for is reported with.
    fn absent_code(self) -> &'static str {
        match self {
            Surface::Distribution => "NoSuchDistribution",
            Surface::Tenant => "EntityNotFound",
        }
    }
}

/// The `Paths` wrapper's own agreement is answered before the resource the batch names is
/// resolved: a document that disagrees with itself is `InconsistentQuantities` whether or not
/// the resource exists, while a document that agrees reaches the resource — and is refused as
/// absent only then. On every surface that takes an invalidation batch.
#[tokio::test]
async fn a_batchs_quantity_is_answered_before_the_resource_is_resolved() {
    let router = emulator();
    let held = distribution(&router, "ref-batch-order", false).await;
    let tenant = distribution_with_tenant(&router, "validation-order").await;

    // (case, whether the resource exists, the Quantity the document states, the items it
    // carries)
    let rows: [(&str, bool, i32, &[&str]); 6] = [
        ("a resource that exists, under an agreeing quantity", true, 2, &["/a.html", "/b.html"]),
        ("a resource that exists, under a quantity one too low", true, 1, &["/a.html", "/b.html"]),
        ("a resource that does not exist, under an agreeing quantity", false, 2, &["/a.html", "/b.html"]),
        ("a resource that does not exist, under a quantity one too low", false, 1, &["/a.html", "/b.html"]),
        ("a resource that does not exist, under a quantity one too high", false, 3, &["/a.html", "/b.html"]),
        ("a resource that does not exist, under a quantity for no items at all", false, 1, &[]),
    ];

    for surface in [Surface::Distribution, Surface::Tenant] {
        for (index, (case, exists, stated, paths)) in rows.iter().enumerate() {
            let label = format!("{}/{case}", surface.label());
            let id = match (surface, *exists) {
                (Surface::Distribution, true) => held.clone(),
                (Surface::Distribution, false) => ABSENT_DISTRIBUTION.to_string(),
                (Surface::Tenant, true) => tenant.clone(),
                (Surface::Tenant, false) => ABSENT_TENANT.to_string(),
            };
            let batch = batch_xml_with_quantity(
                &format!("order-{}-{index}", surface.label()),
                paths,
                *stated,
            );
            let answer = match surface {
                Surface::Distribution => create_invalidation(&router, &id, &batch).await,
                Surface::Tenant => create_invalidation_for_tenant(&router, &id, &batch).await,
            };

            let agrees = usize::try_from(*stated).ok() == Some(paths.len());
            if !agrees {
                assert_eq!(
                    answer.status, 400,
                    "case {label}: expected HTTP 400, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    "InconsistentQuantities",
                    "case {label}: expected InconsistentQuantities, got {:?} in {}",
                    answer.error_code(),
                    answer.raw
                );
                let expected = inconsistent_quantities_message("Paths", *stated, paths.len());
                let message = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    message, expected,
                    "case {label}: expected the sentence {expected:?}, got {message:?}"
                );
                continue;
            }

            if !*exists {
                assert_eq!(
                    answer.status, 404,
                    "case {label}: expected HTTP 404 once the document agrees with itself, got \
                     {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    surface.absent_code(),
                    "case {label}: expected {}, got {:?} in {}",
                    surface.absent_code(),
                    answer.error_code(),
                    answer.raw
                );
                continue;
            }

            assert_eq!(
                answer.status, 201,
                "case {label}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
        }
    }
}

// ---------------------------------------------------------------------------
// What a finished cache-tag batch removes from the edge cache
// ---------------------------------------------------------------------------

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it answers a second request for the same object out of the edge cache.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// Two paths an origin this emulator SERVES answers — this service's own control plane, which
/// is the origin the edge suites fetch from.
const TAGGED_PATH: &str = "/2020-05-31/distribution";
const OTHER_PATH: &str = "/2020-05-31/public-key";

/// `X-Cache` for an answer the edge cache held, and for one the origin produced.
const HIT: &str = "Hit from cloudfront";
const MISS: &str = "Miss from cloudfront";

/// One emulator on an engine this suite KEEPS: a case states what the ORIGIN said about the
/// cache tags of an object, and that the clock has finished a batch, by writing the store the
/// edge reads — which needs this handle.
fn emulator_with_its_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Send one VIEWER request — no AWS credential, addressed to the distribution's own domain,
/// exactly as a browser reaches it — and report `(status, X-Cache)`.
async fn view(router: &MultiServiceRouter, host: &str, path: &str) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, x_cache)
}

/// State that the origin's answer for the object cached under `path` listed `tags` in the
/// header this distribution extracts its cache tags from.
///
/// The header is written in the case an origin states it in, lowercased as the edge holds an
/// origin's header names.
fn origin_tagged(
    engine: &Arc<engine::InMemoryEngine>,
    distribution_id: &str,
    path: &str,
    tags: &str,
) {
    let state = engine.state::<ComAmazonawsCloudfrontCloudfront20200531State>();
    let mut store = state.lock();
    let distribution = store
        .distributions
        .get_mut(distribution_id)
        .unwrap_or_else(|| panic!("the store holds distribution {distribution_id}"));
    let mut tagged = false;
    for entry in distribution.edge_cache.values_mut() {
        if entry.path != path {
            continue;
        }
        entry
            .headers
            .retain(|(name, _)| name != &TAG_HEADER.to_lowercase());
        entry
            .headers
            .push((TAG_HEADER.to_lowercase(), tags.to_string()));
        tagged = true;
    }
    assert!(
        tagged,
        "the edge cache holds no object for {path:?}, so there is nothing for the origin to \
         have tagged"
    );
}

/// State that the clock has reached the instant every invalidation of `distribution_id` is
/// owed to finish at, rather than waiting for it.
fn batches_due(engine: &Arc<engine::InMemoryEngine>, distribution_id: &str) {
    let due = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .expect("the clock reaches back a second");
    let state = engine.state::<ComAmazonawsCloudfrontCloudfront20200531State>();
    let mut store = state.lock();
    let distribution = store
        .distributions
        .get_mut(distribution_id)
        .unwrap_or_else(|| panic!("the store holds distribution {distribution_id}"));
    for invalidation in distribution.invalidations.values_mut() {
        invalidation.completed_due = due;
    }
}

/// A batch naming a cache TAG removes from the edge cache exactly the objects the origin
/// tagged with it — the objects under another tag stay held — and it does so when the batch
/// FINISHES, as a batch naming paths does.
#[tokio::test]
async fn a_finished_cache_tag_batch_removes_the_objects_the_origin_tagged() {
    let (router, engine) = emulator_with_its_engine();
    let created = call(
        &router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>ref-cache-tag-edge</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>primary</Id><DomainName>{HOST}</DomainName>\
             <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId></DefaultCacheBehavior>\
             <Comment>cache tag invalidation</Comment><Enabled>true</Enabled>{}\
             </DistributionConfig>",
            cache_tag_config()
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected the distribution to be created, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .text_at("Id")
        .expect("the create reports an id")
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .expect("the create reports a domain name")
        .to_string();
    awaited_deployment(&router, "cache tag invalidation", &id).await;

    // Fill the cache with two objects, and state what the origin said about each one's tags.
    for path in [TAGGED_PATH, OTHER_PATH] {
        let (status, x_cache) = view(&router, &domain, path).await;
        assert_eq!(
            (status, x_cache.as_str()),
            (200, MISS),
            "case {path}: the first request must reach the origin, got {status} with \
             {x_cache:?}"
        );
    }
    origin_tagged(&engine, &id, TAGGED_PATH, "product:123, category:tv");
    origin_tagged(&engine, &id, OTHER_PATH, "brand:example");
    for path in [TAGGED_PATH, OTHER_PATH] {
        let (status, x_cache) = view(&router, &domain, path).await;
        assert_eq!(
            (status, x_cache.as_str()),
            (200, HIT),
            "case {path}: the object must be held before anything is invalidated, got {status} \
             with {x_cache:?}"
        );
    }

    // One batch naming one tag.
    let answer = create_invalidation(
        &router,
        &id,
        &batch_xml("ref-tag-batch", &["#product:123"]),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "expected the cache-tag batch to be accepted, got {} with {}",
        answer.status, answer.raw
    );
    let invalidation_id = answer
        .root()
        .find_text("Id")
        .unwrap_or_default()
        .to_string();

    // An accepted batch has not finished yet, so the object it names is still served from the
    // cache.
    let (status, x_cache) = view(&router, &domain, TAGGED_PATH).await;
    assert_eq!(
        (status, x_cache.as_str()),
        (200, HIT),
        "case the accepted batch: an unfinished batch removes nothing, got {status} with \
         {x_cache:?}"
    );

    // The clock finishes it, which is what empties what the batch named.
    batches_due(&engine, &id);
    let read = get_invalidation(&router, &id, &invalidation_id).await;
    assert_eq!(
        read.root().text_at("Status"),
        Some("Completed"),
        "case the finished batch: expected the batch to report Completed, got {:?}",
        read.root().text_at("Status")
    );

    let (status, x_cache) = view(&router, &domain, TAGGED_PATH).await;
    assert_eq!(
        (status, x_cache.as_str()),
        (200, MISS),
        "case the tagged object: expected the object the origin tagged product:123 to be gone \
         from the cache, got {status} with {x_cache:?}"
    );
    let (status, x_cache) = view(&router, &domain, OTHER_PATH).await;
    assert_eq!(
        (status, x_cache.as_str()),
        (200, HIT),
        "case the object under another tag: expected it to be held still, got {status} with \
         {x_cache:?}"
    );
}
