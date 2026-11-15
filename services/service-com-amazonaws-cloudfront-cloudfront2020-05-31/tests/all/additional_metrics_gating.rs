// SPDX-License-Identifier: CC0-1.0

//! What a client can tell about a distribution's monitoring subscription from the series it
//! can READ: the gating contrast between two siblings serving identical traffic.
//!
//! RULE: **the additional per-distribution series a monitoring subscription buys — the cache
//! hit rate, the origin latency and the per-status-code error rates — are published for the
//! traffic a distribution serves only while that distribution's subscription carries
//! `RealtimeMetricsSubscriptionStatus` `Enabled`; for a distribution carrying no subscription
//! each of them answers NO datapoint to any read of any statistic, basic or extended, and is
//! reported by no `ListMetrics` filter, while the default series the same traffic fills are
//! published for it all the same and agree with what it served.**
//!
//! The sibling suite `edge_additional_metrics` pins that gate over every value the
//! subscription flag takes. This one pins it as a CLIENT observes it: a pair of distributions
//! over one origin, differing only in that one is subscribed, driven with the SAME traffic,
//! read back through the forms a dashboard uses — a basic-statistics read, a PERCENTILE read
//! (`ExtendedStatistics`), and a `ListMetrics` narrowed by metric name and dimensions — plus
//! the refusal the unsubscribed sibling's `GetMonitoringSubscription` answers.
//!
//! The rows differ along the traffic mix, which is what each gated series is a measure OF: a
//! single read an origin answers, a read and its repeat (which the edge cache answers), an
//! origin-produced error beside a request CloudFront refuses itself, and nothing but a request
//! CloudFront refuses itself — so each row expects a different set of the gated series to hold
//! anything at all for the subscribed sibling, and every one of them to hold nothing for the
//! control.

use serde_json::{json, Value};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, call, create_distribution, API_VERSION, HOST,
};
use crate::edge_request_metrics::{
    cloudwatch, dimensions, emulator, held, now_secs, view, window, PERIOD,
};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: the
/// second identical read of an object under it is answered by the edge cache, which is what a
/// hit rate is a rate of.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint, which answers
/// `GET /2020-05-31/distribution` with a `DistributionList` and a read of an id it holds no
/// distribution for with its own 404.
const SERVED_ORIGIN: &str = HOST;

/// The `RealtimeMetricsSubscriptionStatus` that enables the additional metrics.
const ENABLED: &str = "Enabled";

/// The percentile a dashboard reads a latency series at, named as `ExtendedStatistics` spells
/// one: the statistic the additional origin-latency series is watched with.
const PERCENTILE: &str = "p90";

/// The message `NoSuchMonitoringSubscription` carries for a distribution carrying none: fixed
/// service text with no variable part.
const NO_SUCH_SUBSCRIPTION_MESSAGE: &str = "The monitoring subscription does not exist.";

/// The `DistributionConfig` both siblings of a row are created from: one origin under a
/// caching policy, and a default behaviour allowing only the reading methods — so a `DELETE`
/// is refused by CloudFront itself, reaching no origin.
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
         <Comment>additional metrics gating</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One distribution of a row, as the client that created it knows it.
#[derive(Debug, Clone)]
struct Subject {
    id: String,
    domain: String,
}

/// Create one distribution and answer its id and published `DomainName`.
async fn created(router: &MultiServiceRouter, caller_reference: &str) -> Subject {
    let created = create_distribution(router, &config(caller_reference)).await;
    assert_eq!(
        created.status, 201,
        "case {caller_reference:?}: expected HTTP 201 from CreateDistribution, got {}: {}",
        created.status, created.raw
    );
    let root = created.root();
    Subject {
        id: root
            .find_text("Id")
            .expect("the created distribution reports an id")
            .to_string(),
        domain: root
            .find_text("DomainName")
            .expect("the created distribution reports a domain name")
            .to_string(),
    }
}

/// `CreateMonitoringSubscription` for `id`, with the flag `Enabled`.
async fn subscribe(router: &MultiServiceRouter, case: &str, id: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>{ENABLED}</RealtimeMetricsSubscriptionStatus>\
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

/// `GetMonitoringSubscription` for `id`.
async fn read_subscription(
    router: &MultiServiceRouter,
    id: &str,
) -> crate::distribution_lifecycle::Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        None,
        None,
    )
    .await
}

/// One `GetMetricStatistics` read of one of a distribution's series, answering the raw
/// `Datapoints` list: what a client sees, emptiness included.
pub(crate) async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
    statistics: Value,
) -> Vec<Value> {
    let (start, end) = window(at);
    let mut request = json!({
        "Namespace": edge_metrics::NAMESPACE,
        "MetricName": metric,
        "Dimensions": dimensions(distribution_id),
        "StartTime": start,
        "EndTime": end,
        "Period": PERIOD,
    });
    let (member, value) = match statistics {
        Value::String(percentile) => ("ExtendedStatistics", json!([percentile])),
        other => ("Statistics", other),
    };
    request[member] = value;
    let answered = cloudwatch(router, case, "GetMetricStatistics", &request).await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What `ListMetrics` reports for ONE of a distribution's series, narrowed by the series' own
/// name and dimensions — the filter a client uses to ask whether a series exists at all.
pub(crate) async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    distribution_id: &str,
    metric: &str,
) -> Vec<Value> {
    let answered = cloudwatch(
        router,
        case,
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

/// One viewer request a row drives, and what the edge is expected to do with it.
struct Viewer {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    /// The status the edge answers with.
    status: u16,
    /// Whether an ORIGIN produced the answer, which is what the origin-latency series is
    /// published for.
    from_origin: bool,
    /// Whether this request is one the hit rate is a rate OF, and if so whether the edge cache
    /// answered it: `None` for a request that is not a cacheable request at all.
    cacheable: Option<bool>,
}

/// A read the origin answers from scratch: a cacheable request, and a miss.
fn origin_read(case: &'static str) -> Viewer {
    Viewer {
        case,
        method: "GET",
        path: "/2020-05-31/distribution",
        status: 200,
        from_origin: true,
        cacheable: Some(false),
    }
}

/// The same read again, which the edge cache answers: a cacheable request, and a hit.
fn cached_read(case: &'static str) -> Viewer {
    Viewer {
        case,
        method: "GET",
        path: "/2020-05-31/distribution",
        status: 200,
        from_origin: false,
        cacheable: Some(true),
    }
}

/// A read of an object the origin does not hold: the origin answers it, with an error — which
/// is "not considered a cacheable request".
fn origin_error(case: &'static str) -> Viewer {
    Viewer {
        case,
        method: "GET",
        path: "/2020-05-31/distribution/E0NOSUCHDISTRIB",
        status: 404,
        from_origin: true,
        cacheable: None,
    }
}

/// A method the cache behaviour does not allow: CloudFront refuses it itself, reaching no
/// origin and no cache.
fn refused_by_cloudfront(case: &'static str) -> Viewer {
    Viewer {
        case,
        method: "DELETE",
        path: "/2020-05-31/distribution",
        status: 403,
        from_origin: false,
        cacheable: None,
    }
}

/// One row: a traffic mix both siblings of the row are driven with.
struct Mix {
    case: &'static str,
    traffic: Vec<Viewer>,
}

/// The rows: traffic mixes spanning what the gated series measure.
fn mixes() -> Vec<Mix> {
    vec![
        Mix {
            case: "one read an origin answers",
            traffic: vec![origin_read("the read")],
        },
        Mix {
            case: "a read and the same read again",
            traffic: vec![origin_read("the first read"), cached_read("the repeat")],
        },
        Mix {
            case: "an origin-produced error beside a request CloudFront refuses itself",
            traffic: vec![
                origin_error("the read of an absent object"),
                refused_by_cloudfront("the disallowed method"),
            ],
        },
        Mix {
            case: "nothing but a request CloudFront refuses itself",
            traffic: vec![refused_by_cloudfront("the disallowed method")],
        },
    ]
}

/// What a mix's traffic is expected to have published, derived from the outcomes the mix
/// declares and the definition of each series — never from any one recorded run.
#[derive(Debug)]
struct Expected {
    /// How many viewer requests the mix drove, which the request counter counts.
    requests: f64,
    /// The share of them answered with a status of the 4xx class.
    four_xx_percent: f64,
    /// How many of them the hit rate is a rate of, and the rate those have.
    hit_samples: f64,
    hit_percent: Option<f64>,
    /// How many of them an origin answered, which the latency series is published for.
    origin_samples: f64,
}

impl Expected {
    fn of(traffic: &[Viewer]) -> Self {
        let requests = traffic.len() as f64;
        let four_xx = traffic
            .iter()
            .filter(|request| (400..500).contains(&request.status))
            .count() as f64;
        let hit_samples = traffic
            .iter()
            .filter(|request| request.cacheable.is_some())
            .count() as f64;
        let hits = traffic
            .iter()
            .filter(|request| request.cacheable == Some(true))
            .count() as f64;
        Self {
            requests,
            four_xx_percent: 100.0 * four_xx / requests,
            hit_samples,
            hit_percent: (hit_samples > 0.0).then_some(100.0 * hits / hit_samples),
            origin_samples: traffic
                .iter()
                .filter(|request| request.from_origin)
                .count() as f64,
        }
    }

    /// The share of the mix's requests answered with exactly `status`, which is what a
    /// per-status-code rate is.
    fn status_percent(&self, traffic: &[Viewer], status: u16) -> f64 {
        let answered = traffic
            .iter()
            .filter(|request| request.status == status)
            .count() as f64;
        100.0 * answered / self.requests
    }
}

/// The basic statistics every read of a series asks for here.
pub(crate) fn basic_statistics() -> Value {
    json!(["SampleCount", "Sum", "Average"])
}

/// Drive one mix through `subject`'s published domain name, asserting the edge answered each
/// request as the mix declares.
async fn drive(router: &MultiServiceRouter, case: &str, subject: &Subject, traffic: &[Viewer]) {
    for request in traffic {
        let (status, _bytes) = view(router, request.method, &subject.domain, request.path).await;
        assert_eq!(
            status, request.status,
            "case {case:?} ({}): expected the edge to answer {}, got {status}",
            request.case, request.status
        );
    }
}

/// Assert that `metric` holds NOTHING for `subject`: no datapoint for a basic-statistics read,
/// none for a percentile read, and no series listed at all.
async fn assert_withheld(
    router: &MultiServiceRouter,
    case: &str,
    subject: &Subject,
    metric: &str,
    at: f64,
) {
    let basic = datapoints(router, case, &subject.id, metric, at, basic_statistics()).await;
    assert!(
        basic.is_empty(),
        "case {case:?} ({metric}): a distribution with no monitoring subscription publishes this \
         series in no minute, got {basic:?}"
    );
    let percentile = datapoints(
        router,
        case,
        &subject.id,
        metric,
        at,
        Value::String(PERCENTILE.to_string()),
    )
    .await;
    assert!(
        percentile.is_empty(),
        "case {case:?} ({metric}): a withheld series answers no {PERCENTILE} either, got \
         {percentile:?}"
    );
    let series = listed(router, case, &subject.id, metric).await;
    assert!(
        series.is_empty(),
        "case {case:?} ({metric}): a withheld series is listed nowhere, got {series:?}"
    );
}

#[tokio::test]
async fn the_gated_series_follow_the_subscription_and_the_default_series_follow_the_traffic() {
    let router = emulator();
    let rows = mixes();

    // Both siblings of every row are created first, so the clock owes them their deployment
    // while the suite waits once for all of them.
    let mut pairs: Vec<(Subject, Subject)> = Vec::new();
    for index in 0..rows.len() {
        let subscribed = created(&router, &format!("gating-subscribed-{index}")).await;
        let control = created(&router, &format!("gating-control-{index}")).await;
        pairs.push((subscribed, control));
    }

    for (row, (subscribed, control)) in rows.iter().zip(pairs.iter()) {
        let case = row.case;
        awaited_deployment(&router, case, &subscribed.id).await;
        awaited_deployment(&router, case, &control.id).await;
        subscribe(&router, case, &subscribed.id).await;

        // What the two siblings differ in, as the control plane reports it.
        let enabled = read_subscription(&router, &subscribed.id).await;
        assert_eq!(
            enabled.root().find_text("RealtimeMetricsSubscriptionStatus"),
            Some(ENABLED),
            "case {case:?}: the subscribed distribution must read back {ENABLED}, got {}",
            enabled.raw
        );
        let refused = read_subscription(&router, &control.id).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: a distribution carrying no subscription is refused, got {}: {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "NoSuchMonitoringSubscription",
            "case {case:?}: expected NoSuchMonitoringSubscription, got {}",
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message"),
            Some(NO_SUCH_SUBSCRIPTION_MESSAGE),
            "case {case:?}: expected the refusal to carry {NO_SUCH_SUBSCRIPTION_MESSAGE:?}, got {}",
            refused.raw
        );

        // IDENTICAL traffic through both, so the only difference between what they report is
        // the subscription.
        let at = now_secs();
        drive(&router, case, subscribed, &row.traffic).await;
        drive(&router, case, control, &row.traffic).await;
        let expected = Expected::of(&row.traffic);

        // The DEFAULT series are published for BOTH, and agree with what each served.
        for (which, subject) in [("subscribed", subscribed), ("control", control)] {
            let counted = held(
                &router,
                case,
                &subject.id,
                edge_metrics::REQUESTS_METRIC,
                at,
            )
            .await;
            assert_eq!(
                counted.sum, expected.requests,
                "case {case:?} ({which}): expected {} counted viewer requests, got {counted:?}",
                expected.requests
            );
            let four_xx = held(
                &router,
                case,
                &subject.id,
                edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
                at,
            )
            .await;
            assert!(
                (four_xx.average() - expected.four_xx_percent).abs() < f64::EPSILON,
                "case {case:?} ({which}): expected a 4xx rate of {}, got {four_xx:?}",
                expected.four_xx_percent
            );
            for metric in edge_metrics::DISTRIBUTION_METRICS {
                let series = listed(&router, case, &subject.id, metric).await;
                assert_eq!(
                    series.len(),
                    1,
                    "case {case:?} ({which}, {metric}): a default series is listed whatever the \
                     subscription says, got {series:?}"
                );
            }
        }

        // The ADDITIONAL series hold, for the SUBSCRIBED sibling, exactly what this traffic is
        // a measure of — and nothing at all for the sibling that is not subscribed.
        let hit_rate = held(
            &router,
            case,
            &subscribed.id,
            edge_metrics::CACHE_HIT_RATE_METRIC,
            at,
        )
        .await;
        assert_eq!(
            hit_rate.samples, expected.hit_samples,
            "case {case:?}: the hit rate is a rate of the cacheable requests alone ({}), got \
             {hit_rate:?}",
            expected.hit_samples
        );
        match expected.hit_percent {
            Some(percent) => assert!(
                (hit_rate.average() - percent).abs() < f64::EPSILON,
                "case {case:?}: expected a hit rate of {percent}, got {hit_rate:?}"
            ),
            None => {
                let empty = datapoints(
                    &router,
                    case,
                    &subscribed.id,
                    edge_metrics::CACHE_HIT_RATE_METRIC,
                    at,
                    basic_statistics(),
                )
                .await;
                assert!(
                    empty.is_empty(),
                    "case {case:?}: traffic with no cacheable request has no hit rate rather \
                     than a rate of zero, got {empty:?}"
                );
            }
        }

        let latency = held(
            &router,
            case,
            &subscribed.id,
            edge_metrics::ORIGIN_LATENCY_METRIC,
            at,
        )
        .await;
        assert_eq!(
            latency.samples, expected.origin_samples,
            "case {case:?}: the origin latency is published for the requests an origin answered \
             ({}), got {latency:?}",
            expected.origin_samples
        );
        let percentile = datapoints(
            &router,
            case,
            &subscribed.id,
            edge_metrics::ORIGIN_LATENCY_METRIC,
            at,
            Value::String(PERCENTILE.to_string()),
        )
        .await;
        if expected.origin_samples > 0.0 {
            assert_eq!(
                latency.units,
                vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
                "case {case:?}: an origin latency is a span of time, got {latency:?}"
            );
            assert!(
                !percentile.is_empty(),
                "case {case:?}: a published latency series answers the {PERCENTILE} a dashboard \
                 reads it at, got {percentile:?}"
            );
            for datapoint in &percentile {
                let value = datapoint["ExtendedStatistics"][PERCENTILE].as_f64();
                assert!(
                    value.is_some_and(|measured| measured >= 0.0 && measured.is_finite()),
                    "case {case:?}: expected a measured {PERCENTILE} latency, got {datapoint:?}"
                );
            }
        } else {
            assert!(
                percentile.is_empty(),
                "case {case:?}: traffic no origin answered has no latency at all, got \
                 {percentile:?}"
            );
        }

        // One series per status code CloudFront reports a rate for: every request observes
        // each of them, and the rate is the share answered with that one status.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&router, case, &subscribed.id, metric, at).await;
            let percent = expected.status_percent(&row.traffic, code);
            assert_eq!(
                rate.samples, expected.requests,
                "case {case:?} ({metric}): every request observes a per-status rate, got {rate:?}"
            );
            assert!(
                (rate.average() - percent).abs() < f64::EPSILON,
                "case {case:?} ({metric}): expected a rate of {percent} for the requests \
                 answered {code}, got {rate:?}"
            );
        }

        // Every gated series the subscription bought is LISTED for the subscribed sibling
        // exactly where it holds a datapoint, and for the control none of them is listed, read
        // or answered at any statistic.
        for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
            let published = datapoints(
                &router,
                case,
                &subscribed.id,
                metric,
                at,
                basic_statistics(),
            )
            .await;
            let series = listed(&router, case, &subscribed.id, metric).await;
            assert_eq!(
                series.is_empty(),
                published.is_empty(),
                "case {case:?} ({metric}): what a client lists must agree with what it can read, \
                 got listed {series:?} for datapoints {published:?}"
            );
            assert_withheld(&router, case, control, metric, at).await;
        }
    }
}
