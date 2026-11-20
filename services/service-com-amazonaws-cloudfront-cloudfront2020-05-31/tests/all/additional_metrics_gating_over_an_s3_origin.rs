// SPDX-License-Identifier: CC0-1.0

//! The OPT-IN gate on the additional `AWS/CloudFront` series, read off distributions whose
//! traffic is served out of a private Amazon S3 ORIGIN.
//!
//! RULE: **the additional per-distribution series — the cache hit rate, the origin latency and
//! the per-status-code error rates — are published for the traffic a distribution serves if and
//! only if that distribution's own monitoring subscription carries
//! `RealtimeMetricsSubscriptionConfig$RealtimeMetricsSubscriptionStatus` `Enabled`, whatever
//! KIND of origin produced the answers; the default series the same traffic fills are published
//! for every distribution regardless, so a distribution that is not opted in reports the
//! traffic it served while answering no datapoint, and listing no series, for any of the gated
//! ones.**
//!
//! The sibling suites each pin one half of that: `edge_additional_metrics_over_an_s3_origin`
//! reconciles the gated series' VALUES against the outcomes an object-store origin produced,
//! but subscribes every one of its distributions, so it can say nothing about the gate; and
//! `additional_metrics_gating`, `edge_additional_metrics` and `dialled_edge_address_metrics`
//! drive the gate over an origin this service answers ITSELF, so they can say nothing about a
//! distribution whose answers come back from another service. A client that fronts a bucket,
//! buys the subscription and then builds a dashboard on `CacheHitRate`, `OriginLatency` and
//! `403ErrorRate` depends on BOTH halves at once: the series have to exist for the subscribed
//! distribution and be absent for an identical one that is not subscribed, over the origin the
//! client actually fronts.
//!
//! The rows span what the gate's input generalises over: a distribution carrying no monitoring
//! subscription at all, and one carrying a subscription for each value the model's enumerated
//! type takes (`REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES`, which is where the rows' flags
//! come from rather than from a list restated here) — so `Disabled`, a value the reported
//! symptom never exercised, is a row of its own. Every row's distribution fronts the SAME
//! bucket under the same cache policy and is driven with the SAME traffic, so the only thing
//! that differs between what two rows report is the opt-in.
//!
//! No row states a hit, a sample count or a rate of its own: every expectation is derived from
//! the status and the `X-Cache` the edge answered each of that row's viewer requests with.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

// The readers, the viewer harness and the object-store origin the sibling suites already own:
// one definition of a `GetMetricStatistics` read, of the `ListMetrics` filter, of what the edge
// answered one viewer request with, and of the bucket a distribution fronts — so the suites over
// these series cannot drift apart.
use crate::additional_metrics_gating::{basic_statistics, datapoints, listed};
use crate::additional_metrics_opt_in_boundary::subscribe;
use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_additional_metric_reconciliation::{
    viewed, Outcome, CACHE_ERROR, CACHE_HIT, CACHE_MISS,
};
use crate::edge_additional_metrics_over_an_s3_origin::{
    created, given_origin_access_control, given_origin_bucket, mounted, ABSENT_KEY, PRESENT_KEY,
};
use crate::edge_request_metrics::{held, now_secs};

/// The `RealtimeMetricsSubscriptionStatus` value that BUYS the additional series — "a flag that
/// indicates whether additional CloudWatch metrics are enabled for a given CloudFront
/// distribution" — as the enumerated type spells it. Asserted below to be one of the modelled
/// values, so this suite cannot drive a flag the model does not have.
const ENABLED: &str = "Enabled";

/// How close a folded percentage has to be to the share it is a percentage of: both sides are
/// computed in floating point out of the same whole counts, so the slack only absorbs the last
/// bit of a division.
const TOLERANCE: f64 = 1e-9;

/// What the opt-in of one row's distribution reads while its traffic is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptIn {
    /// The distribution carries no monitoring subscription at all.
    Unsubscribed,
    /// It carries one whose `RealtimeMetricsSubscriptionStatus` reads this value.
    Subscribed(&'static str),
}

impl OptIn {
    /// Whether the additional series are bought while the opt-in reads this.
    fn publishes_additional(self) -> bool {
        self == OptIn::Subscribed(ENABLED)
    }

    /// A caller reference and a case name that say which row this is.
    fn slug(self) -> String {
        match self {
            OptIn::Unsubscribed => "unsubscribed".to_string(),
            OptIn::Subscribed(flag) => flag.to_ascii_lowercase(),
        }
    }
}

/// The rows: no subscription at all, beside one carrying each value the modelled enumerated
/// type takes.
fn rows() -> Vec<OptIn> {
    let mut states = vec![OptIn::Unsubscribed];
    states.extend(
        REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES
            .iter()
            .copied()
            .map(OptIn::Subscribed),
    );
    states
}

/// One viewer request every row drives, and the status the ORIGIN (or CloudFront) answers it
/// with — a check that the row drove the traffic it meant to, never where an expectation comes
/// from.
struct Request {
    method: &'static str,
    key: &'static str,
    status: u16,
}

/// The traffic every row's distribution is driven with: a read the origin bucket answers, the
/// same read again (which the edge cache holds, so the hit rate has a hit), a read of a key the
/// bucket does not hold (the ORIGIN's own not-found, which is no cacheable request), and a
/// method the cache behaviour does not allow, which CloudFront refuses itself with no origin
/// request at all.
fn traffic() -> [Request; 4] {
    [
        Request {
            method: "GET",
            key: PRESENT_KEY,
            status: 200,
        },
        Request {
            method: "GET",
            key: PRESENT_KEY,
            status: 200,
        },
        Request {
            method: "GET",
            key: ABSENT_KEY,
            status: 404,
        },
        Request {
            method: "DELETE",
            key: PRESENT_KEY,
            status: 403,
        },
    ]
}

/// `GetMonitoringSubscription` for `id`: what the control plane reports the opt-in of one row's
/// distribution as, which is the difference between the rows.
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
        OptIn::Subscribed(flag) => {
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected HTTP 200 from GetMonitoringSubscription, got {}: {}",
                read.status, read.raw
            );
            assert_eq!(
                read.root().find_text("RealtimeMetricsSubscriptionStatus"),
                Some(flag),
                "case {case:?}: expected the opt-in to read {flag:?}, got {}",
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
        }
    }
}

/// Assert that `metric` holds NOTHING for the distribution `id`: no datapoint in any minute of
/// the window, and no series listed at all.
async fn assert_withheld(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    metric: &str,
    at: f64,
) {
    let published = datapoints(router, case, id, metric, at, basic_statistics()).await;
    assert!(
        published.is_empty(),
        "case {case:?} ({metric}): a distribution whose opt-in is not enabled publishes this \
         series in no minute, got {published:?}"
    );
    let series = listed(router, case, id, metric).await;
    assert!(
        series.is_empty(),
        "case {case:?} ({metric}): a series nothing published is listed nowhere, got {series:?}"
    );
}

#[tokio::test]
async fn the_opt_in_gates_the_additional_series_of_an_object_store_origin_distribution() {
    // The flag this suite calls the buying one is a value of the modelled enumerated type.
    assert!(
        REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES.contains(&ENABLED),
        "expected {ENABLED:?} to be one of the modelled values, which are {:?}",
        REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES
    );

    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let setup = mounted(&engine, false);
    let viewers = mounted(&engine, true);

    given_origin_bucket(&setup).await;
    let control = given_origin_access_control(&setup, "gating-metrics-s3").await;

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let table = rows();
    let mut subjects: Vec<(String, String)> = Vec::new();
    for opt_in in &table {
        let slug = opt_in.slug();
        subjects.push(
            created(
                &setup,
                &slug,
                &format!("gating-metrics-s3-{slug}"),
                &control,
            )
            .await,
        );
    }

    for (opt_in, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = opt_in.slug();
        let case = case.as_str();
        awaited_deployment(&setup, case, id).await;
        if let OptIn::Subscribed(flag) = opt_in {
            subscribe(&setup, case, id, flag).await;
        }
        assert_opt_in_reads(&setup, case, id, *opt_in).await;

        // Drive the row's traffic and RECORD what the edge answered each request with.
        let at = now_secs();
        let mut outcomes: Vec<Outcome> = Vec::new();
        for request in traffic() {
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

        // What those recorded answers say the series must hold.
        let served = outcomes.len() as f64;
        let cacheable = outcomes.iter().filter(|o| o.cacheable()).count() as f64;
        let hits = outcomes
            .iter()
            .filter(|o| o.cacheable() && o.served_by_cache())
            .count() as f64;
        let origin_answered = outcomes.iter().filter(|o| o.served_by_origin()).count() as f64;
        let client_errors = outcomes
            .iter()
            .filter(|o| (400..500).contains(&o.status))
            .count() as f64;

        // The traffic really exercised the classes the gated series measure, whatever the row's
        // opt-in: the object store answered a read, the edge cache answered its repeat, and
        // CloudFront refused one request itself.
        assert!(
            hits > 0.0 && origin_answered > 0.0 && client_errors > 0.0,
            "case {case:?}: this traffic must span a cache hit, an origin answer and an error, \
             got {outcomes:?}"
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
                "case {case:?} ({metric}): a default series is listed whatever the opt-in says, \
                 got {series:?}"
            );
        }

        // The ADDITIONAL series: nothing at all for a row the opt-in does not buy them for.
        if !opt_in.publishes_additional() {
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
        assert!(
            latency.sum >= 0.0 && latency.sum.is_finite(),
            "case {case:?}: every published span is a measured one, got {latency:?}"
        );
        assert_eq!(
            latency.units,
            vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
            "case {case:?}: an origin latency is a span of time, got {latency:?}"
        );

        // One series per status code CloudFront reports a rate of its own for — including
        // `403ErrorRate`, which this traffic fills with CloudFront's own refusal, and the codes
        // the traffic never produced.
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
