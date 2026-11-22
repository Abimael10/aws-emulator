// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a deployed REST API stage publishes about the requests it ANSWERED.
//!
//! A request a stage answered leaves no response a later caller can read: nothing in the
//! control plane reports that a client called a method, which status it got or how long it
//! waited. The only way a client learns any of it — a dashboard, a CloudWatch alarm, an
//! on-call rule — is the series API Gateway publishes about the stage itself, so a stage that
//! serves traffic and publishes nothing leaves every alarm over its traffic inert.
//!
//! WHAT IS COUNTED. Every request the stage ANSWERED is one observation of [`COUNT_METRIC`],
//! and one observation of each error counter — [`CLIENT_ERROR_METRIC`] for the client-error
//! class and [`SERVER_ERROR_METRIC`] for the server-error class (RFC 9110 §15.5, §15.6) —
//! which is [`ONE_REQUEST`] for a request whose status is in that class and [`NO_REQUEST`] for
//! one that is not. Both counters are published for EVERY request, which is what makes the
//! `Sum` over a span how many requests of that class the span held and the `Average` the share
//! of them, and what tells "nothing failed" from "this stage is publishing nothing at all" — a
//! gap no dashboard and no alarm can act on. Beside them goes the measurement of the same
//! request: how long answering it took end to end ([`LATENCY_METRIC`]), how long the method's
//! INTEGRATION took to answer the call the gateway relayed to it
//! ([`INTEGRATION_LATENCY_METRIC`], one sample per request an integration answered), and —
//! for a method whose settings cache its responses — whether the stage's cache answered it
//! ([`CACHE_HIT_METRIC`]) or the integration had to ([`CACHE_MISS_METRIC`]).
//!
//! The ANSWER is what attributes a request to a counter, not what produced it: a request the
//! api refused itself — an authorization it did not carry, a payload the validator rejected, a
//! usage plan that shed it, a path the deployment does not hold — is a request the stage
//! answered, so it is counted like one an integration answered and the status alone decides
//! which counter it lands in. WHAT IS NOT COUNTED is a request no stage of a stored api served
//! at all ([`planned`]): a host or path this plane resolves to no api, an api whose
//! `execute-api` endpoint is turned off, a stage the api does not hold and a stage whose
//! deployment is gone. Each of those is answered without a stage, so none is traffic of one.
//!
//! WHICH DIMENSIONS. The api's own series first — [`API_NAME_DIMENSION`] alone, so a reader
//! filtering on the api sees its whole traffic — then [`API_NAME_DIMENSION`] with
//! [`STAGE_DIMENSION`], so one stage's traffic is separable from another's. Then, and only for
//! a method whose settings turn CloudWatch metrics on ("specifies whether Amazon CloudWatch
//! metrics are enabled for this method", `MethodSetting$metricsEnabled`), the same
//! observations again under the method's own [`METHOD_DIMENSION`] and [`RESOURCE_DIMENSION`]
//! beside those two. The opt-in is the caller's instruction about the metrics and not about
//! the serving, so a stage that asks for nothing still serves its traffic and still publishes
//! the first two.
//!
//! The datapoints are appended to the CloudWatch store directly rather than sent through
//! `PutMetricData`: CloudWatch reserves the `AWS/` namespaces for the services that own them
//! ("you should not specify a namespace that begins with `AWS/`", `PutMetricData$Namespace`),
//! so a service's own metric never travels over that API. They go into the very store
//! `ListMetrics`, `GetMetricStatistics`, `GetMetricData` and the alarm evaluation read, so one
//! metric and its readers cannot disagree — and storing one is what makes the evaluation of an
//! alarm over the series it landed in due ([`stored`]), exactly as an ingested datapoint does,
//! because an alarm is evaluated FROM the datapoints of the series it watches.
//!
//! No count is kept here. Each observation is a datapoint of what ONE request did, so the
//! total a client reads is the `Sum` CloudWatch computes over a period and the number of
//! requests is its `SampleCount`.

use std::time::Duration;

use super::{RtRequest, RtResponse};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

/// The CloudWatch store an API Gateway stage's own metrics live in: the CloudWatch service's
/// OWN state struct, reached through the SAME shared engine this service's store lives in.
type CloudWatchState =
    ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ComAmazonawsCloudwatchGraniteserviceversion20100801State;

/// The namespace API Gateway publishes a stage's own metrics to.
pub(super) const METRIC_NAMESPACE: &str = "AWS/ApiGateway";

/// The dimension that names the api every one of these series is of. A REST API's series are
/// dimensioned on the api's NAME (`RestApi$name`) rather than its id, which is what a reader
/// of these series filters on.
pub(super) const API_NAME_DIMENSION: &str = "ApiName";

/// The dimension that names the stage that answered the request (`Stage$stageName`).
pub(super) const STAGE_DIMENSION: &str = "Stage";

/// The dimension that names the VERB of the deployed method a per-method series is of.
pub(super) const METHOD_DIMENSION: &str = "Method";

/// The dimension that names the deployed RESOURCE PATH of that method.
pub(super) const RESOURCE_DIMENSION: &str = "Resource";

/// How many requests the series' subject answered.
pub(super) const COUNT_METRIC: &str = "Count";

/// How many of them were answered with a client-error status.
pub(super) const CLIENT_ERROR_METRIC: &str = "4XXError";

/// How many of them were answered with a server-error status.
pub(super) const SERVER_ERROR_METRIC: &str = "5XXError";

/// How long answering one request took, end to end.
pub(super) const LATENCY_METRIC: &str = "Latency";

/// How long the method's INTEGRATION took to answer the call the gateway relayed to it — one
/// sample per request an integration answered, so this series' `SampleCount` is how many calls
/// an integration answered rather than how many requests the stage served.
pub(super) const INTEGRATION_LATENCY_METRIC: &str = "IntegrationLatency";

/// How many requests the stage's own cache answered.
pub(super) const CACHE_HIT_METRIC: &str = "CacheHitCount";

/// How many requests the stage's cache was consulted for and did NOT answer, so the
/// integration was performed for them.
pub(super) const CACHE_MISS_METRIC: &str = "CacheMissCount";

/// The `StandardUnit` a bare count of things carries: `Count`.
const COUNT_UNIT: &str = "Count";

/// The `StandardUnit` a measured duration carries: `Milliseconds`, the unit API Gateway's own
/// latency series are expressed in.
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// What ONE request contributes to a counter it belongs to, and what it contributes to one it
/// does not: a counter counts requests, so each one is a single observation, and a request
/// outside a counter's class is observed as none of them rather than left unpublished.
const ONE_REQUEST: f64 = 1.0;
const NO_REQUEST: f64 = 0.0;

/// Seconds hold a thousand milliseconds: a latency measured as a duration is reported in the
/// unit [`MILLISECONDS_UNIT`] names, with the sub-millisecond part kept.
const MILLIS_PER_SECOND: f64 = 1_000.0;

/// The `StorageResolution` these datapoints are published at, in seconds: regular resolution,
/// "which CloudWatch stores at 1-minute resolution" (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// The statuses [`CLIENT_ERROR_METRIC`] counts: the client-error class, "the 4xx (Client
/// Error) class of status code" (RFC 9110 §15.5).
const CLIENT_ERROR_STATUSES: ::std::ops::RangeInclusive<u16> = 400..=499;

/// The statuses [`SERVER_ERROR_METRIC`] counts: the server-error class, "the 5xx (Server
/// Error) class of status code" (RFC 9110 §15.6).
const SERVER_ERROR_STATUSES: ::std::ops::RangeInclusive<u16> = 500..=599;

/// What the stage's CACHE did with one request, for the two counters that report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheOutcome {
    /// The cache HELD this call's response, so the integration was never performed for it.
    Hit,
    /// The cache was consulted for this call and held nothing usable, so the integration
    /// answered it.
    Miss,
}

/// What serving ONE request OBSERVED that nothing about the answer carries: whether the
/// stage's cache answered it, and how long the method's integration took to answer the call.
///
/// Filled by the serving path as it decides the request and read here, for the same reason the
/// authorization values are carried out of it: neither is recoverable from the response a
/// client holds, and both are measurements of THIS request rather than of its records.
#[derive(Debug, Default)]
pub(super) struct Observed {
    /// What the stage's cache did with the request, and `None` for a request whose method
    /// caches nothing — which is what keeps the two cache counters to the methods that have
    /// a cache at all.
    cache: Option<CacheOutcome>,
    /// How long the method's integration took to answer, and `None` for a request no
    /// integration answered: one the cache answered, and one the api refused before it.
    integration_latency: Option<Duration>,
}

impl Observed {
    /// Record that the stage's cache HELD this call's response.
    pub(super) fn cache_hit(&mut self) {
        self.cache = Some(CacheOutcome::Hit);
    }

    /// Record that the stage's cache was consulted for this call and did not answer it.
    pub(super) fn cache_missed(&mut self) {
        self.cache = Some(CacheOutcome::Miss);
    }

    /// Record how long the method's INTEGRATION took to answer the call relayed to it.
    pub(super) fn integration_answered_in(&mut self, elapsed: Duration) {
        self.integration_latency = Some(elapsed);
    }
}

/// Publish what the stage has just answered into every series the request belongs to.
///
/// Called from the ONE funnel every invoke of this data plane leaves through, with the
/// response in hand: which counter a request lands in and how long answering it took are facts
/// of the ANSWER, so they cannot be known before it.
///
/// `latency` is how long the whole invoke took, which is what the stage reports as the latency
/// of answering this request — the same measurement its access log renders.
///
/// Nothing here can fail the request: the stage has already answered, so an observation that
/// could not be stored is reported where a developer can see it and nowhere else.
pub(super) fn published<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    response: &RtResponse,
    latency: Duration,
    observed: &Observed,
) {
    let Some(planned) = planned(engine, req, response, latency, observed) else {
        return;
    };
    // An engine carrying no CloudWatch at all has no store to publish into, and nothing about
    // publishing a metric may fail the request the stage has already answered.
    let Some(store) = cloudwatch::service_state(engine) else {
        return;
    };
    stored(&store, &planned);
}

/// One observation of one metric: its value and the unit that value is in.
#[derive(Debug, Clone, PartialEq)]
struct Observation {
    metric_name: &'static str,
    value: f64,
    unit: &'static str,
}

/// Everything to publish for one answered request: which series it belongs to and what each
/// of them observes.
#[derive(Debug)]
struct Planned {
    /// The api the request was answered for, for the one thing that can go wrong here.
    api_name: String,
    /// The dimension sets this request is counted under.
    series: Vec<cloudwatch::DimensionSet>,
    /// The observations each of those series takes of it.
    observations: Vec<Observation>,
}

/// The series this request belongs to, read under ONE shared read of this service's store,
/// which is RELEASED before the datapoints are stored: the metric store belongs to another
/// service, and no lock of this one is held while that one is written.
///
/// `None` when no stage of a stored api served the request — see the module docs — so there is
/// no stage whose traffic it is and nothing to count it in. The resolution is the serving's
/// own ([`super::resolve`], [`super::split_stage`] and the deployment the stage serves), so a
/// request is attributed to the stage that answered it and to no other.
fn planned<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    response: &RtResponse,
    latency: Duration,
    observed: &Observed,
) -> Option<Planned> {
    let state = crate::state::service_state(engine)?;
    // Which api, stage and resource path this request reaches is the same reading the serving
    // makes, so the request a custom domain name's front door relayed is counted under the
    // api and stage its base path mapping names — read, like there, before the store is held.
    let addressed = super::Addressed::of_request(req);
    let arrival = super::custom_domain_arrival::arrival_of(&state, addressed.host, addressed.path);
    let addressed = addressed.arriving_on(arrival.as_ref());
    let store = state.lock();
    let resolved = super::resolve(&store, &addressed)?;
    let api = resolved.api;
    let (stage_name, resource_path) = super::split_stage(resolved.invoke_path)?;
    let stage = api.stages.get(stage_name)?;
    let deployment = api.deployments.get(&stage.deployment_id)?;
    let mut series = vec![api_series(&api.name)];
    let mut stage_series = api_series(&api.name);
    stage_series.insert(STAGE_DIMENSION.to_string(), stage_name.to_string());
    series.push(stage_series.clone());
    // WHICH deployed method the request reached, where it reached one — read the way the
    // serving itself reads it — because the per-method series are of that method: a request
    // that matched none has no verb and no resource path of the deployment to name, so it is
    // counted in the two stage-level series alone.
    let matched = super::match_resource_path(deployment, &resource_path).and_then(|path| {
        let (verb, _) = super::match_method(deployment, path, req.method())?;
        Some((path, verb))
    });
    if let Some((path, verb)) = matched {
        if super::response_cache::method_settings_in_force(stage, path, verb)
            .is_some_and(|settings| settings.metrics_enabled)
        {
            let mut method_series = stage_series;
            method_series.insert(METHOD_DIMENSION.to_string(), verb.to_string());
            method_series.insert(RESOURCE_DIMENSION.to_string(), path.to_string());
            series.push(method_series);
        }
    }
    Some(Planned {
        api_name: api.name.clone(),
        series,
        observations: observations(response.status().as_u16(), latency, observed),
    })
}

/// The dimension set that names the api alone, which every other set of these series extends.
fn api_series(api_name: &str) -> cloudwatch::DimensionSet {
    [(API_NAME_DIMENSION.to_string(), api_name.to_string())]
        .into_iter()
        .collect()
}

/// What the series take of a request answered with `status` in `latency`, as `observed`
/// measured serving it.
///
/// The integration's own latency is observed only for a request an integration ANSWERED: the
/// measurement is the time between the gateway relaying the call and the answer reaching it,
/// so a request no integration answered has none to report rather than a zero — which is what
/// keeps that series' `SampleCount` the number of calls an integration answered. The two cache
/// counters are published only for a request whose method HAS a cache, and then both of them
/// are, so the share of the method's traffic the cache answered is readable from either.
fn observations(status: u16, latency: Duration, observed: &Observed) -> Vec<Observation> {
    let counted = |in_class: bool| {
        if in_class {
            ONE_REQUEST
        } else {
            NO_REQUEST
        }
    };
    let mut observations = vec![
        Observation {
            metric_name: COUNT_METRIC,
            value: ONE_REQUEST,
            unit: COUNT_UNIT,
        },
        Observation {
            metric_name: CLIENT_ERROR_METRIC,
            value: counted(CLIENT_ERROR_STATUSES.contains(&status)),
            unit: COUNT_UNIT,
        },
        Observation {
            metric_name: SERVER_ERROR_METRIC,
            value: counted(SERVER_ERROR_STATUSES.contains(&status)),
            unit: COUNT_UNIT,
        },
        Observation {
            metric_name: LATENCY_METRIC,
            value: latency.as_secs_f64() * MILLIS_PER_SECOND,
            unit: MILLISECONDS_UNIT,
        },
    ];
    if let Some(answered_in) = observed.integration_latency {
        observations.push(Observation {
            metric_name: INTEGRATION_LATENCY_METRIC,
            value: answered_in.as_secs_f64() * MILLIS_PER_SECOND,
            unit: MILLISECONDS_UNIT,
        });
    }
    if let Some(cache) = observed.cache {
        observations.push(Observation {
            metric_name: CACHE_HIT_METRIC,
            value: counted(cache == CacheOutcome::Hit),
            unit: COUNT_UNIT,
        });
        observations.push(Observation {
            metric_name: CACHE_MISS_METRIC,
            value: counted(cache == CacheOutcome::Miss),
            unit: COUNT_UNIT,
        });
    }
    observations
}

/// Store what `planned` holds in the CloudWatch metric store of the SAME shared engine, and
/// make every alarm watching a series it touched due for evaluation.
///
/// Storing a datapoint owes more than the storing: an alarm is evaluated FROM the datapoints
/// of the series it watches, so the write that stores one is what makes that alarm's
/// evaluation due — the same thing an ingested `PutMetricData` datapoint owes the alarms over
/// the series it lands in, which is why a service's own series cannot skip it. An alarm over a
/// stage's traffic would otherwise be graded only by the store's own scheduled sweep: the
/// transition the traffic decided would arrive later than the traffic that decided it, and a
/// client watching an api's error series could not tell an alarm that is late from a pipeline
/// that never ran at all.
fn stored(store: &::engine::ServiceState<CloudWatchState>, planned: &Planned) {
    let at_secs = cloudwatch::now_secs();
    let mut written: Vec<cloudwatch::WrittenSeries> =
        Vec::with_capacity(planned.series.len() * planned.observations.len());
    let mut store = store.lock();
    for dimensions in &planned.series {
        for observation in &planned.observations {
            let key = cloudwatch::MetricKey {
                namespace: METRIC_NAMESPACE.to_string(),
                metric_name: observation.metric_name.to_string(),
                dimensions: dimensions.clone(),
            };
            let datum = cloudwatch::StoredDatum {
                timestamp_secs: at_secs,
                sample_count: ONE_REQUEST,
                sum: observation.value,
                minimum: observation.value,
                maximum: observation.value,
                unit: Some(observation.unit.to_string()),
                storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
            };
            let units = vec![datum.unit().to_string()];
            if store.append_datapoints(key.clone(), vec![datum]).is_err() {
                // The metric store holds no more series. The request itself was already
                // answered, so nothing is failed for it; the dropped observation is reported
                // where a developer can see it.
                ::tracing::warn!(
                    api = %planned.api_name,
                    metric = %observation.metric_name,
                    "the CloudWatch metric store is at capacity, so this request was not counted"
                );
                continue;
            }
            written.push(cloudwatch::WrittenSeries { key, units });
        }
    }
    store.make_alarms_due(&written, at_secs);
}

#[cfg(test)]
mod tests {
    //! What ONE answered request is observed as. The published series themselves are pinned
    //! end to end over the wire in `core/router/tests/all/apigateway_stage_request_metrics.rs`,
    //! where the metric store they land in is the CloudWatch emulator's own.

    use super::*;

    /// The value `metric` was observed at for a request answered with `status`, out of what
    /// `observed` measured serving it; `None` when the request is observed in no such series.
    fn observed_value(status: u16, observed: &Observed, metric: &str) -> Option<f64> {
        observations(status, Duration::from_millis(7), observed)
            .into_iter()
            .find(|observation| observation.metric_name == metric)
            .map(|observation| observation.value)
    }

    #[test]
    fn every_answered_request_is_counted_and_its_class_decides_the_error_counters() {
        // Rows span the status classes a stage answers a request with: what an integration
        // produced, the redirect an upstream is answered as, each class of refusal this data
        // plane words itself, and the statuses an integration that produced nothing earns.
        let cases: [(u16, f64, f64); 10] = [
            (200, NO_REQUEST, NO_REQUEST),
            (201, NO_REQUEST, NO_REQUEST),
            (302, NO_REQUEST, NO_REQUEST),
            (400, ONE_REQUEST, NO_REQUEST),
            (403, ONE_REQUEST, NO_REQUEST),
            (415, ONE_REQUEST, NO_REQUEST),
            (429, ONE_REQUEST, NO_REQUEST),
            (500, NO_REQUEST, ONE_REQUEST),
            (502, NO_REQUEST, ONE_REQUEST),
            (504, NO_REQUEST, ONE_REQUEST),
        ];
        for (status, client, server) in cases {
            let observed = Observed::default();
            let counted = observed_value(status, &observed, COUNT_METRIC);
            assert_eq!(
                counted,
                Some(ONE_REQUEST),
                "case {status}: expected one request counted, got {counted:?}"
            );
            let actual = observed_value(status, &observed, CLIENT_ERROR_METRIC);
            assert_eq!(
                actual,
                Some(client),
                "case {status}: expected the {CLIENT_ERROR_METRIC} counter at {client}, got \
                 {actual:?}"
            );
            let actual = observed_value(status, &observed, SERVER_ERROR_METRIC);
            assert_eq!(
                actual,
                Some(server),
                "case {status}: expected the {SERVER_ERROR_METRIC} counter at {server}, got \
                 {actual:?}"
            );
        }
    }

    #[test]
    fn the_integration_latency_is_observed_exactly_for_a_request_an_integration_answered() {
        // Rows differ along WHETHER an integration answered the call and how long it took: an
        // answer that came back in no measurable time at all is still an answer, a longer one
        // is the same observation in milliseconds, and a request no integration answered — one
        // the cache answered, one the api refused — has no latency to report at all.
        let cases: [(&str, Option<Duration>, Option<f64>); 4] = [
            (
                "an integration that answered at once",
                Some(Duration::ZERO),
                Some(0.0),
            ),
            (
                "an integration that answered in sub-millisecond time",
                Some(Duration::from_micros(1_500)),
                Some(1.5),
            ),
            (
                "an integration that took a while",
                Some(Duration::from_millis(250)),
                Some(250.0),
            ),
            ("a request no integration answered", None, None),
        ];
        for (case, answered_in, expected) in cases {
            let mut observed = Observed::default();
            if let Some(answered_in) = answered_in {
                observed.integration_answered_in(answered_in);
            }
            let actual = observed_value(200, &observed, INTEGRATION_LATENCY_METRIC);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the {INTEGRATION_LATENCY_METRIC} observation \
                 {expected:?}, got {actual:?}"
            );
        }
    }

    /// One row of the cache-counter table: what the cache did with the request, and what each
    /// of the two counters then observes.
    struct CacheCase {
        case: &'static str,
        outcome: Option<CacheOutcome>,
        hits: Option<f64>,
        misses: Option<f64>,
    }

    #[test]
    fn the_cache_counters_are_published_only_for_a_method_that_has_a_cache() {
        // Rows span what the stage's cache did with the request: it answered it, it was
        // consulted and did not, and the method caches nothing at all.
        let cases = [
            CacheCase {
                case: "a request the cache answered",
                outcome: Some(CacheOutcome::Hit),
                hits: Some(ONE_REQUEST),
                misses: Some(NO_REQUEST),
            },
            CacheCase {
                case: "a request the cache held nothing for",
                outcome: Some(CacheOutcome::Miss),
                hits: Some(NO_REQUEST),
                misses: Some(ONE_REQUEST),
            },
            CacheCase {
                case: "a method that caches nothing",
                outcome: None,
                hits: None,
                misses: None,
            },
        ];
        for CacheCase {
            case,
            outcome,
            hits,
            misses,
        } in cases
        {
            let mut observed = Observed::default();
            match outcome {
                Some(CacheOutcome::Hit) => observed.cache_hit(),
                Some(CacheOutcome::Miss) => observed.cache_missed(),
                None => {}
            }
            let actual = observed_value(200, &observed, CACHE_HIT_METRIC);
            assert_eq!(
                actual, hits,
                "case {case:?}: expected the {CACHE_HIT_METRIC} observation {hits:?}, got \
                 {actual:?}"
            );
            let actual = observed_value(200, &observed, CACHE_MISS_METRIC);
            assert_eq!(
                actual, misses,
                "case {case:?}: expected the {CACHE_MISS_METRIC} observation {misses:?}, got \
                 {actual:?}"
            );
        }
    }

    #[test]
    fn the_latency_of_answering_one_request_is_observed_in_milliseconds() {
        // Rows span the spans answering a request can take, including the sub-millisecond one
        // every in-process answer takes.
        let cases: [(Duration, f64); 4] = [
            (Duration::ZERO, 0.0),
            (Duration::from_micros(250), 0.25),
            (Duration::from_millis(12), 12.0),
            (Duration::from_secs(3), 3_000.0),
        ];
        for (latency, expected) in cases {
            let actual = observations(200, latency, &Observed::default())
                .into_iter()
                .find(|observation| observation.metric_name == LATENCY_METRIC)
                .expect("the latency observation");
            assert_eq!(
                actual.value, expected,
                "case {latency:?}: expected {expected} {MILLISECONDS_UNIT}, got {actual:?}"
            );
            assert_eq!(
                actual.unit, MILLISECONDS_UNIT,
                "case {latency:?}: expected the unit {MILLISECONDS_UNIT}, got {actual:?}"
            );
        }
    }
}
