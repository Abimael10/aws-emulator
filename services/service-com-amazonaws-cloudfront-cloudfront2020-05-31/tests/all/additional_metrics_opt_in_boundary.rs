// SPDX-License-Identifier: CC0-1.0

//! WHERE the opt-in boundary of the additional `AWS/CloudFront` series falls when the opt-in
//! is turned on or off between two runs of the same traffic.
//!
//! RULE: **whether one viewer request contributes to the additional per-distribution series —
//! the cache hit rate, the origin latency and the per-status-code error rates — is decided for
//! that request alone, from the monitoring subscription its distribution carried when the edge
//! served it, so those series cover exactly the requests served while
//! `RealtimeMetricsSubscriptionConfig$RealtimeMetricsSubscriptionStatus` read `Enabled`:
//! turning the opt-in on or off changes what LATER traffic publishes and never what EARLIER
//! traffic already published, while the default series count every request of either run and a
//! distribution that was never opted in publishes no additional datapoint at all.**
//!
//! The sibling suites pin WHICH distributions publish these series (`edge_additional_metrics`),
//! what a client reads off two siblings differing only in the subscription
//! (`additional_metrics_gating`) and what the series' VALUES are
//! (`edge_additional_metric_reconciliation`). All three hold the opt-in still for the whole of
//! a distribution's traffic; this one MOVES it, which is what the boundary is a boundary of.
//!
//! Each row drives the same traffic twice through a distribution of its own and differs only in
//! what the opt-in read during each run: the rows span every transition between the states the
//! opt-in can be in — no subscription, a subscription whose flag is each value the model's
//! enumerated type takes (`REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES`) — including turning it
//! on part-way, turning it off part-way (by releasing the subscription and by replacing it with
//! a disabled one), leaving it off throughout and leaving it on throughout.
//!
//! No row states a hit, a miss or a sample count of its own: every expectation is derived from
//! the status and the `X-Cache` the edge answered each request with, folded over the runs whose
//! opt-in was on.

use std::collections::BTreeMap;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

// The readers and the viewer harness the sibling suites already own: one definition of a
// `GetMetricStatistics` read, of the `ListMetrics` filter, and of what the edge answered one
// viewer request with, so the suites over these series cannot drift apart.
use crate::additional_metrics_gating::{basic_statistics, datapoints, listed};
use crate::distribution_lifecycle::{
    awaited_deployment, call, create_distribution, API_VERSION, HOST,
};
use crate::edge_additional_metric_reconciliation::{
    viewed, Outcome, CACHE_ERROR, CACHE_HIT, CACHE_MISS,
};
use crate::edge_request_metrics::{emulator, held, now_secs};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// repeat of an identical read under it is answered by the edge cache, which is what a hit rate
/// is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint, which answers
/// `GET /2020-05-31/distribution` with a `DistributionList` and a read of an id it holds no
/// distribution for with its own 404.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the origin HOLDS, answered 200.
const PRESENT_KEY: &str = "/2020-05-31/distribution";

/// The `RealtimeMetricsSubscriptionStatus` that turns the additional metrics ON, as the model's
/// enumerated type spells it (`REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES`).
const ENABLED: &str = "Enabled";

/// The other value that enumerated type takes, which buys nothing.
const DISABLED: &str = "Disabled";

/// How close a folded percentage has to be to the share it is a percentage of: the fold is
/// exact arithmetic over whole observations, so the slack is only against a division's last
/// bit.
const TOLERANCE: f64 = 1e-9;

/// The `DistributionConfig` every row serves through: one origin under a caching policy, and a
/// default behaviour allowing only the reading methods — so a `DELETE` is refused by CloudFront
/// itself, reaching neither the cache nor the origin.
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
         <Comment>additional metrics opt-in boundary</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// What the opt-in reads for a distribution at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptIn {
    /// The distribution carries no monitoring subscription at all.
    Unsubscribed,
    /// It carries one whose `RealtimeMetricsSubscriptionStatus` reads this value.
    Subscribed(&'static str),
}

impl OptIn {
    /// Whether the additional series are bought while the opt-in reads this: only the one
    /// value of the flag that enables them does.
    fn publishes_additional(self) -> bool {
        self == OptIn::Subscribed(ENABLED)
    }
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

/// `CreateMonitoringSubscription` for `id`, carrying `flag`.
pub(crate) async fn subscribe(router: &MultiServiceRouter, case: &str, id: &str, flag: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>{flag}</RealtimeMetricsSubscriptionStatus>\
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
        "case {case:?}: expected HTTP 200 from CreateMonitoringSubscription({flag}), got {}: {}",
        answer.status, answer.raw
    );
}

/// `DeleteMonitoringSubscription` for `id`.
async fn unsubscribe(router: &MultiServiceRouter, case: &str, id: &str) {
    let answer = call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        None,
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from DeleteMonitoringSubscription, got {}: {}",
        answer.status, answer.raw
    );
}

/// Move the opt-in of `id` from `from` to `to` through the control plane, and assert a client
/// reads the new state back: a subscription carrying the flag, or the refusal a distribution
/// carrying none answers.
async fn move_opt_in(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    from: OptIn,
    to: OptIn,
) {
    if from != to {
        // A distribution carries at most one monitoring subscription, so replacing the flag is
        // releasing the subscription and buying another.
        if matches!(from, OptIn::Subscribed(_)) {
            unsubscribe(router, case, id).await;
        }
        if let OptIn::Subscribed(flag) = to {
            subscribe(router, case, id, flag).await;
        }
    }

    let read = call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        None,
        None,
    )
    .await;
    match to {
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

/// What a set of recorded answers adds up to, in the terms the series are defined in.
#[derive(Debug, Default)]
struct Tally {
    /// How many viewer requests were served.
    requests: f64,
    /// How many of them are cacheable requests, which the hit rate is a rate of.
    cacheable: f64,
    /// How many of those the edge cache answered.
    hits: f64,
    /// How many an ORIGIN answered, which the latency series is published for.
    from_origin: f64,
    /// How many carried each status, which the per-status-code rates are shares of.
    by_status: BTreeMap<u16, f64>,
}

impl Tally {
    fn record(&mut self, outcome: &Outcome) {
        self.requests += 1.0;
        if outcome.cacheable() {
            self.cacheable += 1.0;
            if outcome.served_by_cache() {
                self.hits += 1.0;
            }
        }
        if outcome.served_by_origin() {
            self.from_origin += 1.0;
        }
        *self.by_status.entry(outcome.status).or_default() += 1.0;
    }

    /// The share of these requests answered with exactly `status`, as a percentage.
    fn status_percent(&self, status: u16) -> f64 {
        100.0 * self.by_status.get(&status).copied().unwrap_or_default() / self.requests
    }

    /// The share answered with a status of the 4xx class, as a percentage.
    fn four_xx_percent(&self) -> f64 {
        let client_errors: f64 = self
            .by_status
            .iter()
            .filter(|(status, _)| (400..500).contains(*status))
            .map(|(_, count)| *count)
            .sum();
        100.0 * client_errors / self.requests
    }
}

/// One viewer request a run drives: the method, the path and the status the edge answers it
/// with — a check that the run drove the traffic it meant to, never where an expectation comes
/// from.
struct Request {
    method: &'static str,
    path: String,
    status: u16,
}

/// The traffic ONE run drives: a read of a key the origin holds and a repeat of it, a read of a
/// key no origin holds (answered with an error, which is no cacheable request), and a method the
/// behaviour does not allow, which CloudFront refuses itself. The absent key is this run's own,
/// so the error is produced by the origin in every run rather than by a cache a previous run
/// filled.
fn traffic(run: &str) -> Vec<Request> {
    vec![
        Request {
            method: "GET",
            path: PRESENT_KEY.to_string(),
            status: 200,
        },
        Request {
            method: "GET",
            path: PRESENT_KEY.to_string(),
            status: 200,
        },
        Request {
            method: "GET",
            path: format!("{PRESENT_KEY}/E0ABSENT{run}"),
            status: 404,
        },
        Request {
            method: "DELETE",
            path: PRESENT_KEY.to_string(),
            status: 403,
        },
    ]
}

/// One row: what the opt-in reads during each of the two runs of that traffic.
struct Row {
    case: &'static str,
    runs: [OptIn; 2],
}

/// The rows: every transition of the opt-in across the two runs, plus leaving it where it is.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "opted in only after the first traffic was served",
            runs: [OptIn::Unsubscribed, OptIn::Subscribed(ENABLED)],
        },
        Row {
            case: "opted out by releasing the subscription after the first traffic",
            runs: [OptIn::Subscribed(ENABLED), OptIn::Unsubscribed],
        },
        Row {
            case: "opted in by replacing a disabled subscription with an enabled one",
            runs: [OptIn::Subscribed(DISABLED), OptIn::Subscribed(ENABLED)],
        },
        Row {
            case: "opted out by replacing an enabled subscription with a disabled one",
            runs: [OptIn::Subscribed(ENABLED), OptIn::Subscribed(DISABLED)],
        },
        Row {
            case: "opted in for both runs",
            runs: [OptIn::Subscribed(ENABLED), OptIn::Subscribed(ENABLED)],
        },
        Row {
            case: "never opted in",
            runs: [OptIn::Unsubscribed, OptIn::Unsubscribed],
        },
    ]
}

#[tokio::test]
async fn the_additional_series_cover_exactly_the_traffic_served_while_the_opt_in_was_on() {
    let router = emulator();
    let table = rows();

    // Every row's distribution is created first and then awaited, so the suite waits for the
    // CONDITION each record reaches rather than for a span of the host's clock.
    let mut subjects: Vec<(String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(created(&router, row.case, &format!("opt-in-boundary-{index}")).await);
    }

    for (row, (id, domain)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        awaited_deployment(&router, case, id).await;

        let at = now_secs();
        // Everything the row's traffic adds up to, and the part of it served while the opt-in
        // was on: the only difference between the two is WHEN each request was served.
        let mut all = Tally::default();
        let mut gated = Tally::default();
        let mut opt_in = OptIn::Unsubscribed;
        for (index, run) in row.runs.iter().enumerate() {
            move_opt_in(&router, case, id, opt_in, *run).await;
            opt_in = *run;
            for request in traffic(&format!("{index}")) {
                let outcome = viewed(&router, request.method, domain, &request.path).await;
                assert_eq!(
                    outcome.status, request.status,
                    "case {case:?} (run {index}, {} {}): expected the edge to answer {}, got \
                     {outcome:?}",
                    request.method, request.path, request.status
                );
                all.record(&outcome);
                assert!(
                    [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&outcome.x_cache.as_str()),
                    "case {case:?} (run {index}): every answer reports who produced it, got \
                     {outcome:?}"
                );
                if run.publishes_additional() {
                    gated.record(&outcome);
                }
            }
        }

        // Every request of both runs is counted into the DEFAULT series, whatever the opt-in
        // read while it was served.
        let counted = held(&router, case, id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, all.requests,
            "case {case:?}: the default counter counts every request of both runs ({}), got \
             {counted:?}",
            all.requests
        );
        let four_xx = held(
            &router,
            case,
            id,
            edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
            at,
        )
        .await;
        assert!(
            (four_xx.average() - all.four_xx_percent()).abs() < TOLERANCE,
            "case {case:?}: the default 4xx rate is the share of ALL the traffic answered 4xx \
             ({}), got {four_xx:?}",
            all.four_xx_percent()
        );

        // The ADDITIONAL series cover the requests served while the opt-in was on, and those
        // alone.
        if gated.requests == 0.0 {
            for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
                let published =
                    datapoints(&router, case, id, metric, at, basic_statistics()).await;
                assert!(
                    published.is_empty(),
                    "case {case:?} ({metric}): no request was served under an enabled opt-in, so \
                     this series has no datapoint in any minute, got {published:?}"
                );
                let series = listed(&router, case, id, metric).await;
                assert!(
                    series.is_empty(),
                    "case {case:?} ({metric}): a series nothing published is listed nowhere, got \
                     {series:?}"
                );
            }
            continue;
        }

        let hit_rate = held(&router, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at).await;
        assert_eq!(
            hit_rate.samples, gated.cacheable,
            "case {case:?}: the hit rate is a rate of the cacheable requests served under an \
             enabled opt-in ({}), got {hit_rate:?}",
            gated.cacheable
        );
        if gated.cacheable > 0.0 {
            let expected = 100.0 * gated.hits / gated.cacheable;
            assert!(
                (hit_rate.average() - expected).abs() < TOLERANCE,
                "case {case:?}: expected a hit rate of {expected} ({} of {} cacheable requests \
                 came from the cache), got {hit_rate:?}",
                gated.hits,
                gated.cacheable
            );
        }

        let latency = held(&router, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at).await;
        assert_eq!(
            latency.samples, gated.from_origin,
            "case {case:?}: the origin latency is published for the answers an origin produced \
             under an enabled opt-in ({}), got {latency:?}",
            gated.from_origin
        );
        if gated.from_origin > 0.0 {
            assert_eq!(
                latency.units,
                vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
                "case {case:?}: an origin latency is a span of time, got {latency:?}"
            );
        }

        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&router, case, id, metric, at).await;
            assert_eq!(
                rate.samples, gated.requests,
                "case {case:?} ({metric}): every request served under an enabled opt-in observes \
                 a per-status rate ({}), got {rate:?}",
                gated.requests
            );
            let expected = gated.status_percent(code);
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected a rate of {expected} over the traffic served \
                 under an enabled opt-in, got {rate:?}"
            );
        }

        // What a client can LIST agrees with what it can read, series by series.
        for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
            let published = datapoints(&router, case, id, metric, at, basic_statistics()).await;
            let series = listed(&router, case, id, metric).await;
            assert_eq!(
                series.is_empty(),
                published.is_empty(),
                "case {case:?} ({metric}): what a client lists must agree with what it can read, \
                 got listed {series:?} for datapoints {published:?}"
            );
        }
    }
}

#[test]
fn the_gate_this_suite_drives_is_the_one_the_publisher_reads() {
    use edge_metrics::AdditionalMetrics;
    use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::{
        MonitoringSubscription, RealtimeMetricsSubscriptionConfig,
    };

    // The two values named above are the enumerated type's own
    // (`REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES`), so a row cannot name a flag the model
    // does not have.
    for flag in [ENABLED, DISABLED] {
        assert!(
            REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES.contains(&flag),
            "case {flag:?}: expected a value of the modelled enumerated type, which holds {:?}",
            REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES
        );
    }

    // Each state the rows above drive the opt-in into, beside the record a distribution carries
    // while it is in that state: what this suite reads off the state and what the PUBLISHER
    // reads off the record have to be one reading.
    let mut states: Vec<(OptIn, Option<MonitoringSubscription>)> = vec![(OptIn::Unsubscribed, None)];
    for flag in REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES.iter().copied() {
        states.push((
            OptIn::Subscribed(flag),
            Some(MonitoringSubscription {
                realtime_metrics_subscription_config: Some(RealtimeMetricsSubscriptionConfig {
                    realtime_metrics_subscription_status: flag.to_string(),
                }),
            }),
        ));
    }
    for (opt_in, carried) in &states {
        let gate = AdditionalMetrics::of_subscription(carried.as_ref());
        let expected = if opt_in.publishes_additional() {
            AdditionalMetrics::Enabled
        } else {
            AdditionalMetrics::Withheld
        };
        assert_eq!(
            gate, expected,
            "case {opt_in:?}: expected the publisher's gate to read {expected:?}, got {gate:?}"
        );
    }

    // A subscription carrying no configuration at all names no flag, so it buys nothing.
    let empty = MonitoringSubscription::default();
    let gate = AdditionalMetrics::of_subscription(Some(&empty));
    assert_eq!(
        gate,
        AdditionalMetrics::Withheld,
        "case {:?}: expected the publisher's gate to read {:?}, got {gate:?}",
        "a subscription carrying no configuration",
        AdditionalMetrics::Withheld
    );
}
