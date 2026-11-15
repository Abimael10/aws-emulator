// SPDX-License-Identifier: CC0-1.0

//! The ADDITIONAL `AWS/CloudFront` series RECONCILED against the edge outcomes they are a
//! measure of, read back through `GetMetricData`.
//!
//! RULE: **while a distribution's monitoring subscription enables the additional metrics,
//! each additional series is computed from the answers that distribution's edge actually
//! served — `CacheHitRate` is the percentage of its CACHEABLE answers the edge cache
//! produced (an answer of an error class, and one CloudFront produced itself, is no cacheable
//! request and observes the rate not at all), `OriginLatency` carries one measured span per
//! answer an ORIGIN produced, and each per-status-code rate is the percentage of ALL that
//! distribution's answers that carried that code — and those datapoints have ONE home, so a
//! client folding them with `GetMetricData` is answered the same values as one folding them
//! with `GetMetricStatistics`.**
//!
//! Every expected value here is DERIVED from what the edge answered each viewer request with:
//! the suite records each answer's status and its `X-Cache` — `Hit from cloudfront` for one
//! the edge cache held, `Miss from cloudfront` for one an origin produced, `Error from
//! cloudfront` for one CloudFront produced itself — and reconciles the series against those
//! recorded outcomes. No row states a hit count or a rate of its own.
//!
//! The rows span what the rule generalises over: HOW MANY answers of each outcome class the
//! traffic produced, so the hit rate takes a different value in each row (a lone miss, an even
//! split, three hits in four, and a row with no cacheable request at all, whose hit rate is
//! therefore not published rather than published as zero), and WHICH status codes the answers
//! carried, so a per-status rate is read for a code the traffic filled and for codes it did
//! not.
//!
//! The sibling `edge_additional_metrics` suite pins WHICH distributions publish these series;
//! this one pins what their VALUES are and that every reader of them agrees.

use serde_json::{json, Value};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, create_distribution, API_VERSION, HOST};
use crate::edge_request_metrics::{cloudwatch, dimensions, emulator, held, now_secs, Held};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// repeat of an identical read under it is answered by the edge cache, which is what a hit
/// rate is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint, which answers
/// `GET /2020-05-31/distribution` with a `DistributionList` and a read of an id it holds no
/// distribution for with its own 404.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the origin HOLDS, answered 200.
const PRESENT_KEY: &str = "/2020-05-31/distribution";

/// A viewer path the origin does NOT hold, answered with its own 404.
const ABSENT_KEY: &str = "/2020-05-31/distribution/E0NOSUCHDISTRIB";

/// The `X-Cache` of an answer the edge cache held.
pub(crate) const CACHE_HIT: &str = "Hit from cloudfront";

/// The `X-Cache` of an answer an origin produced for this very request.
pub(crate) const CACHE_MISS: &str = "Miss from cloudfront";

/// The `X-Cache` of an answer CloudFront produced itself, reaching no origin.
pub(crate) const CACHE_ERROR: &str = "Error from cloudfront";

/// The period the series are folded at, in seconds: the granularity CloudFront reports a
/// distribution's traffic at.
const PERIOD: i64 = 60;

/// How far either side of the instant the traffic was served the fold reaches, in periods:
/// wide enough that every datapoint falls inside it whichever minute the suite runs in.
const WINDOW_PERIODS: f64 = 2.0;

/// How close a folded percentage has to be to the share it is a percentage of. The fold is
/// exact arithmetic over whole observations; the slack is only against the last bit of a
/// division.
const TOLERANCE: f64 = 1e-9;

/// The `DistributionConfig` every row serves through: one origin under a caching policy, and a
/// default behaviour allowing only the reading methods — so a `DELETE` is refused by
/// CloudFront itself, before any origin request exists.
fn config(caller_reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>3</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method><Method>OPTIONS</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>edge additional metric reconciliation</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one distribution for a row and answer its `(id, DomainName)`.
async fn created(router: &MultiServiceRouter, case: &str, caller_reference: &str) -> (String, String) {
    let created = create_distribution(router, &config(caller_reference)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateDistribution, got {}: {}",
        created.status, created.raw
    );
    let root = created.root();
    let id = root
        .find_text("Id")
        .expect("the created distribution reports an id")
        .to_string();
    let domain = root
        .find_text("DomainName")
        .expect("the created distribution reports a domain name")
        .to_string();
    (id, domain)
}

/// Turn the additional metrics ON for `id`, which is what buys the series this suite reads.
async fn subscribe(router: &MultiServiceRouter, case: &str, id: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>Enabled</RealtimeMetricsSubscriptionStatus>\
         </RealtimeMetricsSubscriptionConfig>\
         </MonitoringSubscription>"
    );
    let answer = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        Some(&document),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from CreateMonitoringSubscription, got {}: {}",
        answer.status, answer.raw
    );
}

/// What the edge answered one viewer request with, as this suite records it.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) status: u16,
    pub(crate) x_cache: String,
}

impl Outcome {
    /// Whether this answer is one of the CACHEABLE requests the hit rate is a rate of: a
    /// reading method answered below the error classes by the cache or an origin. An answer
    /// CloudFront produced itself came from neither, so it is not one.
    pub(crate) fn cacheable(&self) -> bool {
        self.status < 400 && self.x_cache != CACHE_ERROR
    }

    /// Whether the edge CACHE produced this answer.
    pub(crate) fn served_by_cache(&self) -> bool {
        self.x_cache == CACHE_HIT
    }

    /// Whether an ORIGIN produced this answer, which is what an origin latency is measured
    /// for.
    pub(crate) fn served_by_origin(&self) -> bool {
        self.x_cache == CACHE_MISS
    }
}

/// One VIEWER request: no AWS credential, addressed at the name the distribution published,
/// exactly as a browser reaches a distribution. Answers what the edge served it with.
pub(crate) async fn viewed(
    router: &MultiServiceRouter,
    method: &str,
    domain: &str,
    path: &str,
) -> Outcome {
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
    Outcome { status, x_cache }
}

/// One viewer request a row drives: the method, the path, and the status the ORIGIN (or
/// CloudFront) answers it with — which is not what the metric expectations are derived from,
/// only a check that the row drove the traffic it meant to.
struct Request {
    method: &'static str,
    path: &'static str,
    status: u16,
}

/// A read of the key the origin holds.
fn present() -> Request {
    Request {
        method: "GET",
        path: PRESENT_KEY,
        status: 200,
    }
}

/// A read of a key the origin does not hold.
fn absent() -> Request {
    Request {
        method: "GET",
        path: ABSENT_KEY,
        status: 404,
    }
}

/// A method the cache behaviour does not allow, which CloudFront refuses itself.
fn refused() -> Request {
    Request {
        method: "DELETE",
        path: PRESENT_KEY,
        status: 403,
    }
}

/// One row: a distribution of its own, the traffic driven through it, and nothing else.
struct Row {
    case: &'static str,
    requests: Vec<Request>,
    /// Whether this row's traffic REPEATS an identical read, so the edge cache has to have
    /// answered at least one of its requests. Asserted on its own, so a row goes on driving
    /// the outcome class its case names rather than reconciling against whatever the edge
    /// happened to do.
    repeats_a_read: bool,
}

/// The rows, differing in how many answers of each outcome class the traffic produced and in
/// which status codes they carried.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one read of a key the origin holds",
            requests: vec![present()],
            repeats_a_read: false,
        },
        Row {
            case: "a read and one repeat of it",
            requests: vec![present(), present()],
            repeats_a_read: true,
        },
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
        },
        Row {
            case: "nothing cacheable: two reads of an absent key and a refused method",
            requests: vec![absent(), absent(), refused()],
            repeats_a_read: true,
        },
    ]
}

/// Read one of a distribution's series through `GetMetricData`, asking for the two statistics
/// a fold needs, and add up what each period carries.
///
/// Answers the SAME [`Held`] the `GetMetricStatistics` reader beside it answers, so the two
/// readers of one series are folded by one definition and cannot drift. A datapoint's unit is
/// not a member of a `MetricDataResult`, so the unit half is left empty here and asserted
/// through the reader that does carry it.
async fn folded(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> Held {
    let span = WINDOW_PERIODS * PERIOD as f64;
    let series = json!({
        "Namespace": edge_metrics::NAMESPACE,
        "MetricName": metric,
        "Dimensions": dimensions(distribution_id),
    });
    let query = |id: &str, stat: &str| {
        json!({
            "Id": id,
            "MetricStat": {"Metric": series.clone(), "Period": PERIOD, "Stat": stat},
        })
    };
    let answered = cloudwatch(
        router,
        case,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [query("samples", "SampleCount"), query("total", "Sum")],
            "StartTime": at - span,
            "EndTime": at + span,
            "ScanBy": "TimestampAscending",
        }),
    )
    .await;
    let results = answered["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut held = Held::default();
    for result in &results {
        let values: f64 = result["Values"]
            .as_array()
            .map(|listed| listed.iter().filter_map(Value::as_f64).sum())
            .unwrap_or_default();
        match result["Id"].as_str().unwrap_or_default() {
            "samples" => held.samples = values,
            "total" => held.sum = values,
            other => panic!("case {case:?} ({metric}): GetMetricData answered an unasked query {other:?}: {answered}"),
        }
    }
    assert_eq!(
        results.len(),
        2,
        "case {case:?} ({metric}): a two-query request answers two results, got {answered}"
    );
    held
}

#[tokio::test]
async fn the_additional_series_reconcile_with_the_edge_outcomes_they_measure() {
    let router = emulator();
    let table = rows();

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let mut subjects: Vec<(String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(created(&router, row.case, &format!("edge-reconciled-{index}")).await);
    }

    for (row, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        awaited_deployment(&router, case, id).await;
        subscribe(&router, case, id).await;

        // Drive the row's traffic and RECORD what the edge answered each request with.
        let at = now_secs();
        let mut outcomes: Vec<Outcome> = Vec::new();
        for request in &row.requests {
            let outcome = viewed(&router, request.method, domain, request.path).await;
            assert_eq!(
                outcome.status, request.status,
                "case {case:?}: expected the edge to answer {} {} with {}, got {outcome:?}",
                request.method, request.path, request.status
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

        // A row that repeats a read has to have been answered from the CACHE at least once,
        // and one that repeats nothing from no cache at all: the reconciliation below is
        // against what the edge answered, so this is what keeps each row driving the outcome
        // class its case names.
        let from_cache = outcomes.iter().filter(|o| o.served_by_cache()).count();
        assert_eq!(
            from_cache > 0,
            row.repeats_a_read,
            "case {case:?}: expected an answer from the edge cache = {}, got {from_cache} of \
             {outcomes:?}",
            row.repeats_a_read
        );
        let first_read_went_to_the_origin = outcomes
            .first()
            .is_some_and(|first| first.served_by_origin());
        assert!(
            first_read_went_to_the_origin,
            "case {case:?}: the first read of a key is answered by the origin, got {outcomes:?}"
        );

        // The default counter first: it is what says the traffic was served at all, so a
        // reconciliation of the additional series stands on it.
        let counted = folded(&router, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, served,
            "case {case:?}: every answer the edge served is counted once, got {counted:?}"
        );

        // `CacheHitRate`: the share of the CACHEABLE answers the edge cache produced — and no
        // observation at all from a row that had no cacheable request, rather than a rate of
        // zero.
        let hit_rate = folded(&router, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
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
        let latency = folded(&router, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            latency.samples, origin_answered,
            "case {case:?}: an origin produced {origin_answered} of these {served} answers, so \
             that many spans are published, got {latency:?}"
        );
        assert!(
            latency.sum >= 0.0 && latency.sum.is_finite(),
            "case {case:?}: every published span is a measured one, got {latency:?}"
        );

        // One series per status code CloudFront reports a rate for: each the share of ALL the
        // answers that carried that code, including the codes this row's traffic never
        // produced.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let answered_with = outcomes.iter().filter(|o| o.status == code).count() as f64;
            let expected = 100.0 * answered_with / served;
            let rate = folded(&router, case, id, metric, at).await;
            assert_eq!(
                rate.samples, served,
                "case {case:?} ({metric}): every answer observes a per-status rate, got {rate:?}"
            );
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): {answered_with} of the {served} answers carried \
                 {code}, so the rate is {expected}, got {rate:?}"
            );

            // And the OTHER reader of the same datapoints answers the same fold: a series has
            // one home.
            let through_statistics = held(&router, case, id, metric, at).await;
            assert_eq!(
                through_statistics.samples, rate.samples,
                "case {case:?} ({metric}): GetMetricStatistics folds the same observations as \
                 GetMetricData, got {through_statistics:?} against {rate:?}"
            );
            assert!(
                (through_statistics.sum - rate.sum).abs() < TOLERANCE,
                "case {case:?} ({metric}): GetMetricStatistics folds the same observations as \
                 GetMetricData, got {through_statistics:?} against {rate:?}"
            );
            assert_eq!(
                through_statistics.units,
                vec!["Percent".to_string()],
                "case {case:?} ({metric}): a rate is a percentage of the period's requests, got \
                 {through_statistics:?}"
            );
        }

        // The two measured series read the same either way as well, and carry the units their
        // own definitions name.
        for (metric, unit) in [
            (edge_metrics::CACHE_HIT_RATE_METRIC, "Percent"),
            (
                edge_metrics::ORIGIN_LATENCY_METRIC,
                edge_metrics::MILLISECONDS_UNIT,
            ),
        ] {
            let through_data = folded(&router, case, id, metric, at).await;
            let through_statistics = held(&router, case, id, metric, at).await;
            assert_eq!(
                through_statistics.samples, through_data.samples,
                "case {case:?} ({metric}): both readers fold the same observations, got \
                 {through_statistics:?} against {through_data:?}"
            );
            assert!(
                (through_statistics.sum - through_data.sum).abs() < TOLERANCE,
                "case {case:?} ({metric}): both readers fold the same observations, got \
                 {through_statistics:?} against {through_data:?}"
            );
            let expected_units = if through_data.samples > 0.0 {
                vec![unit.to_string()]
            } else {
                Vec::new()
            };
            assert_eq!(
                through_statistics.units, expected_units,
                "case {case:?} ({metric}): expected the unit {unit} on every datapoint this \
                 series has, got {through_statistics:?}"
            );
        }
    }
}
