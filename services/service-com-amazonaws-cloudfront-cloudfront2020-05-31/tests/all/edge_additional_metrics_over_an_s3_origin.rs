// SPDX-License-Identifier: CC0-1.0

//! The ADDITIONAL `AWS/CloudFront` series a monitoring subscription buys, reconciled against
//! the edge outcomes of viewer traffic served out of a PRIVATE Amazon S3 ORIGIN — CloudFront,
//! the object store the origin lives in and CloudWatch all mounted on one engine.
//!
//! RULE: **while a distribution's monitoring subscription enables the additional metrics, every
//! answer that distribution's edge serves observes them, whatever KIND of origin produced it:
//! `CacheHitRate` is the percentage of the distribution's CACHEABLE answers the edge cache
//! produced (an answer of an error class, and one CloudFront produced itself, is no cacheable
//! request and observes the rate not at all), `OriginLatency` carries one measured span per
//! answer an ORIGIN produced, each per-status-code rate is the percentage of ALL that
//! distribution's answers that carried that code, and the default `Requests` counter counts
//! every one of them.**
//!
//! The sibling `edge_additional_metric_reconciliation` suite pins those values for an origin
//! this service serves itself, and `edge_additional_metrics` pins WHICH distributions publish
//! the series at all. Neither can say that a distribution whose origin is an object store —
//! the origin a client actually fronts, reached by a signed origin request the bucket's own
//! policy has to admit, and the origin whose own not-found answer is what fills a
//! per-status-code rate — fills those series too: the series are published by the edge's
//! per-request outcome recording, so an origin fetch that goes out to another service and
//! comes back has to end in the same observations as one this service answers.
//!
//! Every expected value here is DERIVED from what the edge answered each viewer request with:
//! each answer's status and its `X-Cache` is recorded — `Hit from cloudfront` for one the edge
//! cache held, `Miss from cloudfront` for one an origin produced, `Error from cloudfront` for
//! one CloudFront produced itself — and the series are reconciled against those recorded
//! outcomes. No row states a hit count or a rate of its own.
//!
//! The rows span what the rule generalises over: how many answers of each outcome class the
//! traffic produced — so the hit rate takes a different value in each row (three hits in four
//! beside two origin not-founds, an even split, and a row with no cacheable request at all,
//! whose hit rate is therefore not published rather than published as zero) — and which status
//! codes the answers carried, so a per-status rate is read for codes the traffic filled (the
//! origin's own not-found and CloudFront's own refusal of a method the behaviour does not
//! allow) and for codes it did not.
//!
//! Every viewer request goes through a router with request authentication ON, which is what
//! makes the origin half real: a viewer presents no credential, so the only thing that can make
//! the object store answer the origin fetch is the signature the edge puts on it for the
//! CloudFront service principal. The setup calls go through a second router over the SAME
//! engine with authentication off, so each carries a credential scope without a computed
//! signature.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_additional_metric_reconciliation::{
    viewed, Outcome, CACHE_ERROR, CACHE_HIT, CACHE_MISS,
};
use crate::edge_request_metrics::{held, now_secs};

/// The Region the origin bucket is created in, and so the Region the S3 origin's `DomainName`
/// addresses it at.
const REGION: &str = "us-east-1";

/// The service principal an origin access control makes CloudFront's origin request as, and so
/// the principal the origin bucket's policy grants the read to.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// repeat of an identical read under it is answered by the edge cache, which is what a hit rate
/// is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The one bucket every row's distribution fronts: what differs between rows is the traffic,
/// not the origin.
const ORIGIN_BUCKET: &str = "edge-additional-metrics-origin";

/// The key the origin bucket HOLDS, and the bytes it holds there.
pub(crate) const PRESENT_KEY: &str = "present-1.txt";
const PRESENT_BODY: &str = "the object the edge serves";

/// A key the origin bucket does NOT hold, so the ORIGIN answers a read of it with its own
/// not-found.
pub(crate) const ABSENT_KEY: &str = "absent-never-written.txt";

/// How close a folded percentage has to be to the share it is a percentage of. Both sides are
/// computed in floating point out of the same whole counts, so the slack only has to absorb the
/// last bit of a division.
const TOLERANCE: f64 = 1e-9;

/// CloudFront, the object store its origin lives in, and CloudWatch over ONE engine, with
/// request authentication as `authenticated` says.
///
/// All three have to share the engine: the edge publishes into the CloudWatch store (a metric
/// has one home) and fetches the origin out of the object store, so only a router carrying all
/// of them can reconcile what a client reads against what the edge served.
pub(crate) fn mounted(
    engine: &Arc<engine::InMemoryEngine>,
    authenticated: bool,
) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticated);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// A bucket policy granting the CloudFront service principal the reads its signed origin
/// request makes: `s3:GetObject` on every object of `bucket`, and `s3:ListBucket` on the bucket
/// itself — which is what decides how the ORIGIN answers a read of a key it does not hold ("if
/// you have the `s3:ListBucket` permission on the bucket, Amazon S3 returns an HTTP status code
/// 404 Not Found error. If you don't have the `s3:ListBucket` permission, Amazon S3 returns an
/// HTTP status code 403 Access Denied error", `GetObject`'s own documentation), so this row's
/// absent key is answered by the origin's own not-found.
fn grant(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":[\"s3:GetObject\",\"s3:ListBucket\"],\
         \"Resource\":[\"arn:aws:s3:::{bucket}/*\",\"arn:aws:s3:::{bucket}\"]}}]}}"
    )
}

/// Create the origin bucket, put [`PRESENT_BODY`] at [`PRESENT_KEY`] in it and grant CloudFront
/// the read — through Amazon S3's OWN API over the in-process callback, so the object the edge
/// fetches is one a client really put there.
pub(crate) async fn given_origin_bucket(setup: &MultiServiceRouter) {
    let callback = EmbeddedEmulator::from_router(setup.clone());
    let s3 = service_common::s3_client::s3_client(&callback, REGION);
    s3.create_bucket()
        .bucket(ORIGIN_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("creating {ORIGIN_BUCKET} must succeed, got {error:?}"));
    s3.put_object()
        .bucket(ORIGIN_BUCKET)
        .key(PRESENT_KEY)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            PRESENT_BODY.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("putting {ORIGIN_BUCKET}/{PRESENT_KEY} must succeed, got {error:?}")
        });
    s3.put_bucket_policy()
        .bucket(ORIGIN_BUCKET)
        .policy(grant(ORIGIN_BUCKET))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("the policy of {ORIGIN_BUCKET} must be stored, got {error:?}")
        });
}

/// `CreateOriginAccessControl` for an `s3` origin CloudFront signs every request to, answering
/// the control's id.
pub(crate) async fn given_origin_access_control(setup: &MultiServiceRouter, name: &str) -> String {
    let created = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateOriginAccessControl answered {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created origin access control reports an id")
        .to_string()
}

/// The `DistributionConfig` every row serves through: the S3 origin under the origin access
/// control `control` and the caching policy a repeat is answered from the cache under, with a
/// default behaviour allowing only the reading methods — so a `DELETE` is refused by CloudFront
/// itself, before any origin request exists.
fn config(caller_reference: &str, control: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>3</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method><Method>OPTIONS</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>additional metrics over an s3 origin</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(ORIGIN_BUCKET)
    )
}

/// Create one row's distribution and answer its `(id, DomainName)`.
pub(crate) async fn created(
    setup: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    control: &str,
) -> (String, String) {
    let created = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config(caller_reference, control)),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateDistribution, got {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"))
}

/// Turn the additional metrics ON for `id`, which is what buys the series this suite reads.
async fn subscribe(setup: &MultiServiceRouter, case: &str, id: &str) {
    let answer = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        Some(&format!(
            "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <RealtimeMetricsSubscriptionConfig>\
             <RealtimeMetricsSubscriptionStatus>Enabled</RealtimeMetricsSubscriptionStatus>\
             </RealtimeMetricsSubscriptionConfig>\
             </MonitoringSubscription>"
        )),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from CreateMonitoringSubscription, got {}: {}",
        answer.status, answer.raw
    );
}

/// One viewer request a row drives: the method, the key, and the status the ORIGIN (or
/// CloudFront) answers it with — a check that the row drove the traffic it meant to, not what
/// the metric expectations are derived from.
struct Request {
    method: &'static str,
    key: &'static str,
    status: u16,
}

/// A read of the key the origin bucket holds, answered by the object store.
fn present() -> Request {
    Request {
        method: "GET",
        key: PRESENT_KEY,
        status: 200,
    }
}

/// A read of a key the origin bucket does not hold, answered by the ORIGIN's own not-found.
fn absent() -> Request {
    Request {
        method: "GET",
        key: ABSENT_KEY,
        status: 404,
    }
}

/// A method the cache behaviour does not allow, which CloudFront refuses itself with no origin
/// request at all.
fn refused() -> Request {
    Request {
        method: "DELETE",
        key: PRESENT_KEY,
        status: 403,
    }
}

/// One row: a distribution of its own over the shared origin, and the traffic driven through it.
struct Row {
    case: &'static str,
    requests: Vec<Request>,
    /// Whether this row's traffic REPEATS a read the edge cache can answer, so at least one of
    /// its answers has to have come from that cache. Asserted on its own, so a row goes on
    /// driving the outcome class its case names rather than reconciling against whatever the
    /// edge happened to do.
    repeats_a_read: bool,
    /// Whether this row's traffic contains a CACHEABLE request at all, which is what decides
    /// whether its hit rate is published or has no observation.
    has_a_cacheable_request: bool,
}

/// The rows, differing in how many answers of each outcome class the traffic produced and in
/// which status codes they carried.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a read and three repeats, beside two reads of a key the origin does not hold",
            requests: vec![
                present(),
                present(),
                present(),
                present(),
                absent(),
                absent(),
            ],
            repeats_a_read: true,
            has_a_cacheable_request: true,
        },
        Row {
            case: "a read and one repeat of it",
            requests: vec![present(), present()],
            repeats_a_read: true,
            has_a_cacheable_request: true,
        },
        Row {
            case: "nothing cacheable: a read of an absent key and a refused method",
            requests: vec![absent(), refused()],
            repeats_a_read: false,
            has_a_cacheable_request: false,
        },
    ]
}

#[tokio::test]
async fn the_additional_series_reconcile_with_the_outcomes_an_s3_origin_edge_served() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let setup = mounted(&engine, false);
    let viewers = mounted(&engine, true);

    given_origin_bucket(&setup).await;
    let control = given_origin_access_control(&setup, "additional-metrics-s3").await;

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let table = rows();
    let mut subjects: Vec<(String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(
            created(
                &setup,
                row.case,
                &format!("additional-metrics-s3-{index}"),
                &control,
            )
            .await,
        );
    }

    for (row, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        awaited_deployment(&setup, case, id).await;
        subscribe(&setup, case, id).await;

        // Drive the row's traffic and RECORD what the edge answered each request with.
        let at = now_secs();
        let mut outcomes: Vec<Outcome> = Vec::new();
        for request in &row.requests {
            let outcome = viewed(
                &viewers,
                request.method,
                domain,
                &format!("/{}", request.key),
            )
            .await;
            assert_eq!(
                outcome.status, request.status,
                "case {case:?}: expected the edge to answer {} /{} with {}, got {outcome:?}",
                request.method, request.key, request.status
            );
            assert!(
                [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&outcome.x_cache.as_str()),
                "case {case:?}: every answer reports who produced it, got {outcome:?}"
            );
            outcomes.push(outcome);
        }

        // What those recorded outcomes say each series must hold.
        let served = outcomes.len() as f64;
        let cacheable = outcomes.iter().filter(|o| o.cacheable()).count() as f64;
        let hits = outcomes
            .iter()
            .filter(|o| o.cacheable() && o.served_by_cache())
            .count() as f64;
        let origin_answered = outcomes.iter().filter(|o| o.served_by_origin()).count() as f64;

        // Each row goes on driving the outcome classes its case names: one that repeats a read
        // is answered from the CACHE at least once, one that repeats none from no cache at all,
        // and a row with no cacheable request has none however the edge answered it.
        let from_cache = outcomes.iter().filter(|o| o.served_by_cache()).count();
        assert_eq!(
            from_cache > 0,
            row.repeats_a_read,
            "case {case:?}: expected an answer from the edge cache = {}, got {from_cache} of \
             {outcomes:?}",
            row.repeats_a_read
        );
        assert_eq!(
            cacheable > 0.0,
            row.has_a_cacheable_request,
            "case {case:?}: expected a cacheable request = {}, got {cacheable} of {outcomes:?}",
            row.has_a_cacheable_request
        );
        assert!(
            outcomes
                .first()
                .is_some_and(|first| first.served_by_origin()),
            "case {case:?}: the first read of a key is answered by the origin, got {outcomes:?}"
        );
        // The object store really served the fetch: the first answer of a row that reads the
        // present key carries the bytes a client put there, so the origin half is the object
        // store's rather than a page CloudFront produced.
        if row.has_a_cacheable_request {
            assert!(
                outcomes.iter().any(|o| o.status == 200),
                "case {case:?}: the origin bucket must have served the present key, got \
                 {outcomes:?}"
            );
        }

        // The default counter first: it is what says the traffic was served at all, so a
        // reconciliation of the additional series stands on it.
        let counted = held(&setup, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, served,
            "case {case:?}: every answer the edge served is counted once, got {counted:?}"
        );

        // `CacheHitRate`: the share of the CACHEABLE answers the edge cache produced — and no
        // observation at all from a row that had no cacheable request, rather than a rate of
        // zero.
        let hit_rate = held(&setup, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
        assert_eq!(
            hit_rate.samples, cacheable,
            "case {case:?}: the hit rate is observed by the {cacheable} cacheable answers of \
             these {served}, got {hit_rate:?}"
        );
        if cacheable > 0.0 {
            let expected = 100.0 * hits / cacheable;
            assert!(
                (hit_rate.average() - expected).abs() < TOLERANCE,
                "case {case:?}: the edge cache produced {hits} of the {cacheable} cacheable \
                 answers, so the rate is {expected}, got {hit_rate:?}"
            );
        }

        // `OriginLatency`: one measured span per answer an ORIGIN produced, and none for an
        // answer the cache or CloudFront itself produced.
        let latency = held(&setup, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            latency.samples, origin_answered,
            "case {case:?}: an origin produced {origin_answered} of these {served} answers, so \
             that many spans are published, got {latency:?}"
        );
        assert!(
            latency.sum >= 0.0 && latency.sum.is_finite(),
            "case {case:?}: every published span is a measured one, got {latency:?}"
        );
        let expected_latency_units = if origin_answered > 0.0 {
            vec![edge_metrics::MILLISECONDS_UNIT.to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            latency.units, expected_latency_units,
            "case {case:?}: a latency is published in {}, got {latency:?}",
            edge_metrics::MILLISECONDS_UNIT
        );

        // One series per status code CloudFront reports a rate for: each the share of ALL this
        // distribution's answers that carried that code, including the codes this row's traffic
        // never produced.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let answered_with = outcomes.iter().filter(|o| o.status == code).count() as f64;
            let expected = 100.0 * answered_with / served;
            let rate = held(&setup, case, id, metric, at).await;
            assert_eq!(
                rate.samples, served,
                "case {case:?} ({metric}): every answer observes a per-status rate, got {rate:?}"
            );
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): {answered_with} of the {served} answers carried \
                 {code}, so the rate is {expected}, got {rate:?}"
            );
        }
    }
}
