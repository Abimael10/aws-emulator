// SPDX-License-Identifier: CC0-1.0

//! The monitoring-subscription gate on the additional `AWS/CloudFront` series, read off TWIN
//! distributions over a private Amazon S3 origin whose grant decides which 4xx the origin
//! answers with.
//!
//! RULE: **a distribution that serves viewer traffic publishes its DEFAULT per-distribution
//! `AWS/CloudFront` series — dimensioned by its own `DistributionId` beside `Region=Global` —
//! for every answer its edge served, whatever status the origin produced; the ADDITIONAL series
//! (`CacheHitRate`, `OriginLatency` and the per-status-code rates) are published, and listed, for
//! that same traffic if and only if that distribution's own monitoring subscription carries
//! `RealtimeMetricsSubscriptionConfig$RealtimeMetricsSubscriptionStatus` `Enabled`, so one
//! distribution's opt-in buys nothing for an identical twin that has none.**
//!
//! The sibling suites hold the ORIGIN's answers fixed: `additional_metrics_gating_over_an_s3_origin`
//! drives the gate over a bucket whose policy grants the CloudFront service principal both
//! `s3:GetObject` and `s3:ListBucket`, so a read of a key the bucket does not hold is always
//! answered by the origin's own not-found, and the suites over an origin this service answers
//! itself never reach an object store at all. A client that fronts a bucket with the grant AWS's
//! own console writes — `s3:GetObject` on the objects and nothing on the bucket — gets a
//! DIFFERENT 4xx for that same read: "if you have the `s3:ListBucket` permission on the bucket,
//! Amazon S3 returns an HTTP status code 404 Not Found error. If you don't have the
//! `s3:ListBucket` permission, Amazon S3 returns an HTTP status code 403 Access Denied error"
//! (`GetObject`'s own documentation, which is where this suite's two grants and the status each
//! one's absent key is answered with come from). That status is what the gated `403ErrorRate`
//! and `404ErrorRate` series are shares OF, so it is the input the gate has to be shown to be
//! independent of.
//!
//! The rows span that: each GRANT crossed with each OPT-IN — a distribution carrying an enabled
//! subscription and an identical twin carrying none — all four fronting a bucket holding the same
//! object under the same cache policy and driven with the SAME traffic, so the only thing that
//! differs between what two rows of one grant report is the opt-in.
//!
//! No row states a hit, a sample count or a rate of its own: every expectation is derived from
//! the status and the `X-Cache` the edge answered each of that row's viewer requests with.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

// The readers, the viewer harness, the subscription call and the object-store mount the sibling
// suites already own: one definition of a `GetMetricStatistics` read, of the `ListMetrics`
// filter, of what the edge answered one viewer request with, and of the emulator all three
// services are mounted on — so the suites over these series cannot drift apart.
use crate::additional_metrics_gating::{basic_statistics, datapoints, listed};
use crate::additional_metrics_opt_in_boundary::subscribe;
use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_additional_metric_reconciliation::{Outcome, CACHE_ERROR, CACHE_HIT, CACHE_MISS};
use crate::edge_additional_metrics_over_an_s3_origin::{
    given_origin_access_control, mounted, PRESENT_KEY,
};
use crate::edge_request_metrics::{held, now_secs};

/// The Region the origin buckets are created in, and so the Region an S3 origin's `DomainName`
/// addresses them at.
const REGION: &str = "us-east-1";

/// The service principal an origin access control makes CloudFront's origin request as, and so
/// the principal an origin bucket's policy grants the read to.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// repeat of an identical read under it is answered by the edge cache, which is what a hit rate
/// is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The bytes every row's bucket holds at the key it holds, so a 200 the edge serves can be shown
/// to carry what the ORIGIN holds rather than a page CloudFront produced.
const PRESENT_BODY: &str = "the object the read-only origin serves";

/// A key no row's bucket holds, so a read of it is answered by the ORIGIN — with the status that
/// bucket's grant leaves it.
const ABSENT_KEY: &str = "never-written-by-anyone.txt";

/// The `RealtimeMetricsSubscriptionStatus` value that BUYS the additional series, as the
/// enumerated type spells it. Asserted below to be one of the modelled values, so this suite
/// cannot drive a flag the model does not have.
const ENABLED: &str = "Enabled";

/// The message `NoSuchMonitoringSubscription` carries for a distribution carrying no
/// subscription: fixed service text with no variable part.
const NO_SUCH_SUBSCRIPTION_MESSAGE: &str = "The monitoring subscription does not exist.";

/// How close a folded percentage has to be to the share it is a percentage of: both sides are
/// computed in floating point out of the same whole counts, so the slack only absorbs the last
/// bit of a division.
const TOLERANCE: f64 = 1e-9;

/// What a row's bucket policy grants the CloudFront service principal — which is what decides
/// the status the ORIGIN answers a read of a key the bucket does not hold with, per `GetObject`'s
/// own documented rule quoted in this suite's module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// `s3:GetObject` on the bucket's objects and nothing on the bucket itself, so a read of an
    /// absent key is refused rather than answered with a not-found.
    ReadOnly,
    /// `s3:GetObject` beside `s3:ListBucket`, so the origin answers an absent key with its own
    /// not-found.
    ReadAndList,
}

impl Grant {
    /// The bucket a row of this grant fronts: one bucket per grant, since a bucket carries one
    /// policy.
    fn bucket(self) -> &'static str {
        match self {
            Grant::ReadOnly => "read-only-oac-origin",
            Grant::ReadAndList => "read-and-list-oac-origin",
        }
    }

    /// The policy document granting exactly this.
    fn policy(self) -> String {
        let bucket = self.bucket();
        let (actions, resources) = match self {
            Grant::ReadOnly => (
                "[\"s3:GetObject\"]".to_string(),
                format!("[\"arn:aws:s3:::{bucket}/*\"]"),
            ),
            Grant::ReadAndList => (
                "[\"s3:GetObject\",\"s3:ListBucket\"]".to_string(),
                format!("[\"arn:aws:s3:::{bucket}/*\",\"arn:aws:s3:::{bucket}\"]"),
            ),
        };
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
             \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
             \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
             \"Action\":{actions},\"Resource\":{resources}}}]}}"
        )
    }

    /// The status the ORIGIN answers a read of [`ABSENT_KEY`] with under this grant.
    fn absent_key_status(self) -> u16 {
        match self {
            Grant::ReadOnly => 403,
            Grant::ReadAndList => 404,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Grant::ReadOnly => "read-only",
            Grant::ReadAndList => "read-and-list",
        }
    }
}

/// What a row's distribution reads for its monitoring subscription while its traffic is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptIn {
    /// It carries one whose `RealtimeMetricsSubscriptionStatus` reads [`ENABLED`].
    Subscribed,
    /// It carries no monitoring subscription at all.
    Unsubscribed,
}

impl OptIn {
    fn slug(self) -> &'static str {
        match self {
            OptIn::Subscribed => "subscribed",
            OptIn::Unsubscribed => "unsubscribed",
        }
    }
}

/// One row: a grant and an opt-in, which is the whole of what the rows differ in.
#[derive(Debug, Clone, Copy)]
struct Row {
    grant: Grant,
    opt_in: OptIn,
}

impl Row {
    /// A caller reference and a case name that say which row this is.
    fn slug(self) -> String {
        format!("{}-{}", self.grant.slug(), self.opt_in.slug())
    }
}

/// Each grant crossed with each opt-in.
fn rows() -> Vec<Row> {
    let mut table = Vec::new();
    for grant in [Grant::ReadOnly, Grant::ReadAndList] {
        for opt_in in [OptIn::Subscribed, OptIn::Unsubscribed] {
            table.push(Row { grant, opt_in });
        }
    }
    table
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// Create one grant's origin bucket, put [`PRESENT_BODY`] at [`PRESENT_KEY`] in it and store the
/// policy that grant describes — through Amazon S3's OWN API over the in-process callback, so the
/// object the edge fetches is one a client really put there.
async fn given_origin_bucket(setup: &MultiServiceRouter, grant: Grant) {
    let bucket = grant.bucket();
    let callback = EmbeddedEmulator::from_router(setup.clone());
    let s3 = service_common::s3_client::s3_client(&callback, REGION);
    s3.create_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
    s3.put_object()
        .bucket(bucket)
        .key(PRESENT_KEY)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            PRESENT_BODY.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("putting {bucket}/{PRESENT_KEY} must succeed, got {error:?}"));
    s3.put_bucket_policy()
        .bucket(bucket)
        .policy(grant.policy())
        .send()
        .await
        .unwrap_or_else(|error| panic!("the policy of {bucket} must be stored, got {error:?}"));
}

/// The `DistributionConfig` a row serves through: its grant's bucket as an S3 origin under the
/// origin access control `control` and the caching policy a repeat is answered from the cache
/// under, with a default behaviour allowing only the reading methods — so a `DELETE` is refused
/// by CloudFront itself, before any origin request exists.
fn config(caller_reference: &str, bucket: &str, control: &str) -> String {
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
         <Comment>additional metrics over a read-only s3 origin</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(bucket)
    )
}

/// Create one row's distribution and answer its `(id, DomainName)`.
async fn created(setup: &MultiServiceRouter, row: Row, control: &str) -> (String, String) {
    let case = row.slug();
    let created = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config(
            &format!("read-only-origin-metrics-{case}"),
            row.grant.bucket(),
            control,
        )),
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

/// What the edge served one viewer request: the outcome the sibling suites record, beside the
/// body, which is how the ORIGIN's bytes are told from a page CloudFront produced.
struct Served {
    outcome: Outcome,
    body: Vec<u8>,
}

/// One VIEWER request: no AWS credential, addressed at the name the distribution published,
/// exactly as a browser reaches a distribution.
async fn viewed(
    router: &MultiServiceRouter,
    method: &str,
    domain: &str,
    path: &str,
) -> Served {
    let request = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Served {
        outcome: Outcome { status, x_cache },
        body: response.into_body().to_vec(),
    }
}

/// One viewer request every row drives: the method and the key, the status the ORIGIN (or
/// CloudFront) answers it with, and WHO answers it — which of the two is a statement about the
/// topology the row built, not about the series: the first read of an object the bucket holds and
/// a read of one it does not are both answered by the object store, its repeat by the edge cache,
/// and a method the behaviour does not allow by CloudFront itself with no origin request at all.
struct Request {
    method: &'static str,
    key: &'static str,
    status: u16,
    /// Whether an ORIGIN produced this answer, which is what an origin latency is measured for.
    from_origin: bool,
    /// Whether this is one of the CACHEABLE requests the hit rate is a rate of, and if so
    /// whether the edge CACHE answered it — `None` for a request that is no cacheable request at
    /// all, which an answer of an error class and one CloudFront produced itself both are.
    cacheable: Option<bool>,
}

/// The traffic every row's distribution is driven with: a read the origin bucket answers, the
/// same read again (which the edge cache holds, so the hit rate has a hit), a read of a key the
/// bucket does not hold (answered by the ORIGIN with whatever its grant leaves it, which is no
/// cacheable request), and a method the cache behaviour does not allow, which CloudFront refuses
/// itself with no origin request at all.
fn traffic(grant: Grant) -> [Request; 4] {
    [
        Request {
            method: "GET",
            key: PRESENT_KEY,
            status: 200,
            from_origin: true,
            cacheable: Some(false),
        },
        Request {
            method: "GET",
            key: PRESENT_KEY,
            status: 200,
            from_origin: false,
            cacheable: Some(true),
        },
        Request {
            method: "GET",
            key: ABSENT_KEY,
            status: grant.absent_key_status(),
            from_origin: true,
            cacheable: None,
        },
        Request {
            method: "DELETE",
            key: PRESENT_KEY,
            status: 403,
            from_origin: false,
            cacheable: None,
        },
    ]
}

/// `GetMonitoringSubscription` for `id`: what the control plane reports the opt-in of one row's
/// distribution as, which is the difference between the rows of one grant.
async fn assert_opt_in_reads(router: &MultiServiceRouter, case: &str, id: &str, opt_in: OptIn) {
    let read = call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        None,
        None,
    )
    .await;
    match opt_in {
        OptIn::Subscribed => {
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected HTTP 200 from GetMonitoringSubscription, got {}: {}",
                read.status, read.raw
            );
            assert_eq!(
                read.root().find_text("RealtimeMetricsSubscriptionStatus"),
                Some(ENABLED),
                "case {case:?}: expected the opt-in to read {ENABLED:?}, got {}",
                read.raw
            );
        }
        OptIn::Unsubscribed => {
            assert_eq!(
                read.status, 404,
                "case {case:?}: a distribution carrying no subscription is refused, got {}: {}",
                read.status, read.raw
            );
            assert_eq!(
                read.error_code(),
                "NoSuchMonitoringSubscription",
                "case {case:?}: expected NoSuchMonitoringSubscription, got {}",
                read.raw
            );
            assert_eq!(
                read.root().find_text("Message"),
                Some(NO_SUCH_SUBSCRIPTION_MESSAGE),
                "case {case:?}: expected the refusal to carry {NO_SUCH_SUBSCRIPTION_MESSAGE:?}, \
                 got {}",
                read.raw
            );
        }
    }
}

/// Assert that `metric` holds NOTHING for the distribution `id`: no datapoint in any minute of
/// the window, and no series listed at all.
async fn assert_withheld(router: &MultiServiceRouter, case: &str, id: &str, metric: &str, at: f64) {
    let published = datapoints(router, case, id, metric, at, basic_statistics()).await;
    assert!(
        published.is_empty(),
        "case {case:?} ({metric}): a distribution carrying no subscription publishes this series \
         in no minute, got {published:?}"
    );
    let series = listed(router, case, id, metric).await;
    assert!(
        series.is_empty(),
        "case {case:?} ({metric}): a series nothing published is listed nowhere, got {series:?}"
    );
}

#[tokio::test]
async fn the_gate_holds_over_an_origin_whose_grant_decides_the_status_of_an_absent_key() {
    // The flag this suite calls the buying one is a value of the modelled enumerated type.
    assert!(
        REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES.contains(&ENABLED),
        "expected {ENABLED:?} to be one of the modelled values, which are {:?}",
        REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES
    );

    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let setup = mounted(&engine, false);
    let viewers = mounted(&engine, true);

    given_origin_bucket(&setup, Grant::ReadOnly).await;
    given_origin_bucket(&setup, Grant::ReadAndList).await;
    let control = given_origin_access_control(&setup, "read-only-origin-metrics").await;

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let table = rows();
    let mut subjects: Vec<(String, String)> = Vec::new();
    for row in &table {
        subjects.push(created(&setup, *row, &control).await);
    }

    for (row, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = row.slug();
        let case = case.as_str();
        awaited_deployment(&setup, case, id).await;
        if row.opt_in == OptIn::Subscribed {
            subscribe(&setup, case, id, ENABLED).await;
        }
        assert_opt_in_reads(&setup, case, id, row.opt_in).await;

        // Drive the row's traffic, asserting each answer is the one its request describes: the
        // status, the bytes a 200 carries, and the `X-Cache` that says who produced it.
        let at = now_secs();
        let driven = traffic(row.grant);
        let mut statuses: Vec<u16> = Vec::new();
        for request in &driven {
            let served = viewed(
                &viewers,
                request.method,
                domain,
                &format!("/{}", request.key),
            )
            .await;
            let outcome = served.outcome;
            assert_eq!(
                outcome.status, request.status,
                "case {case:?}: expected the edge to answer {} /{} with {}, got {outcome:?}",
                request.method, request.key, request.status
            );
            assert!(
                [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&outcome.x_cache.as_str()),
                "case {case:?}: every answer reports who produced it, got {outcome:?}"
            );
            if request.cacheable == Some(true) {
                assert!(
                    outcome.served_by_cache(),
                    "case {case:?}: the repeat of a read under this cache policy is answered by \
                     the edge CACHE, got {outcome:?}"
                );
            }
            if outcome.status == 200 {
                assert_eq!(
                    served.body,
                    PRESENT_BODY.as_bytes(),
                    "case {case:?}: a 200 the edge serves carries the bytes the ORIGIN holds, got \
                     {:?}",
                    String::from_utf8_lossy(&served.body)
                );
                if request.from_origin {
                    assert!(
                        outcome.served_by_origin(),
                        "case {case:?}: the first read of the object is answered by the ORIGIN, \
                         got {outcome:?}"
                    );
                }
            }
            statuses.push(outcome.status);
        }

        // What that traffic says the series must hold, in the terms each series is defined in.
        let served = driven.len() as f64;
        let cacheable = driven
            .iter()
            .filter(|request| request.cacheable.is_some())
            .count() as f64;
        let hits = driven
            .iter()
            .filter(|request| request.cacheable == Some(true))
            .count() as f64;
        let origin_answered = driven
            .iter()
            .filter(|request| request.from_origin)
            .count() as f64;
        let client_errors = statuses
            .iter()
            .filter(|status| (400..500).contains(*status))
            .count() as f64;

        // The traffic really exercised the classes the gated series measure, whatever the row's
        // grant and opt-in: the object store answered a read, the edge cache answered its repeat,
        // and the row's absent key was answered with the status its grant leaves it.
        assert!(
            hits > 0.0 && origin_answered > 0.0 && client_errors > 0.0,
            "case {case:?}: this traffic must span a cache hit, an origin answer and an error, \
             got {statuses:?}"
        );
        let absent_status = row.grant.absent_key_status();
        assert!(
            statuses.contains(&absent_status),
            "case {case:?}: the grant leaves an absent key answered {absent_status}, got \
             {statuses:?}"
        );

        // The DEFAULT series are published whatever the opt-in reads, and agree with what this
        // distribution served.
        let counted = held(&setup, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, served,
            "case {case:?}: every answer the edge served is counted once ({served}), got \
             {counted:?}"
        );
        let four_xx = held(
            &setup,
            case,
            id,
            edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
            at,
        )
        .await;
        let expected_four_xx = 100.0 * client_errors / served;
        assert!(
            (four_xx.average() - expected_four_xx).abs() < TOLERANCE,
            "case {case:?}: the default 4xx rate is the share of this traffic answered 4xx \
             ({expected_four_xx}), got {four_xx:?}"
        );
        for metric in edge_metrics::DISTRIBUTION_METRICS {
            let series = listed(&setup, case, id, metric).await;
            assert_eq!(
                series.len(),
                1,
                "case {case:?} ({metric}): a default series is listed under this distribution and \
                 the Region it is reported for whatever the opt-in says, got {series:?}"
            );
        }

        // The ADDITIONAL series: nothing at all for a row carrying no subscription, however its
        // twin of the same grant was driven.
        if row.opt_in == OptIn::Unsubscribed {
            for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
                assert_withheld(&setup, case, id, metric, at).await;
            }
            continue;
        }

        // `CacheHitRate`: the share of the CACHEABLE answers the edge cache produced.
        let hit_rate = held(&setup, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
        assert_eq!(
            hit_rate.samples, cacheable,
            "case {case:?}: the hit rate is observed by the {cacheable} cacheable answers of \
             these {served}, got {hit_rate:?}"
        );
        let expected_hit_rate = 100.0 * hits / cacheable;
        assert!(
            (hit_rate.average() - expected_hit_rate).abs() < TOLERANCE,
            "case {case:?}: the edge cache produced {hits} of the {cacheable} cacheable answers, \
             so the rate is {expected_hit_rate}, got {hit_rate:?}"
        );

        // `OriginLatency`: one measured span per answer an ORIGIN produced, in milliseconds.
        let latency = held(&setup, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            latency.samples, origin_answered,
            "case {case:?}: the origin bucket produced {origin_answered} of these {served} \
             answers, so that many spans are published, got {latency:?}"
        );
        assert_eq!(
            latency.units,
            vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
            "case {case:?}: an origin latency is a span of time, got {latency:?}"
        );

        // One series per status code CloudFront reports a rate of its own for — so the row whose
        // grant had the origin refuse the absent key fills `403ErrorRate` where its sibling grant
        // fills `404ErrorRate`.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let answered_with = statuses.iter().filter(|status| **status == code).count() as f64;
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

        // What a client can LIST agrees with what it can read, series by series.
        for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
            let published = datapoints(&setup, case, id, metric, at, basic_statistics()).await;
            let series = listed(&setup, case, id, metric).await;
            assert_eq!(
                series.is_empty(),
                published.is_empty(),
                "case {case:?} ({metric}): what a client lists must agree with what it can read, \
                 got listed {series:?} for datapoints {published:?}"
            );
            assert!(
                !published.is_empty(),
                "case {case:?} ({metric}): an enabled opt-in publishes this series for the \
                 traffic it served, got {published:?}"
            );
        }
    }
}
