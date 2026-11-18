// SPDX-License-Identifier: CC0-1.0

//! The ADDITIONAL `AWS/CloudFront` series a monitoring subscription buys, read back for ONE
//! MINUTE of the viewer traffic a distribution's edge served out of a private Amazon S3
//! origin — CloudFront, the object store the origin lives in and CloudWatch over the whole
//! mounted service set.
//!
//! RULE: **while a distribution's monitoring subscription carries
//! `RealtimeMetricsSubscriptionStatus` `Enabled`, every answer its edge serves is observed in
//! the SAME minute-aligned 60-second period as the default series that counts it, so one
//! `GetMetricStatistics` read of a period answers, together, every series that period's
//! traffic filled: `Requests` (the number of answers served, published with no unit of
//! measure), the class and per-status-code error rates (the share of that period's answers
//! carrying each, in `Percent`), `CacheHitRate` (the share of that period's CACHEABLE answers
//! the edge cache produced, in `Percent`) and `OriginLatency` (one measured span per answer an
//! ORIGIN produced, in `Milliseconds`, answered at a percentile too) — each reconciled against
//! the outcomes the edge recorded for those answers.**
//!
//! Only a suite over the mounted service set can state that: the edge's per-request outcome
//! recording publishes into the CloudWatch store (a metric has one home) and fetches the origin
//! out of the object store, and a client watching a distribution reads one period of all of
//! those series at once — a dashboard panel and an alarm both ask for a period, so a series
//! that lands in a period of its own is a series that cannot be read beside the traffic that
//! produced it. The crate-local suites pin the neighbouring halves:
//! `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/tests/all/edge_additional_metrics_over_an_s3_origin.rs`
//! reconciles the same series folded over a whole window, and `additional_metrics_gating.rs`
//! pins WHICH distributions publish them at all.
//!
//! Every expected value here is DERIVED from what the edge answered each viewer request with:
//! the suite records each answer's status and its `X-Cache` — [`CACHE_HIT`] for one the edge
//! cache held, [`CACHE_MISS`] for one an origin produced, [`CACHE_ERROR`] for one CloudFront
//! produced itself — and reconciles the periods against those recorded outcomes. No row states
//! a count or a rate of its own.
//!
//! The rows differ along what the rule generalises over: how many answers of each outcome class
//! the minute carried, and so which series it fills and what value each takes — a read with
//! three repeats beside two reads of a key the origin refuses, a read and one repeat (an even
//! hit rate), and a minute of nothing but refused reads, whose hit rate is therefore not
//! published at all while its request counter and its rates are.
//!
//! Every viewer request goes through a router with request authentication ON, which is what
//! makes the origin half real: a viewer presents no credential, so the only thing that can make
//! the object store answer the origin fetch is the signature the edge puts on it for the
//! CloudFront service principal. The setup calls go through a second router over the SAME
//! engine with authentication off, so each carries a credential scope without a computed
//! signature.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;

use crate::cloudfront_edge_metrics_drive_the_alarm_chain::{deployed, emulator, now_secs};
use crate::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, create_distribution, create_origin_access_control, policy,
    put_bucket_policy, put_object, view, API_VERSION, CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
    REGION,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::cw;

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// repeat of an identical read under it is answered by the edge cache, which is what a hit rate
/// is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The one bucket every row's distribution fronts: what differs between rows is the traffic the
/// minute carried, not the origin.
const ORIGIN_BUCKET: &str = "subscribed-minute-origin";

/// The key the origin bucket HOLDS, and the bytes it holds there.
const PRESENT_KEY: &str = "present-1.txt";
const PRESENT_BODY: &str = "the object the edge serves";

/// A key the origin bucket does NOT hold. The bucket's policy grants the CloudFront service
/// principal `s3:GetObject` and nothing else, so the ORIGIN refuses a read of this key rather
/// than answering it not-found: "if you have the `s3:ListBucket` permission on the bucket,
/// Amazon S3 returns an HTTP status code 404 Not Found error. If you don't have the
/// `s3:ListBucket` permission, Amazon S3 returns an HTTP status code 403 Access Denied error"
/// (`GetObject`'s own documentation).
const ABSENT_KEY: &str = "absent-never-written.txt";

/// The status the origin refuses a read of [`ABSENT_KEY`] with, under that rule.
const ORIGIN_REFUSAL_STATUS: u16 = 403;

/// The `X-Cache` values CloudFront reports who produced an answer with.
const CACHE_HIT: &str = "Hit from cloudfront";
const CACHE_MISS: &str = "Miss from cloudfront";
const CACHE_ERROR: &str = "Error from cloudfront";

/// The period a distribution's traffic is reported at, in seconds: the granularity CloudFront
/// publishes these series at, and the period a client reads one minute of them with.
const PERIOD: i64 = 60;

/// How far either side of the instant the traffic was served a read reaches, in periods: wide
/// enough that every datapoint falls inside it whichever minute the suite runs in.
const WINDOW_PERIODS: i64 = 2;

/// The `(StartTime, EndTime)` a read of one minute's series spans, around the instant the
/// traffic was served.
#[allow(clippy::cast_precision_loss)]
fn window(at: f64) -> (f64, f64) {
    let span = (WINDOW_PERIODS * PERIOD) as f64;
    (at - span, at + span)
}

/// The percentile a dashboard reads a latency series at, named as `ExtendedStatistics` spells
/// one.
const PERCENTILE: &str = "p90";

/// How close a folded percentage has to be to the share it is a percentage of. Both sides are
/// computed in floating point out of the same whole counts, so the slack only has to absorb the
/// last bit of a division.
const TOLERANCE: f64 = 1e-9;

/// What the edge answered one viewer request with, as this suite records it.
#[derive(Debug)]
struct Outcome {
    status: u16,
    x_cache: String,
}

impl Outcome {
    /// Whether this answer is one of the CACHEABLE requests the hit rate is a rate of: a read
    /// answered below the error classes by the cache or an origin. An answer CloudFront produced
    /// itself came from neither, so it is not one.
    fn cacheable(&self) -> bool {
        self.status < 400 && self.x_cache != CACHE_ERROR
    }

    /// Whether the edge CACHE produced this answer.
    fn served_by_cache(&self) -> bool {
        self.x_cache == CACHE_HIT
    }

    /// Whether an ORIGIN produced this answer, which is what an origin latency is measured for.
    ///
    /// Read as "the edge cache did not hold it": `X-Cache` reports an answer of an error class
    /// as [`CACHE_ERROR`] whoever produced it, so the header alone does not separate an origin's
    /// own refusal from a page CloudFront generated. Every request this suite drives is a read
    /// the cache behaviour allows for an origin the object store serves, so an answer the cache
    /// did not hold is one that went to that origin.
    fn served_by_origin(&self) -> bool {
        !self.served_by_cache()
    }
}

/// One VIEWER request at the name the distribution published, recording what the edge served it
/// with.
async fn viewed(router: &MultiServiceRouter, domain: &str, key: &str) -> Outcome {
    let answer = view(router, domain, &format!("/{key}")).await;
    Outcome {
        status: answer.status,
        x_cache: answer.header("x-cache"),
    }
}

/// `CreateMonitoringSubscription` with the flag `Enabled` for `id`, which is what buys the
/// additional series, and the `GetMonitoringSubscription` read that confirms a client sees it.
async fn subscribe(router: &MultiServiceRouter, case: &str, id: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>Enabled</RealtimeMetricsSubscriptionStatus>\
         </RealtimeMetricsSubscriptionConfig>\
         </MonitoringSubscription>"
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        &[("content-type", "application/xml")],
        Bytes::from(document),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateMonitoringSubscription must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let read = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        read.element("RealtimeMetricsSubscriptionStatus"),
        "Enabled",
        "case {case:?}: the subscription a client opted in with must read back Enabled, got {:?}",
        read.text()
    );
}

/// The dimension pair a distribution's own series carries: the distribution the traffic was
/// addressed to, beside the Region its per-distribution series are reported for.
fn dimensions(distribution_id: &str) -> Value {
    json!([
        {"Name": edge_metrics::DISTRIBUTION_ID_DIMENSION, "Value": distribution_id},
        {"Name": edge_metrics::REGION_DIMENSION, "Value": edge_metrics::GLOBAL_REGION},
    ])
}

/// What one series holds in ONE period, as `GetMetricStatistics` folds it.
#[derive(Debug, Clone)]
struct Fold {
    samples: f64,
    sum: f64,
    average: f64,
    minimum: f64,
    maximum: f64,
    /// The unit every observation of the period was published under, or the empty string when
    /// the answer carries none.
    unit: String,
}

/// Every period `metric` holds a datapoint for, by the instant that period begins.
///
/// The key is that instant in whole seconds: CloudWatch lays its period windows on clock
/// boundaries ("round down to the nearest whole minute"), which is what lets a client read one
/// minute of several series and get the same minute from each.
async fn series(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> BTreeMap<i64, Fold> {
    let (start, end) = window(at);
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": edge_metrics::NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions(distribution_id),
            "StartTime": start,
            "EndTime": end,
            "Period": PERIOD,
            "Statistics": ["SampleCount", "Sum", "Average", "Minimum", "Maximum"],
        }),
    )
    .await;
    let mut periods: BTreeMap<i64, Fold> = BTreeMap::new();
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let read = |member: &str| {
            datapoint[member].as_f64().unwrap_or_else(|| {
                panic!("case {case:?} ({metric}): a datapoint reports no {member}: {datapoint:?}")
            })
        };
        let fold = Fold {
            samples: read("SampleCount"),
            sum: read("Sum"),
            average: read("Average"),
            minimum: read("Minimum"),
            maximum: read("Maximum"),
            unit: datapoint["Unit"].as_str().unwrap_or_default().to_string(),
        };
        // The statistic a rate is read with is the MEAN of the period's own observations, which
        // is what makes an average over a period of per-request observations the share of that
        // period's requests — so it is asserted of every period of every series read here.
        assert!(
            fold.samples > 0.0 && (fold.average * fold.samples - fold.sum).abs() < TOLERANCE,
            "case {case:?} ({metric}): a period's Average must be the mean of the observations \
             it folded, got {fold:?}"
        );
        periods.insert(period_of(case, metric, read("Timestamp")), fold);
    }
    periods
}

/// The percentile `PERCENTILE` of `metric` in each period it holds one for — the read a
/// dashboard makes of a latency series.
async fn percentiles(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> BTreeMap<i64, f64> {
    let (start, end) = window(at);
    let answered = cw(
        router,
        case,
        REGION,
        "GetMetricStatistics",
        &json!({
            "Namespace": edge_metrics::NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions(distribution_id),
            "StartTime": start,
            "EndTime": end,
            "Period": PERIOD,
            "ExtendedStatistics": [PERCENTILE],
        }),
    )
    .await;
    let mut periods: BTreeMap<i64, f64> = BTreeMap::new();
    for datapoint in answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let timestamp = datapoint["Timestamp"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?} ({metric}): a datapoint reports no Timestamp: {datapoint:?}")
        });
        let measured = datapoint["ExtendedStatistics"][PERCENTILE]
            .as_f64()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?} ({metric}): a percentile read must answer {PERCENTILE}, got \
                     {datapoint:?}"
                )
            });
        periods.insert(period_of(case, metric, timestamp), measured);
    }
    periods
}

/// `timestamp` as the whole second the period it names begins at, asserting it IS a period
/// boundary — a datapoint stamped anywhere else could not be read beside another series'.
#[allow(clippy::cast_possible_truncation)]
fn period_of(case: &str, metric: &str, timestamp: f64) -> i64 {
    let whole = timestamp.round();
    assert!(
        (timestamp - whole).abs() < TOLERANCE,
        "case {case:?} ({metric}): expected a whole-second period boundary, got {timestamp}"
    );
    let seconds = whole as i64;
    assert_eq!(
        seconds % PERIOD,
        0,
        "case {case:?} ({metric}): expected a period aligned to {PERIOD} seconds, got {timestamp}"
    );
    seconds
}

/// Every series a distribution publishes while its subscription is enabled, each beside the unit
/// its own definition names — the per-status-code rates included, which is what a client watching
/// one status reads.
fn published_series() -> Vec<(&'static str, &'static str)> {
    let mut named: Vec<(&'static str, &'static str)> = vec![
        (edge_metrics::REQUESTS_METRIC, ""),
        (edge_metrics::FOUR_XX_ERROR_RATE_METRIC, PERCENT_UNIT),
        (edge_metrics::FIVE_XX_ERROR_RATE_METRIC, PERCENT_UNIT),
        (edge_metrics::TOTAL_ERROR_RATE_METRIC, PERCENT_UNIT),
        (edge_metrics::CACHE_HIT_RATE_METRIC, PERCENT_UNIT),
        (
            edge_metrics::ORIGIN_LATENCY_METRIC,
            edge_metrics::MILLISECONDS_UNIT,
        ),
    ];
    for (_status, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
        named.push((metric, PERCENT_UNIT));
    }
    named
}

/// The unit a rate carries: it is a percentage of the period's requests.
const PERCENT_UNIT: &str = "Percent";

/// The unit a counter CloudFront publishes with no unit of measure is reported under, which is
/// the `StandardUnit` value for exactly that.
const NO_UNIT: &str = "None";

/// Whether the one answer `outcome` records observes `metric` at all: the counter and every rate
/// are observed by every answer, a hit rate only by a cacheable one, and an origin latency only
/// by one an origin produced.
fn observes(outcome: &Outcome, metric: &str) -> bool {
    if metric == edge_metrics::CACHE_HIT_RATE_METRIC {
        return outcome.cacheable();
    }
    if metric == edge_metrics::ORIGIN_LATENCY_METRIC {
        return outcome.served_by_origin();
    }
    true
}

/// One row: the keys the minute's viewer requests asked for, in order.
struct Row {
    case: &'static str,
    keys: Vec<&'static str>,
    /// Whether the row's traffic repeats a read the edge cache can answer, so at least one of its
    /// answers has to have come from that cache. Asserted on its own, so a row goes on driving
    /// the outcome classes its case names rather than reconciling against whatever the edge
    /// happened to do.
    repeats_a_read: bool,
}

/// The rows: minutes differing in how many answers of each outcome class they carried.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a read, three repeats of it and two reads of a key the origin refuses",
            keys: vec![
                PRESENT_KEY,
                PRESENT_KEY,
                PRESENT_KEY,
                PRESENT_KEY,
                ABSENT_KEY,
                ABSENT_KEY,
            ],
            repeats_a_read: true,
        },
        Row {
            case: "a read and one repeat of it",
            keys: vec![PRESENT_KEY, PRESENT_KEY],
            repeats_a_read: true,
        },
        Row {
            case: "nothing but reads of a key the origin refuses",
            keys: vec![ABSENT_KEY, ABSENT_KEY],
            repeats_a_read: false,
        },
    ]
}

#[tokio::test]
#[allow(clippy::cast_precision_loss)]
async fn one_minute_of_a_subscribed_edge_answers_every_series_it_filled() {
    let engine = aws_emulator_router::new_engine();
    // The setup plane carries a credential scope without a computed signature; the viewer plane
    // is the authenticating one the binary serves.
    let setup = emulator(&engine, false);
    let viewers = emulator(&engine, true);

    // One private origin bucket, holding the present key and granting the CloudFront service
    // principal the one action its signed origin request needs.
    let case = "the origin";
    create_bucket(&setup, case, ORIGIN_BUCKET).await;
    put_object(&setup, case, ORIGIN_BUCKET, PRESENT_KEY, PRESENT_BODY, &[]).await;
    put_bucket_policy(
        &setup,
        case,
        ORIGIN_BUCKET,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{ORIGIN_BUCKET}/*"),
            None,
        ),
    )
    .await;
    let control = create_origin_access_control(&setup, case, "subscribed-minute", "always").await;

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let table = rows();
    let mut subjects: Vec<(String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        let (id, domain, _arn) = create_distribution(
            &setup,
            row.case,
            &format!("subscribed-minute-{index}"),
            ORIGIN_BUCKET,
            &control,
            CACHING_OPTIMIZED,
        )
        .await;
        subjects.push((id, domain));
    }

    for (row, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        deployed(&setup, case, id).await;
        subscribe(&setup, case, id).await;

        // The FIRST answer this distribution's edge serves, on its own: nothing else has been
        // counted for it yet, so the period that one answer fell in is the only period any of
        // its series holds — which is what makes it readable as ONE minute.
        let at = now_secs();
        let first = viewed(&viewers, domain, row.keys[0]).await;
        assert!(
            !first.served_by_cache(),
            "case {case:?}: the first read a distribution serves is answered by the origin, since \
             its edge cache holds nothing yet, got {first:?}"
        );
        let mut minute: Option<i64> = None;
        for (metric, unit) in published_series() {
            let held = series(&setup, case, id, metric, at).await;
            if !observes(&first, metric) {
                assert!(
                    held.is_empty(),
                    "case {case:?} ({metric}): an answer that does not observe this series \
                     publishes none of it, got {held:?}"
                );
                continue;
            }
            assert_eq!(
                held.len(),
                1,
                "case {case:?} ({metric}): one served answer fills exactly one period, got \
                 {held:?}"
            );
            let (period, fold) = held
                .iter()
                .next()
                .expect("the period just asserted to be there");
            assert_eq!(
                fold.samples, 1.0,
                "case {case:?} ({metric}): that period holds this one answer's observation, got \
                 {fold:?}"
            );
            let expected_unit = if unit.is_empty() { NO_UNIT } else { unit };
            assert_eq!(
                fold.unit, expected_unit,
                "case {case:?} ({metric}): expected the unit {expected_unit}, got {fold:?}"
            );
            match minute {
                None => minute = Some(*period),
                Some(shared) => assert_eq!(
                    *period, shared,
                    "case {case:?} ({metric}): every series of one answer must be answered for \
                     the SAME period, got {period} beside {shared}"
                ),
            }
        }
        assert!(
            minute.is_some(),
            "case {case:?}: a served answer must fill a period of the distribution's series"
        );

        // The REST of the minute's traffic, recording what the edge answered each request with.
        let mut outcomes: Vec<Outcome> = vec![first];
        for key in &row.keys[1..] {
            let outcome = viewed(&viewers, domain, key).await;
            outcomes.push(outcome);
        }
        for (index, outcome) in outcomes.iter().enumerate() {
            let expected = if row.keys[index] == PRESENT_KEY {
                200
            } else {
                ORIGIN_REFUSAL_STATUS
            };
            assert_eq!(
                outcome.status, expected,
                "case {case:?}: expected the edge to answer /{} with {expected}, got {outcome:?}",
                row.keys[index]
            );
            assert!(
                [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&outcome.x_cache.as_str()),
                "case {case:?}: every answer reports who produced it, got {outcome:?}"
            );
        }
        let from_cache = outcomes.iter().filter(|o| o.served_by_cache()).count();
        assert_eq!(
            from_cache > 0,
            row.repeats_a_read,
            "case {case:?}: expected an answer from the edge cache = {}, got {from_cache} of \
             {outcomes:?}",
            row.repeats_a_read
        );

        // What those recorded outcomes say the minute's series must hold.
        let served = outcomes.len() as f64;
        let cacheable = outcomes.iter().filter(|o| o.cacheable()).count() as f64;
        let hits = outcomes
            .iter()
            .filter(|o| o.cacheable() && o.served_by_cache())
            .count() as f64;
        let origin_answered = outcomes.iter().filter(|o| o.served_by_origin()).count() as f64;

        // The counter the rest stands on: every answer the edge served, counted once, in the
        // periods the minute's traffic fell in.
        let counted = series(&setup, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        let counted_total: f64 = counted.values().map(|fold| fold.sum).sum();
        assert_eq!(
            counted_total, served,
            "case {case:?}: every answer the edge served is counted once, got {counted:?}"
        );

        // Each RATE is observed by every one of those answers, in the SAME periods the counter
        // holds them in, and its average is the share of them that carried its own class or
        // status.
        let mut rates: Vec<(&str, f64)> = vec![
            (
                edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
                share(&outcomes, |status| (400..500).contains(&status)),
            ),
            (
                edge_metrics::FIVE_XX_ERROR_RATE_METRIC,
                share(&outcomes, |status| (500..600).contains(&status)),
            ),
            (
                edge_metrics::TOTAL_ERROR_RATE_METRIC,
                share(&outcomes, |status| status >= 400),
            ),
        ];
        for (status, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            rates.push((metric, share(&outcomes, |answered| answered == status)));
        }
        for (metric, expected) in rates {
            let held = series(&setup, case, id, metric, at).await;
            assert_eq!(
                periods_of(&held),
                periods_of(&counted),
                "case {case:?} ({metric}): a rate is answered for the same periods as the \
                 requests it is a rate of, got {held:?} against {counted:?}"
            );
            for (period, fold) in &held {
                assert_eq!(
                    Some(fold.samples),
                    counted.get(period).map(|requests| requests.samples),
                    "case {case:?} ({metric}): every request of a period observes its rates, got \
                     {fold:?} against {counted:?}"
                );
                assert_eq!(
                    fold.unit, PERCENT_UNIT,
                    "case {case:?} ({metric}): a rate is a percentage, got {fold:?}"
                );
            }
            let folded = folded_average(&held);
            assert!(
                (folded - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected a rate of {expected} over the {served} \
                 answers served, got {folded} from {held:?}"
            );
        }

        // `CacheHitRate`: observed by the CACHEABLE answers alone, in periods the counter also
        // holds, and the share of them the edge cache produced — a minute with no cacheable
        // answer publishes no hit rate rather than a rate of zero.
        let hit_rate = series(&setup, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
        assert!(
            within(&hit_rate, &counted),
            "case {case:?}: a hit rate is answered only for periods the requests counter holds, \
             got {hit_rate:?} against {counted:?}"
        );
        let hit_samples: f64 = hit_rate.values().map(|fold| fold.samples).sum();
        assert_eq!(
            hit_samples, cacheable,
            "case {case:?}: the hit rate is observed by the {cacheable} cacheable answers of \
             these {served}, got {hit_rate:?}"
        );
        if cacheable > 0.0 {
            let expected = 100.0 * hits / cacheable;
            let folded = folded_average(&hit_rate);
            assert!(
                (folded - expected).abs() < TOLERANCE,
                "case {case:?}: the edge cache produced {hits} of the {cacheable} cacheable \
                 answers, so the rate is {expected}, got {folded} from {hit_rate:?}"
            );
        }

        // `OriginLatency`: one measured span per answer an ORIGIN produced, published in
        // milliseconds and answered at the percentile a dashboard reads it at — within the
        // spans the same period folded.
        let latency = series(&setup, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert!(
            within(&latency, &counted),
            "case {case:?}: a latency is answered only for periods the requests counter holds, \
             got {latency:?} against {counted:?}"
        );
        let latency_samples: f64 = latency.values().map(|fold| fold.samples).sum();
        assert_eq!(
            latency_samples, origin_answered,
            "case {case:?}: an origin produced {origin_answered} of these {served} answers, so \
             that many spans are published, got {latency:?}"
        );
        let measured = percentiles(&setup, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            periods_of(&measured),
            periods_of(&latency),
            "case {case:?}: a percentile is answered for every period the series holds spans in, \
             got {measured:?} against {latency:?}"
        );
        for (period, fold) in &latency {
            assert_eq!(
                fold.unit,
                edge_metrics::MILLISECONDS_UNIT,
                "case {case:?}: an origin latency is a span of time, got {fold:?}"
            );
            let at_percentile = measured
                .get(period)
                .copied()
                .expect("the percentile of a period just asserted to be answered");
            assert!(
                at_percentile > 0.0
                    && at_percentile >= fold.minimum
                    && at_percentile <= fold.maximum,
                "case {case:?}: expected a measured {PERCENTILE} inside the period's own spans, \
                 got {at_percentile} against {fold:?}"
            );
        }
    }
}

/// The share of `outcomes` whose status `of_class` holds for, as a percentage — what a rate over
/// those answers is.
#[allow(clippy::cast_precision_loss)]
fn share(outcomes: &[Outcome], of_class: impl Fn(u16) -> bool) -> f64 {
    let answered = outcomes
        .iter()
        .filter(|outcome| of_class(outcome.status))
        .count() as f64;
    100.0 * answered / outcomes.len() as f64
}

/// The periods a series holds a datapoint for.
fn periods_of<T>(held: &BTreeMap<i64, T>) -> Vec<i64> {
    held.keys().copied().collect()
}

/// Whether every period `held` answers is one `counted` answers too.
fn within(held: &BTreeMap<i64, Fold>, counted: &BTreeMap<i64, Fold>) -> bool {
    held.keys().all(|period| counted.contains_key(period))
}

/// The average over every observation the series holds, folded across the periods the read split
/// the minute into so the answer does not depend on which boundary the traffic straddled.
fn folded_average(held: &BTreeMap<i64, Fold>) -> f64 {
    let samples: f64 = held.values().map(|fold| fold.samples).sum();
    if samples == 0.0 {
        return f64::NAN;
    }
    held.values().map(|fold| fold.sum).sum::<f64>() / samples
}
