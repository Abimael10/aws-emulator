// SPDX-License-Identifier: CC0-1.0

//! What a viewer DIALLING the address a distribution published is served — on EITHER half of
//! that one address — and what the `AWS/CloudFront` series then hold about those answers.
//!
//! RULE: **the address a distribution publishes as `Distribution$DomainName` answers a viewer
//! request on both halves of the one address it is: the encrypted one a client of AWS dials
//! (`https://`) and the plaintext one a client that verifies no certificate dials (`http://`).
//! Whichever half served it, every answer is counted into THAT distribution's own
//! `AWS/CloudFront` series — dimensioned `DistributionId` beside `Region=Global` — with the
//! DEFAULT series published for every distribution and the ADDITIONAL ones (the cache hit
//! rate, the origin latency and the per-status-code error rates) published, and listed, exactly
//! while that distribution's monitoring subscription carries
//! `RealtimeMetricsSubscriptionStatus` `Enabled`.**
//!
//! Only a suite over the mounted service set can state that. The viewer here is a STOCK HTTP
//! client opening a real connection to the published `host:port` rather than a request handed
//! to the router: that is the one client a customer handed a domain name has, and the scheme it
//! dials is its own choice. Behind the address the origin is a private Amazon S3 bucket whose
//! policy grants the CloudFront service principal `s3:GetObject` and nothing else, so the edge
//! has to sign the origin fetch for the object store to serve it, and the object store refuses
//! a read of a key it does not hold with its own `403` (GetObject: "If you don't have the
//! `s3:ListBucket` permission, Amazon S3 returns an HTTP status code 403 Access Denied error")
//! — and CloudWatch is where the series land. A published address that answers on one scheme
//! alone leaves a client that dialled the other with no datapoint at all, which reads exactly
//! like a distribution that serves nothing.
//!
//! The crate-local suite
//! `services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/tests/all/published_edge_address.rs`
//! dials the same address in the clear over an origin needing no credential, and
//! `all/cloudfront_dialled_edge_address_over_a_private_s3_origin` dials it in the clear over
//! this private origin; `all/cloudfront_subscribed_minute_of_additional_metrics` reconciles one
//! minute of these series for traffic handed to the router. This suite is the one where the
//! viewer CHOOSES the scheme, and `all/cloudfront_edge_served_on_a_started_emulator` is the
//! same rule on the binary a customer starts — it reads the vocabulary below rather than
//! restating it, so the two arms cannot drift.
//!
//! The rows span what the rule generalises over: the scheme the viewer dialled, times whether
//! the distribution's opt-in is on. Every expected value is DERIVED from what the dialled
//! answers carried — their status and the `X-Cache` naming who produced each — so no row states
//! a count or a rate of its own.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;

use crate::cloudfront_dialled_edge_address_over_a_private_s3_origin::is_dialable;
use crate::cloudfront_edge_metrics_drive_the_alarm_chain::{emulator, now_secs};
use crate::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_distribution, create_origin_access_control,
    policy, put_bucket_policy, put_object, API_VERSION, CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
    REGION,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::cw;

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: the
/// repeat of an identical read under it is answered by the edge cache, which is what a hit rate
/// is a rate of.
pub(crate) const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The one private bucket every row's distribution fronts: what differs between rows is the
/// scheme the viewer dialled and the opt-in, never the origin.
const ORIGIN_BUCKET: &str = "dialled-scheme-origin";

/// The key that bucket HOLDS, and the bytes it holds there.
const PRESENT_KEY: &str = "present.txt";
const PRESENT_BODY: &str = "the object the dialled edge serves";

/// A key the bucket does NOT hold. The policy grants `s3:GetObject` alone, so the ORIGIN
/// refuses a read of it rather than answering not-found.
const ABSENT_KEY: &str = "absent-never-written.txt";

/// The status the origin refuses that read with, under that rule.
pub(crate) const ORIGIN_REFUSAL_STATUS: u16 = 403;

/// The `X-Cache` values CloudFront reports who produced an answer with. `pub(crate)` for the
/// reason the vocabulary below is: the suite that drives the same rule on the started binary
/// classifies its answers by exactly these.
pub(crate) const CACHE_HIT: &str = "Hit from cloudfront";
pub(crate) const CACHE_MISS: &str = "Miss from cloudfront";
pub(crate) const CACHE_ERROR: &str = "Error from cloudfront";

/// The `RealtimeMetricsSubscriptionStatus` that turns the additional series on, as the model's
/// enumerated type spells it.
pub(crate) const ENABLED: &str = "Enabled";

/// The period these series are read at, in seconds, and how many of them the read spans on
/// either side of the instant the traffic was served: the datapoints of one row all fall within
/// a minute or two of it, and the fold below is over every period the read answers.
pub(crate) const PERIOD: i64 = 60;
const WINDOW_PERIODS: i64 = 10;

/// The percentile a dashboard reads a latency series at, named as `ExtendedStatistics` spells
/// one.
const PERCENTILE: &str = "p90";

/// How close a folded percentage has to be to the share it is a percentage of: both sides are
/// computed out of the same whole counts, so the slack only absorbs the last bit of a division.
pub(crate) const TOLERANCE: f64 = 1e-9;

/// How long a dial waits to be answered. Generous: nothing here measures the wait, only what
/// came back — the address is a listener of this same process, which may be busy with every
/// other test of this workspace.
const DIAL_DEADLINE: Duration = Duration::from_secs(60);

/// What the edge answered one dialled viewer request with.
///
/// `pub(crate)`, with [`Tally`] and [`Held`], because the suite that drives this rule on the
/// started binary records and folds the same answers: one vocabulary, so the two arms of the
/// rule cannot disagree about what a hit, a cacheable request or an origin-produced answer is.
#[derive(Debug)]
pub(crate) struct Served {
    pub(crate) status: u16,
    pub(crate) x_cache: String,
    pub(crate) body: String,
}

impl Served {
    /// Whether this answer is one of the CACHEABLE requests the hit rate is a rate of: a read
    /// answered below the error classes by the cache or by an origin.
    pub(crate) fn cacheable(&self) -> bool {
        self.status < 400 && self.x_cache != CACHE_ERROR
    }

    /// Whether the edge CACHE produced it.
    pub(crate) fn served_by_cache(&self) -> bool {
        self.x_cache == CACHE_HIT
    }

    /// Whether an ORIGIN produced it, which is what an origin latency is measured for: every
    /// request these suites drive is a read the behaviour allows for an origin the object store
    /// serves, so an answer the edge cache did not hold is one that went to that origin.
    pub(crate) fn served_by_origin(&self) -> bool {
        !self.served_by_cache()
    }

    /// Whether the answer reports who produced it at all, which every answer the edge serves
    /// does.
    pub(crate) fn names_its_producer(&self) -> bool {
        [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&self.x_cache.as_str())
    }
}

/// What a row's dialled answers add up to, in the terms the series are defined in.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    pub(crate) requests: f64,
    pub(crate) cacheable: f64,
    pub(crate) hits: f64,
    pub(crate) from_origin: f64,
    /// How many bytes of body the viewer was served, over every answer: what the download
    /// counter counts for the reading methods these suites drive.
    pub(crate) bytes: f64,
    by_status: BTreeMap<u16, f64>,
}

impl Tally {
    pub(crate) fn record(&mut self, served: &Served) {
        self.requests += 1.0;
        if served.cacheable() {
            self.cacheable += 1.0;
            if served.served_by_cache() {
                self.hits += 1.0;
            }
        }
        if served.served_by_origin() {
            self.from_origin += 1.0;
        }
        self.bytes += served.body.len() as f64;
        *self.by_status.entry(served.status).or_default() += 1.0;
    }

    /// The share of these answers carrying exactly `status`, as a percentage.
    pub(crate) fn status_percent(&self, status: u16) -> f64 {
        100.0 * self.by_status.get(&status).copied().unwrap_or_default() / self.requests
    }

    /// The share carrying a status of the 4xx class, as a percentage.
    pub(crate) fn four_xx_percent(&self) -> f64 {
        self.class_percent(400..500)
    }

    /// The share carrying a status of the 5xx class, as a percentage.
    pub(crate) fn five_xx_percent(&self) -> f64 {
        self.class_percent(500..600)
    }

    /// The share carrying a status of either error class, as a percentage.
    pub(crate) fn total_error_percent(&self) -> f64 {
        self.class_percent(400..600)
    }

    /// The share of these answers whose status falls in `class`, as a percentage.
    fn class_percent(&self, class: std::ops::Range<u16>) -> f64 {
        let counted: f64 = self
            .by_status
            .iter()
            .filter(|(status, _)| class.contains(&**status))
            .map(|(_, count)| *count)
            .sum();
        100.0 * counted / self.requests
    }
}

/// What one series holds over the whole window, folded across the periods it answered: the
/// number of observations, their sum, and the units they were published under.
#[derive(Debug, Default)]
pub(crate) struct Held {
    pub(crate) samples: f64,
    pub(crate) sum: f64,
    pub(crate) units: Vec<String>,
}

impl Held {
    /// The mean of the observations folded here, which is what a rate series is read with.
    pub(crate) fn average(&self) -> f64 {
        if self.samples == 0.0 {
            return 0.0;
        }
        self.sum / self.samples
    }
}

/// Fold the datapoints a `GetMetricStatistics` read answered into what the series holds over
/// the whole window — the one fold both arms of this rule read their series with.
pub(crate) fn fold(case: &str, metric: &str, datapoints: &[Value]) -> Held {
    let mut folded = Held::default();
    for datapoint in datapoints {
        let read = |member: &str| {
            datapoint[member].as_f64().unwrap_or_else(|| {
                panic!("case {case:?} ({metric}): a datapoint reports no {member}: {datapoint:?}")
            })
        };
        folded.samples += read("SampleCount");
        folded.sum += read("Sum");
        let unit = datapoint["Unit"].as_str().unwrap_or_default().to_string();
        if !folded.units.contains(&unit) {
            folded.units.push(unit);
        }
    }
    folded
}

/// The dimension pair a distribution's own series carries.
pub(crate) fn dimensions(distribution_id: &str) -> Value {
    json!([
        {"Name": edge_metrics::DISTRIBUTION_ID_DIMENSION, "Value": distribution_id},
        {"Name": edge_metrics::REGION_DIMENSION, "Value": edge_metrics::GLOBAL_REGION},
    ])
}

/// The `(StartTime, EndTime)` a read of these series spans, around the instant the traffic was
/// served.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn window(at: f64) -> (f64, f64) {
    let span = (WINDOW_PERIODS * PERIOD) as f64;
    (at - span, at + span)
}

/// The condition clause an origin-access-control bucket policy is scoped by: the distributions
/// the grant is written for, as `AWS:SourceArn` — the form AWS's own origin-access-control
/// instructions publish, written for a set of distributions rather than for one.
pub(crate) fn source_arn_condition(distribution_arns: &[String]) -> String {
    let arns = distribution_arns
        .iter()
        .map(|arn| format!("\"{arn}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"StringEquals\":{{\"AWS:SourceArn\":[{arns}]}}}}")
}

/// One viewer request a row dials, and the status the edge is expected to answer it with.
struct Driven {
    case: &'static str,
    key: &'static str,
    status: u16,
}

/// The traffic every row drives at the address it was handed, in order: a read the origin
/// answers, the same read again (which the edge cache answers), and a read of a key the origin
/// refuses.
fn driven() -> [Driven; 3] {
    [
        Driven {
            case: "a read the origin answers",
            key: PRESENT_KEY,
            status: 200,
        },
        Driven {
            case: "the same read again, which the edge cache answers",
            key: PRESENT_KEY,
            status: 200,
        },
        Driven {
            case: "a read of a key the origin refuses",
            key: ABSENT_KEY,
            status: ORIGIN_REFUSAL_STATUS,
        },
    ]
}

/// One row: the scheme the viewer dials the published address with, and whether the
/// distribution's monitoring subscription is in place while it is dialled.
struct Row {
    case: &'static str,
    scheme: &'static str,
    subscribed: bool,
}

/// The rows: each scheme of the one published address, with the opt-in on and off.
fn rows() -> [Row; 4] {
    [
        Row {
            case: "a viewer dialling the encrypted half of a subscribed distribution's address",
            scheme: "https",
            subscribed: true,
        },
        Row {
            case: "a viewer dialling the encrypted half of an unsubscribed distribution's address",
            scheme: "https",
            subscribed: false,
        },
        Row {
            case: "a viewer dialling the plaintext half of a subscribed distribution's address",
            scheme: "http",
            subscribed: true,
        },
        Row {
            case: "a viewer dialling the plaintext half of an unsubscribed distribution's address",
            scheme: "http",
            subscribed: false,
        },
    ]
}

/// One viewer request made by DIALLING `authority` over `scheme`, with no credential of any
/// kind — a stock client fetching the name the distribution published.
///
/// The exchange runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while the call waits for the answer. The client verifies no certificate,
/// which is what a client of a local emulator does for an address it signed itself. It is built
/// and dropped INSIDE that blocking thread: a blocking client owns a runtime of its own, and
/// dropping one from an asynchronous context panics, so it cannot be hoisted out of here.
async fn dial(case: &str, scheme: &str, authority: &str, key: &str) -> Served {
    let url = format!("{scheme}://{authority}/{key}");
    let named = case.to_string();
    let dialled = url.clone();
    ::tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(DIAL_DEADLINE)
            .build()
            .expect("a client");
        let answered = client.get(&dialled).send().unwrap_or_else(|error| {
            panic!("case {named:?}: the published address {dialled} answered nothing: {error}")
        });
        let status = answered.status().as_u16();
        let x_cache = answered
            .headers()
            .get("x-cache")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Served {
            status,
            x_cache,
            body: answered.text().unwrap_or_default(),
        }
    })
    .await
    .unwrap_or_else(|error| panic!("case {case:?}: the dial of {url} did not finish: {error}"))
}

/// `CreateMonitoringSubscription` with the flag that buys the additional series, and the
/// `GetMonitoringSubscription` read that confirms a client sees it.
async fn subscribe(router: &MultiServiceRouter, case: &str, id: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>{ENABLED}</RealtimeMetricsSubscriptionStatus>\
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
        ENABLED,
        "case {case:?}: the opt-in a client bought must read back {ENABLED}, got {:?}",
        read.text()
    );
}

/// Every datapoint `GetMetricStatistics` answers for one of a distribution's series in the
/// window around `at`.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> Vec<Value> {
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
            "Statistics": ["SampleCount", "Sum", "Average"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What one series holds in that window, folded over every period it answered.
async fn held(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> Held {
    let answered = datapoints(router, case, distribution_id, metric, at).await;
    fold(case, metric, &answered)
}

/// The PERCENTILE the latency series answers in the window, if it answers one — the read a
/// dashboard makes of it.
async fn percentile_of(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> Option<f64> {
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
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .find_map(|datapoint| datapoint["ExtendedStatistics"][PERCENTILE].as_f64())
}

/// What `ListMetrics` reports for one of a distribution's series — the filter a client uses to
/// ask whether a series exists at all.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
) -> Vec<Value> {
    let answered = cw(
        router,
        case,
        REGION,
        "ListMetrics",
        &json!({
            "Namespace": edge_metrics::NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions(distribution_id),
        }),
    )
    .await;
    answered["Metrics"].as_array().cloned().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_dialling_either_half_of_the_published_address_fills_the_series() {
    let engine = aws_emulator_router::new_engine();
    // The setup plane carries a credential scope without a computed signature; the viewer is a
    // stock client dialling the published address, which presents no credential at all.
    let setup = emulator(&engine, false);

    let origin = "the private origin";
    create_bucket(&setup, origin, ORIGIN_BUCKET).await;
    put_object(&setup, origin, ORIGIN_BUCKET, PRESENT_KEY, PRESENT_BODY, &[]).await;
    let control = create_origin_access_control(&setup, origin, "dialled-scheme", "always").await;

    // Every row's distribution is created first, so one document can grant the whole set, and
    // each row is then awaited on the CONDITION its record reaches.
    let table = rows();
    let mut subjects: Vec<(String, String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(
            create_distribution(
                &setup,
                row.case,
                &format!("dialled-scheme-{index}"),
                ORIGIN_BUCKET,
                &control,
                CACHING_OPTIMIZED,
            )
            .await,
        );
    }
    let arns: Vec<String> = subjects.iter().map(|(_, _, arn)| arn.clone()).collect();
    put_bucket_policy(
        &setup,
        origin,
        ORIGIN_BUCKET,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{ORIGIN_BUCKET}/*"),
            Some(&source_arn_condition(&arns)),
        ),
    )
    .await;

    for (row, (id, domain, _arn)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        awaited_deployment(&setup, case, id).await;
        assert!(
            is_dialable(domain),
            "case {case:?}: expected a published address a viewer on this machine can dial, got \
             {domain:?}"
        );
        if row.subscribed {
            subscribe(&setup, case, id).await;
        }

        let at = now_secs();
        let mut tally = Tally::default();
        for request in driven() {
            let served = dial(case, row.scheme, domain, request.key).await;
            assert_eq!(
                served.status, request.status,
                "case {case:?} ({}): expected the dialled address to answer {}, got {served:?}",
                request.case, request.status
            );
            if served.status == 200 {
                assert_eq!(
                    served.body, PRESENT_BODY,
                    "case {case:?} ({}): expected the origin's own bytes, got {served:?}",
                    request.case
                );
            } else {
                assert!(
                    !served.body.contains(PRESENT_BODY),
                    "case {case:?} ({}): a refusal carries none of the object's bytes, got \
                     {served:?}",
                    request.case
                );
            }
            assert!(
                served.names_its_producer(),
                "case {case:?} ({}): every answer reports who produced it, got {served:?}",
                request.case
            );
            tally.record(&served);
        }
        assert!(
            tally.hits > 0.0 && tally.from_origin > 0.0,
            "case {case:?}: the traffic must reach both the cache and the origin for the rates \
             below to mean anything, got {tally:?}"
        );

        // The DEFAULT series: published for the dialled traffic whatever the opt-in reads.
        let counted = held(&setup, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, tally.requests,
            "case {case:?}: expected the counter to count every answer the dialled address \
             served ({}), got {counted:?}",
            tally.requests
        );
        // Each class rate is the share of the SAME answers whose status falls in that class, so
        // a class none of them carried is published as the zero every answer observed for it
        // rather than left without a series.
        for (metric, expected) in [
            (
                edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
                tally.four_xx_percent(),
            ),
            (
                edge_metrics::FIVE_XX_ERROR_RATE_METRIC,
                tally.five_xx_percent(),
            ),
            (
                edge_metrics::TOTAL_ERROR_RATE_METRIC,
                tally.total_error_percent(),
            ),
        ] {
            let rate = held(&setup, case, id, metric, at).await;
            assert_eq!(
                rate.samples, tally.requests,
                "case {case:?} ({metric}): every answer served observes this rate ({}), got \
                 {rate:?}",
                tally.requests
            );
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected the share of the dialled answers of that \
                 class ({expected}), got {rate:?}"
            );
        }
        let downloaded = held(&setup, case, id, edge_metrics::BYTES_DOWNLOADED_METRIC, at).await;
        assert_eq!(
            downloaded.sum, tally.bytes,
            "case {case:?}: expected the bytes of every body the viewer was served ({}), got \
             {downloaded:?}",
            tally.bytes
        );

        // The ADDITIONAL series: published, and listed, exactly while the opt-in is on.
        if !row.subscribed {
            for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
                let published = datapoints(&setup, case, id, metric, at).await;
                assert!(
                    published.is_empty(),
                    "case {case:?} ({metric}): a distribution carrying no subscription publishes \
                     no datapoint of this series, got {published:?}"
                );
                let series = listed(&setup, case, id, metric).await;
                assert!(
                    series.is_empty(),
                    "case {case:?} ({metric}): a series nothing published is listed nowhere, got \
                     {series:?}"
                );
            }
            let requests_listed = listed(&setup, case, id, edge_metrics::REQUESTS_METRIC).await;
            assert!(
                !requests_listed.is_empty(),
                "case {case:?}: the default series the dialled traffic filled is listed all the \
                 same, got {requests_listed:?}"
            );
            continue;
        }

        let hit_rate = held(&setup, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
        assert_eq!(
            hit_rate.samples, tally.cacheable,
            "case {case:?}: the hit rate is a rate of the cacheable answers the dialled address \
             served ({}), got {hit_rate:?}",
            tally.cacheable
        );
        let expected_hit_rate = 100.0 * tally.hits / tally.cacheable;
        assert!(
            (hit_rate.average() - expected_hit_rate).abs() < TOLERANCE,
            "case {case:?}: expected a hit rate of {expected_hit_rate} ({} of {} cacheable \
             answers came from the cache), got {hit_rate:?}",
            tally.hits,
            tally.cacheable
        );

        let latency = held(&setup, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            latency.samples, tally.from_origin,
            "case {case:?}: the origin latency is published for the answers an origin produced \
             ({}), got {latency:?}",
            tally.from_origin
        );
        assert_eq!(
            latency.units,
            vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
            "case {case:?}: an origin latency is a span of time, got {latency:?}"
        );
        // The percentile a dashboard reads a latency series at is one of the spans that series
        // folded, so it is a measured span: above zero, and no larger than the sum of the spans
        // the same window observed.
        let percentile = percentile_of(
            &setup,
            case,
            id,
            edge_metrics::ORIGIN_LATENCY_METRIC,
            at,
        )
        .await;
        assert!(
            percentile.is_some_and(|value| value > 0.0 && value <= latency.sum),
            "case {case:?}: expected the {PERCENTILE} of the latency series to be one of the \
             {} spans it folded (summing to {}), got {percentile:?}",
            latency.samples,
            latency.sum
        );

        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&setup, case, id, metric, at).await;
            assert_eq!(
                rate.samples, tally.requests,
                "case {case:?} ({metric}): every answer served observes a per-status rate ({}), \
                 got {rate:?}",
                tally.requests
            );
            let expected = tally.status_percent(code);
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected a rate of {expected} over the dialled \
                 answers, got {rate:?}"
            );
        }

        for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
            let series = listed(&setup, case, id, metric).await;
            let published = datapoints(&setup, case, id, metric, at).await;
            assert_eq!(
                series.is_empty(),
                published.is_empty(),
                "case {case:?} ({metric}): what a client lists must agree with what it can read, \
                 got listed {series:?} for datapoints {published:?}"
            );
        }
    }
}
