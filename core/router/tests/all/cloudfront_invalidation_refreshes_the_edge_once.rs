// SPDX-License-Identifier: CC0-1.0

//! What ONE completed invalidation does to a distribution's edge cache over a PRIVATE Amazon
//! S3 origin reached through a legacy origin access identity, end to end over HTTP across the
//! two services on one engine.
//!
//! RULE: a completed invalidation takes the objects its paths name out of the edge cache
//! EXACTLY ONCE — the next viewer request for such an object reaches the origin again, and the
//! answer it refetches is then HELD under the matched behaviour's cache policy like any other,
//! so a later request inside that TTL is served from the cache; a further write AT THE ORIGIN
//! is therefore invisible to every viewer until ANOTHER invalidation naming the object
//! completes. An invalidation is not a switch that leaves the object uncached, and it is not a
//! subscription to the origin: it is one eviction, after which the cache fills again.
//!
//! Only a cross-service suite can pin that: what a viewer is served has to change AT THE ORIGIN
//! between two viewer requests, which needs a real S3 bucket behind the distribution, and the
//! bucket this one uses is PRIVATE — its policy grants the read to the distribution's origin
//! access identity alone, so the edge is the only path to the bytes. The cloudfront crate's own
//! `edge_data_plane` suites pin the invalidation's asynchrony against an origin whose bytes
//! never change, and [`super::cloudfront_edge_cache_refresh`] pins that a completed
//! invalidation is what surfaces NEW bytes; what is new here is the half after that eviction —
//! the refetched object being cached again, and a write that no invalidation names staying
//! unseen however many times a viewer asks for it.
//!
//! The rows span what the rule generalises over: the shape of each invalidation batch (a
//! wildcard over the whole distribution, the object's exact path, a wildcard over its own
//! prefix, a batch naming several paths one of which matches), the key form of the object (a
//! bare name, a nested prefix, and the object `DefaultRootObject` names dialled at the
//! distribution's root), and how many successive viewer requests are made for the object the
//! second origin write was never invalidated for.
//!
//! The harness is [`super::cloudfront_oac_s3_origin_edge`]'s, reused rather than repeated: the
//! same two routers (setup calls through a router with request authentication off, so each
//! carries a credential scope without a computed signature; VIEWER requests through an
//! authenticating router and carrying no credential at all, as a browser reaches a
//! distribution), the same private bucket, and — from
//! [`super::cloudfront_oai_s3_origin_edge`] — the same origin access identity and the bucket
//! document that grants it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_edge_cache_refresh::{create_invalidation, get_invalidation};
use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, emulator, origin_domain, put_bucket_policy, put_object,
    view, API_VERSION, CLOUDFRONT_HOST,
};
use super::cloudfront_oai_s3_origin_edge::{
    create_origin_access_identity, identity_policy, identity_principal,
};

/// `X-Cache` for an answer the edge cache held.
const HIT: &str = "Hit from cloudfront";

/// `X-Cache` for an answer the origin produced for this request.
const MISS: &str = "Miss from cloudfront";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: an
/// object the edge fetched under it stays in the cache until something takes it out, which is
/// what makes an eviction the only thing that can change what a viewer is served. Its id and
/// name are the catalog's own entry (the managed policies `ListCachePolicies` serves with
/// `Type=managed`).
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The object `DistributionConfig$DefaultRootObject` names on every row's distribution — "the
/// object that you want CloudFront to request from your origin … when a viewer requests the
/// root URL for your distribution", written without a leading separator as the member
/// documents.
const DEFAULT_ROOT_OBJECT: &str = "index.html";

/// The server-side encryption the origin bucket's objects are written under (SSE-S3), which is
/// what a private origin bucket stores them with. The bytes a viewer is served are the object's
/// own whatever encryption S3 recorded for it, so every row writes its three revisions the same
/// way.
const SSE_S3: &[(&str, &str)] = &[("x-amz-server-side-encryption", "AES256")];

/// How long a row waits for a state the clock owes, and how often it looks. The wait is on the
/// CONDITION — the status CloudFront reports — with a deadline far past the propagation any
/// configuration or batch is owed.
const POLL: Duration = Duration::from_millis(25);
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// `cloudfront:CreateDistribution` over the private S3 origin `bucket`, fronted by the origin
/// access identity `identity` and caching under [`CACHING_OPTIMIZED`], answering the
/// distribution's `(Id, DomainName)`.
///
/// The members stand in the model's own declaration order
/// (`smithy_query(query="shape",shape="DistributionConfig")`).
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    identity: &str,
) -> (String, String) {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <DefaultRootObject>{DEFAULT_ROOT_OBJECT}</DefaultRootObject>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{}</DomainName>\
         <S3OriginConfig>\
         <OriginAccessIdentity>origin-access-identity/cloudfront/{identity}</OriginAccessIdentity>\
         </S3OriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>redirect-to-https</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>an invalidated private s3 origin behind an origin access identity</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(bucket)
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain) = (created.element("Id"), created.element("DomainName"));
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case:?}: the created distribution reports no Id or DomainName: {:?}",
        created.text()
    );
    (id, domain)
}

/// Poll `GetInvalidation` until CloudFront first reports the batch `invalidation` finished,
/// which is the state its paths have left the caches in.
///
/// The wait is on the status CloudFront reports, never on a span of the clock.
async fn awaited_completion(
    router: &MultiServiceRouter,
    case: &str,
    distribution: &str,
    invalidation: &str,
) {
    let polls = POLL_DEADLINE.as_millis() / POLL.as_millis().max(1);
    for _ in 0..polls {
        let read = get_invalidation(router, distribution, invalidation).await;
        if read.element("Status") == "Completed" {
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
    panic!(
        "case {case:?}: CloudFront never reported the invalidation {invalidation} of \
         {distribution} Completed within {POLL_DEADLINE:?}"
    );
}

/// Submit one invalidation batch naming `paths` and return once CloudFront reports it
/// finished — asserting on the way that it was accepted UNFINISHED, which is what makes the
/// completion the thing that decided the cache.
async fn completed_invalidation(
    router: &MultiServiceRouter,
    case: &str,
    distribution: &str,
    caller_reference: &str,
    paths: &[&str],
) {
    let created = create_invalidation(router, distribution, caller_reference, paths).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateInvalidation({paths:?}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    assert_ne!(
        created.element("Status"),
        "Completed",
        "case {case:?}: an invalidation is asynchronous, so the create cannot report \
         {paths:?} finished: {:?}",
        created.text()
    );
    let invalidation = created.element("Id");
    assert!(
        !invalidation.is_empty(),
        "case {case:?}: the created invalidation reports no Id: {:?}",
        created.text()
    );
    awaited_completion(router, case, distribution, &invalidation).await;
}

/// Make one viewer request and assert the `(status, bytes, X-Cache)` it was answered with.
///
/// `leg` names which request of the row's sequence this is, so a failure says WHERE the
/// sequence diverged rather than only that it did.
async fn assert_served(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    domain: &str,
    path: &str,
    body: &str,
    x_cache: &str,
) {
    let answer = view(router, domain, path).await;
    assert_eq!(
        (
            answer.status,
            answer.text().as_str(),
            answer.header("x-cache").as_str()
        ),
        (200, body, x_cache),
        "case {case:?} ({leg}): expected (200, {body:?}, X-Cache {x_cache:?}), got ({}, {:?}, \
         X-Cache {:?})",
        answer.status,
        answer.text(),
        answer.header("x-cache")
    );
}

/// One row: a private bucket holding one object written three times, a distribution over it
/// behind an origin access identity, and the two invalidation batches submitted — the first
/// after the second write, the second after the third.
struct Row {
    case: &'static str,
    /// The bucket, this row's own so one row's cache cannot answer another's request.
    bucket: &'static str,
    /// The object's key. A row dialling the distribution's root names
    /// [`DEFAULT_ROOT_OBJECT`], which is the object that path is an origin request for.
    key: &'static str,
    /// The path a viewer dials: the object's own, or `/` for the default root object.
    viewer_path: &'static str,
    /// The three revisions written at the origin, in order.
    first: &'static str,
    second: &'static str,
    third: &'static str,
    /// The paths the invalidation submitted after the SECOND write names.
    first_batch: &'static [&'static str],
    /// The paths the invalidation submitted after the THIRD write names.
    second_batch: &'static [&'static str],
    /// How many successive viewer requests are made while the third write has no invalidation
    /// of its own — each of which must still be the cached second revision.
    uninvalidated_requests: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "a wildcard over the whole distribution, then the object's exact path",
        bucket: "invalidate-once-wildcard-then-exact",
        key: "index.html",
        viewer_path: "/index.html",
        first: "<p>revision one</p>",
        second: "<p>revision two</p>",
        third: "<p>revision three</p>",
        first_batch: &["/*"],
        second_batch: &["/index.html"],
        uninvalidated_requests: 3,
    },
    Row {
        case: "the default root object dialled at the distribution's root",
        bucket: "invalidate-once-root-object",
        key: "index.html",
        viewer_path: "/",
        first: "the root, first",
        second: "the root, second",
        third: "the root, third",
        first_batch: &["/*"],
        second_batch: &["/*"],
        uninvalidated_requests: 1,
    },
    Row {
        case: "a nested key, invalidated by its own prefix then by a batch of several paths",
        bucket: "invalidate-once-nested-key",
        key: "assets/site/app.js",
        viewer_path: "/assets/site/app.js",
        first: "export const build = 1;",
        second: "export const build = 2;",
        third: "export const build = 3;",
        first_batch: &["/assets/*"],
        second_batch: &["/elsewhere.txt", "/assets/site/app.js"],
        uninvalidated_requests: 2,
    },
    Row {
        case: "the object's exact path both times, for a bare key",
        bucket: "invalidate-once-exact-twice",
        key: "page.txt",
        viewer_path: "/page.txt",
        first: "page: first",
        second: "page: second",
        third: "page: third",
        first_batch: &["/page.txt"],
        second_batch: &["/page.txt"],
        uninvalidated_requests: 5,
    },
];

#[tokio::test]
async fn one_invalidation_evicts_once_and_the_edge_caches_the_object_it_refetched() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope without a
    // computed signature. The VIEWER plane is the authenticating one, which is what makes the
    // credential CloudFront puts on its origin request the thing S3 serves the fetch for.
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        create_bucket(&setup, case, row.bucket).await;
        put_object(&setup, case, row.bucket, row.key, row.first, SSE_S3).await;
        let (identity, _canonical) =
            create_origin_access_identity(&setup, case, &format!("invalidate-once-oai-{index}"))
                .await;
        let (id, domain) = create_distribution(
            &setup,
            case,
            &format!("invalidate-once-reference-{index}"),
            row.bucket,
            &identity,
        )
        .await;
        // The bucket is private: this document is the only grant of the object, and it names
        // the distribution's own identity, so the edge is the one path to the bytes.
        put_bucket_policy(
            &setup,
            case,
            row.bucket,
            &identity_policy(
                "AWS",
                &identity_principal(&identity),
                &format!("arn:aws:s3:::{}/*", row.bucket),
            ),
        )
        .await;
        awaited_deployment(&setup, case, &id).await;

        // The first viewer request is the ORIGIN's answer: nothing is cached yet. The second is
        // the cache's, which is what the rest of the row is measured against.
        assert_served(
            &viewers,
            case,
            "the first request",
            &domain,
            row.viewer_path,
            row.first,
            MISS,
        )
        .await;
        assert_served(
            &viewers,
            case,
            "the second request",
            &domain,
            row.viewer_path,
            row.first,
            HIT,
        )
        .await;

        // A write at the origin changes nothing a viewer sees; the completed invalidation is
        // what evicts, and the request after it is the origin's again — with the NEW bytes.
        put_object(&setup, case, row.bucket, row.key, row.second, SSE_S3).await;
        completed_invalidation(
            &setup,
            case,
            &id,
            &format!("invalidate-once-first-batch-{index}"),
            row.first_batch,
        )
        .await;
        assert_served(
            &viewers,
            case,
            "the request after the first invalidation completed",
            &domain,
            row.viewer_path,
            row.second,
            MISS,
        )
        .await;

        // The half this suite exists for: that eviction happened ONCE, and the object the edge
        // refetched is cached again — so the very next request is the cache's.
        assert_served(
            &viewers,
            case,
            "the request after the refetch",
            &domain,
            row.viewer_path,
            row.second,
            HIT,
        )
        .await;

        // And so a THIRD write that no invalidation names is seen by no viewer, however many
        // times one asks: each request is still the re-cached second revision.
        put_object(&setup, case, row.bucket, row.key, row.third, SSE_S3).await;
        for request in 1..=row.uninvalidated_requests {
            assert_served(
                &viewers,
                case,
                &format!(
                    "request {request} of {} with no invalidation of the third write",
                    row.uninvalidated_requests
                ),
                &domain,
                row.viewer_path,
                row.second,
                HIT,
            )
            .await;
        }

        // Only a SECOND invalidation surfaces it, and what it surfaces is cached in its turn.
        completed_invalidation(
            &setup,
            case,
            &id,
            &format!("invalidate-once-second-batch-{index}"),
            row.second_batch,
        )
        .await;
        assert_served(
            &viewers,
            case,
            "the request after the second invalidation completed",
            &domain,
            row.viewer_path,
            row.third,
            MISS,
        )
        .await;
        assert_served(
            &viewers,
            case,
            "the request after the second refetch",
            &domain,
            row.viewer_path,
            row.third,
            HIT,
        )
        .await;
    }
}
