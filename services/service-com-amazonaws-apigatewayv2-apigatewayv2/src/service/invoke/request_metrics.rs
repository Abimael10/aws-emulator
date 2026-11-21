// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series API Gateway publishes about the requests an HTTP api's stage
//! ANSWERED.
//!
//! WHAT IS COUNTED. Every request a ROUTE of a deployed stage served is one observation of
//! [`COUNT_METRIC`], and one observation of each error counter — [`CLIENT_ERROR_METRIC`] for
//! the 4xx class and [`SERVER_ERROR_METRIC`] for the 5xx class (RFC 9110 §15.5, §15.6) — which
//! is [`ONE_REQUEST`] for a request whose status is in that class and [`NO_REQUEST`] for one
//! that is not. Both counters are published for EVERY such request, which is what makes the
//! `Sum` over a span how many requests of that class the span held and the `Average` the share
//! of them. The route the stage matched is what attributes a request to the stage's series, not
//! the answer it ended in: a route the stage's `RouteSettings` throttled, one an authorizer
//! refused and one whose backend produced nothing were all served BY a route, so they are
//! counted like one a backend answered and the status alone decides which counter they land in.
//! Beside the counters go the measurements of the same request: how long answering it took
//! ([`LATENCY_METRIC`]), how long the route's BACKEND took to answer the invocation the gateway
//! relayed to it ([`INTEGRATION_LATENCY_METRIC`], one sample per request a backend answered),
//! and how many payload bytes it carried in and out ([`DATA_PROCESSED_METRIC`]).
//!
//! WHAT IS NOT COUNTED is a request NO route of the stage served ([`routed`]): a path no route
//! key matches, a stage the api does not hold, a stage with no deployment released to it, and
//! an endpoint the api turned off. The gateway answers each of those itself, out of the api's
//! own records and without selecting a route, so none of them is traffic of a stage's routes
//! and none of them appears in any of these series — a client reading `Count` reads the
//! requests the stage routed and no others.
//!
//! WHICH DIMENSIONS. The stage-level series first: [`API_ID_DIMENSION`] alone, so a reader
//! filtering on the api sees its whole traffic, and [`API_ID_DIMENSION`] with
//! [`STAGE_DIMENSION`], so one stage's traffic is separable from another's. Then — and only
//! for a route whose serving stage asks for it with `RouteSettings$DetailedMetricsEnabled`
//! ("specifies whether detailed metrics are enabled") — the same observations again under the
//! route's own dimensions, [`METHOD_DIMENSION`] and [`RESOURCE_DIMENSION`] naming the two
//! halves of its `RouteKey` ([`route_dimensions`]). The opt-in is the caller's instruction
//! about the metrics and not about the serving, so a stage that asks for nothing still serves
//! its traffic and still publishes the stage-level series.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the same store
//! `ListMetrics`, `GetMetricStatistics` and the alarm evaluation read — reached through the
//! shared engine, so one metric has one home and the publisher and its readers cannot drift.
//! They never travel over `PutMetricData`, which refuses a publish into a namespace the
//! services own ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricDataInput$Namespace`). The namespace, the two stage-level dimensions, the
//! `DetailedMetricsEnabled` reading and the write itself are this service's ONE set of them
//! ([`crate::service::metrics`]), shared with the per-call series of a WEBSOCKET api's stage;
//! storing a datapoint there is also what owes every alarm over these series its evaluation.
//! What THIS module owns is which observations an answered REQUEST produces.

use std::time::Duration;

use super::{RtRequest, RtResponse};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use crate::service::metrics;
use crate::service::metrics::{COUNT_UNIT, MILLISECONDS_UNIT};

/// The dimension naming the api whose traffic a datapoint is about — `Api$ApiId`, "the API
/// identifier". The api's own identifier is what separates an HTTP api's series from those of
/// every other kind of api AWS's API Gateway serves inside the one namespace
/// ([`metrics::METRIC_NAMESPACE`]), which is why it is part of every series below.
pub use metrics::API_ID_DIMENSION;

/// The dimension naming the stage that answered it — `Stage$StageName`.
pub use metrics::STAGE_DIMENSION;

/// The dimension naming the verb half of the answering route's `RouteKey`.
pub const METHOD_DIMENSION: &str = "Method";

/// The dimension naming the path half of it.
pub const RESOURCE_DIMENSION: &str = "Resource";

/// How many requests the series' subject answered.
pub const COUNT_METRIC: &str = "Count";

/// How many of them were answered with a client-error status.
pub const CLIENT_ERROR_METRIC: &str = "4xx";

/// How many of them were answered with a server-error status.
pub const SERVER_ERROR_METRIC: &str = "5xx";

/// How long answering one request took, end to end.
pub const LATENCY_METRIC: &str = "Latency";

/// How long the route's BACKEND took to answer the invocation the gateway relayed to it — the
/// series both data planes of this service publish that measurement in
/// ([`metrics::INTEGRATION_LATENCY_METRIC`]), so an api's backend latency has one name whichever
/// plane served the call.
pub use metrics::INTEGRATION_LATENCY_METRIC;

/// How many payload bytes answering it carried, in and out.
pub const DATA_PROCESSED_METRIC: &str = "DataProcessed";

/// The unit a payload size is published in. The two the counters and the latency are published
/// in are the crate's own ([`COUNT_UNIT`], [`MILLISECONDS_UNIT`]).
const BYTES_UNIT: &str = "Bytes";

/// What ONE request contributes to a counter it belongs to, and what it contributes to one it
/// does not: a counter counts requests, so each one is a single observation, and a request
/// outside a counter's class is observed as none of them rather than left unpublished — which
/// is what lets the `Average` of an error counter be the share of the traffic in that class.
const ONE_REQUEST: f64 = 1.0;
const NO_REQUEST: f64 = 0.0;

/// Seconds hold a thousand milliseconds: a latency measured as a duration is reported in the
/// unit [`MILLISECONDS_UNIT`] names, with the sub-millisecond part kept.
const MILLIS_PER_SECOND: f64 = 1_000.0;

/// Count the request this stage has just answered into every series it belongs to.
///
/// Called from the seams that serve this data plane with the ANSWER in hand: which counter a
/// request lands in and how long answering it took are facts of the answer, so they cannot be
/// known before it. A request no route of a stage served belongs to no series at all
/// ([`planned`]), so nothing is published for it.
///
/// `integration_latency` is how long the route's backend took to answer, for a request one
/// ANSWERED, and `None` for a request no backend answered ([`crate::service::invoke::dispatch`]
/// decides which) — which is what the backend-latency series is left out of.
///
/// Nothing here can fail the request: the stage has already answered, so an observation that
/// could not be stored is reported where a developer can see it and nothing else.
pub(super) fn published<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    response: &RtResponse,
    latency: Duration,
    integration_latency: Option<Duration>,
) {
    let Some(planned) = planned(engine, req, response, latency, integration_latency) else {
        return;
    };
    let mut data: Vec<metrics::Datapoint> =
        Vec::with_capacity(planned.series.len() * planned.observations.len());
    for dimensions in &planned.series {
        for observation in &planned.observations {
            data.push(metrics::datapoint(
                observation.metric_name,
                dimensions.clone(),
                observation.value,
                observation.unit,
            ));
        }
    }
    // The one write this service's datapoints go through, which is also what makes every alarm
    // over the series they land in due for its evaluation ([`super::metrics`]).
    super::metrics::stored(engine, &planned.api_id, data);
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
    /// The api the request was answered for, for the log line above.
    api_id: String,
    /// The dimension sets this request is counted under.
    series: Vec<cloudwatch::state::DimensionSet>,
    /// The observations each of those series takes of it.
    observations: Vec<Observation>,
}

/// The series this request belongs to, read under ONE shared read of this service's store,
/// which is RELEASED before the datapoints are stored: the metric store belongs to another
/// service, and no lock of this one is held while that one is written.
///
/// `None` when the request was served by NO route of this service at all — an endpoint that
/// resolves to nothing, and every request the gateway answered out of the api's records without
/// matching a route ([`routed`]) — so there is no stage whose traffic it is and nothing to
/// count it in.
fn planned<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    response: &RtResponse,
    latency: Duration,
    integration_latency: Option<Duration>,
) -> Option<Planned> {
    let state = crate::state::service_state(engine)?;
    let store = state.lock();
    let resolved = super::resolve(&store, &super::Addressed::of_request(req))?;
    let api = resolved.api;
    let (stage, route) = routed(api, resolved.invoke_path, req.method())?;
    let mut series = vec![api_series(&api.api_id)];
    let mut stage_series = api_series(&api.api_id);
    stage_series.insert(STAGE_DIMENSION.to_string(), stage.stage_name.clone());
    series.push(stage_series.clone());
    // The route's OWN series exist only for a route the serving stage asks for them for.
    if metrics::publishes_per_route(stage, &route.route_key) {
        let (method, resource) = route_dimensions(&route.route_key);
        let mut route_series = stage_series;
        route_series.insert(METHOD_DIMENSION.to_string(), method);
        route_series.insert(RESOURCE_DIMENSION.to_string(), resource);
        series.push(route_series);
    }
    Some(Planned {
        api_id: api.api_id.clone(),
        series,
        observations: observations(
            response.status().as_u16(),
            latency,
            request_payload_bytes(req).saturating_add(response_payload_bytes(response)),
            integration_latency,
        ),
    })
}

/// The stage of `api` and the ROUTE of it that served a request addressed at `invoke_path`
/// with `method`, or `None` for a request no route of the api served.
///
/// Which route matched is what attributes a request to a stage's series, so it is read the
/// way the serving itself reads it ([`super::routed_target`], the deployment released to that
/// stage, and [`super::matching_route`] over the routes that deployment froze) — a stage
/// pinned to an older deployment therefore meters the routes that deployment holds. Every
/// answer the gateway produced WITHOUT a route — an endpoint the api turned off
/// ([`crate::service::data_plane::serves_default_endpoint`]), a path naming no stage of the
/// api, a stage with no deployment released to it, and a path no route key matches — resolves
/// to no route here, which is what keeps it out of the series.
fn routed<'s>(
    api: &'s crate::state::Api,
    invoke_path: &str,
    method: &str,
) -> Option<(&'s crate::state::Stage, &'s crate::state::Route)> {
    if !crate::service::data_plane::serves_default_endpoint(api) {
        return None;
    }
    let (stage, route) = super::routed_target(api, invoke_path)?;
    let route = stage
        .deployment_id
        .as_deref()
        .and_then(|deployment_id| api.deployments.get(deployment_id))
        .and_then(|deployment| {
            super::matching_route(&deployment.configuration.routes, method, &route)
        })?;
    Some((stage, route))
}

/// The dimension set that names the api alone, which every other set of these series extends.
fn api_series(api_id: &str) -> cloudwatch::state::DimensionSet {
    [(API_ID_DIMENSION.to_string(), api_id.to_string())]
        .into_iter()
        .collect()
}

/// The `Method` and `Resource` dimensions of ONE route's own series, read off its `RouteKey`.
///
/// A route key is the verb and the path it answers for, written as one value — `GET /orders`,
/// or `ANY /orders` for a key that answers every verb — so the two dimensions are the two
/// halves of it. The `$default` route names neither half: it is "the default route" that
/// answers whatever no other route of the api matches, every verb included, so the key itself
/// is the resource it stands for and the verb it answers is the one API Gateway's own key
/// grammar spells as every verb ([`super::ANY_METHOD`]).
fn route_dimensions(route_key: &str) -> (String, String) {
    match route_key.split_once(' ') {
        Some((method, path)) => (method.trim().to_string(), path.trim().to_string()),
        None => (super::ANY_METHOD.to_string(), route_key.to_string()),
    }
}

/// What the series take of a request answered with `status`, in `latency`, carrying `bytes` of
/// payload in and out, whose backend took `integration_latency` to answer when one answered.
///
/// Which class the status falls in is the crate's ONE reading of each
/// ([`crate::service::data_plane::is_client_error_status`] and its server-error counterpart),
/// so the counter a request lands in and the refusal classes the other data plane meters by
/// cannot disagree about where a status belongs.
///
/// The backend's own latency is observed only for a request a backend ANSWERED: the measurement
/// is the time between the gateway relaying the request and the answer reaching it, so a
/// request no backend answered has none to report rather than a zero — which is what keeps the
/// series' `SampleCount` the number of invocations a backend answered.
fn observations(
    status: u16,
    latency: Duration,
    bytes: usize,
    integration_latency: Option<Duration>,
) -> Vec<Observation> {
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
            value: counted(crate::service::data_plane::is_client_error_status(status)),
            unit: COUNT_UNIT,
        },
        Observation {
            metric_name: SERVER_ERROR_METRIC,
            value: counted(crate::service::data_plane::is_server_error_status(status)),
            unit: COUNT_UNIT,
        },
        Observation {
            metric_name: LATENCY_METRIC,
            value: latency.as_secs_f64() * MILLIS_PER_SECOND,
            unit: MILLISECONDS_UNIT,
        },
        Observation {
            metric_name: DATA_PROCESSED_METRIC,
            value: bytes as f64,
            unit: BYTES_UNIT,
        },
    ];
    if let Some(answered_in) = integration_latency {
        observations.push(Observation {
            metric_name: INTEGRATION_LATENCY_METRIC,
            value: answered_in.as_secs_f64() * MILLIS_PER_SECOND,
            unit: MILLISECONDS_UNIT,
        });
    }
    observations
}

/// How many payload bytes the REQUEST carried, as the gateway measured them — the ONE reading
/// of a request's size this data plane has ([`super::payload_bytes`]), which the payload quota
/// refuses a request by. A request whose size nothing read carries no measurable payload, so
/// it contributes none.
fn request_payload_bytes(req: &RtRequest) -> usize {
    super::payload_bytes(req).unwrap_or_default()
}

/// How many payload bytes the ANSWER carried, which this endpoint renders in memory.
fn response_payload_bytes(response: &RtResponse) -> usize {
    response.body().bytes().map(<[u8]>::len).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    //! Which counters one answered request lands in, and which route a key names. The
    //! published series themselves are pinned end to end over the wire in
    //! `tests/all/http_api_request_metrics.rs`.

    use super::*;

    /// The value `metric` was observed at, out of what one answered request whose backend
    /// answered contributes.
    fn observed(status: u16, metric: &str) -> f64 {
        observations(
            status,
            Duration::from_millis(7),
            11,
            Some(Duration::from_millis(3)),
        )
        .into_iter()
        .find(|observation| observation.metric_name == metric)
        .map(|observation| observation.value)
        .unwrap_or_else(|| panic!("no {metric} observation at all"))
    }

    #[test]
    fn every_answered_request_is_counted_and_its_class_decides_the_error_counters() {
        // Rows span the status classes an invoke is answered with: the successes a backend
        // produced, the redirect an upstream the caller reaches is sent as, each refusal this
        // data plane words itself, and the status a backend that produced nothing earns.
        let cases: [(u16, f64, f64); 9] = [
            (200, NO_REQUEST, NO_REQUEST),
            (201, NO_REQUEST, NO_REQUEST),
            (307, NO_REQUEST, NO_REQUEST),
            (400, ONE_REQUEST, NO_REQUEST),
            (403, ONE_REQUEST, NO_REQUEST),
            (404, ONE_REQUEST, NO_REQUEST),
            (429, ONE_REQUEST, NO_REQUEST),
            (500, NO_REQUEST, ONE_REQUEST),
            (503, NO_REQUEST, ONE_REQUEST),
        ];
        for (status, client, server) in cases {
            let counted = observed(status, COUNT_METRIC);
            assert_eq!(
                counted, ONE_REQUEST,
                "case {status}: expected one request counted, got {counted}"
            );
            let actual = observed(status, CLIENT_ERROR_METRIC);
            assert_eq!(
                actual, client,
                "case {status}: expected the {CLIENT_ERROR_METRIC} counter at {client}, got \
                 {actual}"
            );
            let actual = observed(status, SERVER_ERROR_METRIC);
            assert_eq!(
                actual, server,
                "case {status}: expected the {SERVER_ERROR_METRIC} counter at {server}, got \
                 {actual}"
            );
        }
    }

    #[test]
    fn the_backend_latency_is_observed_exactly_for_a_request_a_backend_answered() {
        // Rows differ along WHETHER a backend answered the invocation and how long it took:
        // an answer that came back in no measurable time at all is still an answer, a longer
        // one is the same observation in milliseconds, and a request no backend answered has
        // no latency to report at all.
        let cases: [(&str, Option<Duration>, Option<f64>); 4] = [
            ("a backend that answered at once", Some(Duration::ZERO), Some(0.0)),
            (
                "a backend that answered in sub-millisecond time",
                Some(Duration::from_micros(1_500)),
                Some(1.5),
            ),
            (
                "a backend that took a while",
                Some(Duration::from_millis(250)),
                Some(250.0),
            ),
            ("a request no backend answered", None, None),
        ];
        for (case, answered_in, expected) in cases {
            let actual = observations(200, Duration::from_millis(9), 11, answered_in)
                .into_iter()
                .find(|observation| observation.metric_name == INTEGRATION_LATENCY_METRIC)
                .map(|observation| observation.value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the {INTEGRATION_LATENCY_METRIC} observation \
                 {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_routes_own_series_name_the_two_halves_of_its_route_key() {
        // Rows span the route-key forms an HTTP api admits: a verb and a path, the
        // verb-independent catch-all, a path carrying a variable and the greedy remainder, and
        // the default route, which names no halves at all.
        let cases: [(&str, &str, &str); 5] = [
            ("GET /orders", "GET", "/orders"),
            ("POST /orders/{id}", "POST", "/orders/{id}"),
            ("ANY /{proxy+}", "ANY", "/{proxy+}"),
            ("DELETE /", "DELETE", "/"),
            ("$default", super::super::ANY_METHOD, "$default"),
        ];
        for (route_key, method, resource) in cases {
            let actual = route_dimensions(route_key);
            assert_eq!(
                actual,
                (method.to_string(), resource.to_string()),
                "case {route_key:?}: expected the dimensions ({method:?}, {resource:?}), got \
                 {actual:?}"
            );
        }
    }
}
