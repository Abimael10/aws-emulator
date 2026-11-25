// SPDX-License-Identifier: CC0-1.0

//! Step 7 of an invoke: the route's INTEGRATION.
//!
//! `super::invoke` resolves the api, the stage, the deployment, the route and its
//! authorization, and then the route's integration decides the response. This module is
//! that last step.
//!
//! What each `IntegrationType` an HTTP api admits is:
//!
//! * `AWS_PROXY` naming no `IntegrationSubtype` is the LAMBDA PROXY integration — "for
//!   integrating the route or method request with the Lambda function-invoking action with
//!   the client request passed through as-is" (`Integration$IntegrationType`). The client
//!   request is passed through as the event the function's payload format defines
//!   ([`super::proxy_event`]), the function is RUN ([`super::lambda`]) through Lambda's own
//!   modelled `Invoke` over the in-process transport `OperationContext::callback` hands a
//!   handler, and its answer is read back as the response that format defines
//!   ([`super::proxy_response`]);
//! * `AWS_PROXY` naming an `IntegrationSubtype` is a first-class SERVICE ACTION ("Specifies
//!   the AWS service action to invoke"), which is not a function to run: the gateway itself
//!   calls that action, with the parameters the integration's `RequestParameters` map out of
//!   the request and under the role its `CredentialsArn` names
//!   ([`super::service_action`]);
//! * `HTTP_PROXY` is "for integrating the route or method request with an HTTP endpoint,
//!   with the client request passed through as-is", so it is a HOP ([`super::http_proxy`]):
//!   the gateway builds a request of its own at that endpoint, carrying the client's method,
//!   body, query string and headers, with the hop-identifying fields the gateway itself states
//!   — its own `Via`, and none of the client's `X-Forwarded-For` claim — and the endpoint's
//!   answer is what the client reads. The hop is performed where THIS plane serves that
//!   endpoint itself ([`super::serves_this_plane`]), by handing the request back to this
//!   plane's own serving, which opens no connection and dials no address a stored
//!   configuration named. Every other endpoint is a URI the CALLER can reach, and this
//!   emulator makes no request of its own to one — a forward target a `CreateIntegration`
//!   chooses is exactly what must not become a request this process makes, and the caller's
//!   own credential must not travel to one — so the caller is sent there instead:
//!   `307 Temporary Redirect` to the upstream, the redirect that preserves the request's
//!   method and body (RFC 7231 §6.4.7), so the client makes exactly the request the gateway
//!   would have forwarded and nothing about that endpoint's answer is invented;
//! * `AWS`, `HTTP` and `MOCK` are not an HTTP api's: `MOCK` "is not permitted for APIs with
//!   a protocol type of HTTP", and the two non-proxy types take request and response
//!   templates a WebSocket api's integration carries.
//!
//! # The statuses this step answers
//!
//! * the backend's own, for a backend that answered;
//! * `500` `{"message":"Internal Server Error"}` for the whole class "the integration
//!   produced no response" — a function that raised, a function the gateway could not
//!   invoke, a function whose own resource policy admits the gateway no invocation, an answer
//!   the payload format cannot be read as, a route with nothing behind it, or an integration
//!   this emulator cannot perform. The reason is logged, never told to the caller, which is
//!   what API Gateway does;
//! * `503` `{"message":"Service Unavailable"}` for an integration that RAN and had not
//!   answered within its own `TimeoutInMillis` — "custom timeout … between 50 and 30,000
//!   milliseconds for HTTP APIs" — which is a different class from one that answered badly
//!   and is why the deadline is applied to the run itself.
//!
//! * the ANSWER OF THE SERVICE for a first-class service action that service refused — its
//!   own status, the error shape it named and the text it stated, relayed: such an
//!   integration is the caller reaching that service through the api, so a refusal it earned
//!   is told to it as the service stated it ([`super::service_action`]);
//! * `429` `{"message":"Too Many Requests"}` for a request the serving stage's
//!   `RouteSettings` throttle ([`throttle`], [`crate::state::route_throttle`]);
//! * `400` `{"message":"Bad Request"}` for a request that does not carry what a first-class
//!   service-action integration's mapping selects for a parameter its action REQUIRES: the
//!   mapping is the api's own configuration and the request is what failed to satisfy it, so
//!   the refusal is the caller's rather than the "produced no response" class, and no action
//!   is performed for it;
//! * the `307` above for an `HTTP_PROXY` upstream this emulator serves nothing at, which is
//!   the caller's to reach.
//!
//! The stage's own variables are interpolated into the integration's `IntegrationUri`
//! before the backend is addressed, for EVERY type: "you can use API Gateway stage variables
//! … as part of an HTTP integration URL" and as part of the function a Lambda integration
//! names, which is what lets one api reach a different backend per stage. A reference the
//! stage carries no variable for names no backend at all, so it is the "produced no
//! response" class rather than a call to a URI carrying an unresolved reference.
//!
//! Beside the answer, this step reports how long the route's BACKEND took to answer the
//! invocation the gateway relayed to it ([`Performed`]), which is the one measurement only the
//! step that made the call can take — and the one the stage's backend-latency series is
//! published from ([`super::request_metrics`]).
//!
//! The answer a BACKEND gave is then transformed by the integration's own
//! `ResponseParameters` before the client reads it ([`response_parameters`]) — "you use
//! response parameters to transform the HTTP response from a backend integration before
//! returning the response to clients" — which is why that step is applied where a backend
//! answered and not to a refusal the gateway itself decided.
//!
//! The REQUEST a backend receives is transformed the same way, by the integration's own
//! `RequestParameters` ([`request_parameters`]) — "a key-value map specifying how to transform
//! HTTP requests before sending them to the backend" — applied once, here, so the event a
//! Lambda proxy integration is handed and the message an `HTTP_PROXY` integration forwards
//! both state the mapped request.

/// The serving stage's throttle, metered around the answer this step produces.
mod throttle;

use std::time::Duration;

use aws_smithy_runtime_api::http::Response as RtResponse;
use smithy_server_runtime::OperationContext;

use super::super::data_plane::{json_response, resolve_stage_variables, UNPLACEABLE_STATUS};
use super::lambda::{self, Unreached};
use super::proxy_event::{self, Invocation, ProxyRequest};
use super::proxy_response;
use super::request_authorizer::{self, Outcome};
use super::request_parameters;
use super::response_parameters;
use super::service_action::{self, Unperformed};
use super::{http_proxy, INTERNAL_MESSAGE};
use crate::state::route_throttle::Limits;
use crate::state::{Authorizer, Integration};

/// The status an integration that ran and did not answer in time is refused with, and the
/// sentence it carries — the status's own reason phrase (RFC 7231 §6.6.4), the convention
/// every sentence this endpoint answers follows.
const UNAVAILABLE_STATUS: u16 = 503;
const UNAVAILABLE_MESSAGE: &str = "Service Unavailable";

/// The status a request that cannot satisfy its route's own parameter mapping is refused
/// with, and the sentence it carries — the same reason-phrase convention (RFC 7231 §6.5.1).
const BAD_REQUEST_STATUS: u16 = 400;
const BAD_REQUEST_MESSAGE: &str = "Bad Request";

/// Everything step 7 needs, read out of the store while it was locked: the request as the
/// backend will see it, the invocation's own facts, the integration to perform, and the
/// authorizer whose decision is still owed.
/// It is OWNED rather than borrowed because the integration is performed with no store lock
/// held: one caller's backend must not stall another caller's request.
pub struct Dispatch {
    /// The request, as the event or the forwarded message states it.
    pub request: ProxyRequest,
    /// What the gateway knows about this invocation.
    pub invocation: Invocation,
    /// The integration the matched route targets, or `None` for a route that targets none —
    /// which has nothing to call at all.
    pub integration: Option<Integration>,
    /// The `REQUEST` authorizer in front of the route and the identity-source values its
    /// decision is about, for a route one gates.
    pub authorizer: Option<(Authorizer, Vec<String>)>,
    /// The throttle the serving stage's `RouteSettings` meter this route with, for a route
    /// the stage states a limit for.
    pub throttle: Option<Limits>,
    /// The route key, for the log lines below.
    pub route_key: String,
}

/// What step 7 produced: the answer the caller is told, and how long the route's BACKEND took
/// to answer the invocation the gateway relayed to it.
///
/// The latency is `None` for a request whose backend ANSWERED NOTHING — one refused before any
/// backend was reached (the authorizer's decision, the stage's throttle, a mapping the request
/// cannot satisfy), one whose route names an `HTTP_PROXY` upstream this emulator serves
/// nothing at, so the caller is sent there and no backend of this process was reached at all,
/// and one whose backend was invoked and never answered. That is the reading `IntegrationLatency` is published by
/// ([`crate::service::metrics::INTEGRATION_LATENCY_METRIC`]): it measures the time between the
/// gateway relaying the request and the backend's answer reaching it, so only a relayed
/// invocation the backend answered has one.
pub struct Performed {
    /// The answer, as the caller reads it.
    pub response: RtResponse,
    /// How long the backend took to answer, for a request one answered.
    pub integration_latency: Option<Duration>,
}

impl Performed {
    /// An answer this step decided WITHOUT a backend answering it, which has no integration
    /// latency to report.
    pub fn gateway(response: RtResponse) -> Self {
        Self {
            response,
            integration_latency: None,
        }
    }
}

/// Perform the authorizer's decision and then the route's integration.
///
/// `hop` is how many `HTTP_PROXY` integrations of this plane's OWN endpoints this request has
/// already been forwarded through ([`super::FIRST_HOP`] for a request a client made), which is
/// what bounds a configuration that forwards in a circle ([`MAX_FORWARDED_HOPS`]).
pub async fn perform<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    dispatch: Dispatch,
    hop: u8,
) -> Performed {
    // The backend's own latency is reported by the ONE step that relayed the request to it and
    // read its answer, which is why it is written through here rather than returned up each of
    // the refusals below: every one of those is an answer no backend gave.
    let mut integration_latency: Option<Duration> = None;
    let response = answered(engine, ctx, dispatch, hop, &mut integration_latency).await;
    Performed {
        response,
        integration_latency,
    }
}

/// The answer itself, recording into `integration_latency` how long a backend that ANSWERED
/// took.
async fn answered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    mut dispatch: Dispatch,
    hop: u8,
    integration_latency: &mut Option<Duration>,
) -> RtResponse {
    if let Some((authorizer, identity)) = dispatch.authorizer.clone() {
        let method = dispatch.request.method.clone();
        match request_authorizer::decide(
            engine,
            ctx,
            &authorizer,
            &dispatch.request,
            &dispatch.invocation,
            &identity,
            &method,
        )
        .await
        {
            Outcome::Refused(response) => return *response,
            Outcome::Allowed(decided) => {
                dispatch.invocation.authorizer_context = decided.context;
                dispatch.invocation.authorizer_principal = decided.principal_id;
            }
        }
    }
    let Some(integration) = dispatch.integration.clone() else {
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            "the matched route targets no integration of this api, so it has nothing to call"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    };
    // The throttle is metered once the route's authorization — including the authorizer's
    // own decision above — has admitted the request, and before anything the route fronts is
    // reached. The submission stays in flight for the rest of this function: the burst the
    // stage states is what the requests arriving TOGETHER at this route may draw ahead of its
    // rate, so what the throttle admits depends on which of them are still being answered.
    let submission = throttle::metered(engine, &dispatch);
    if let Some(refusal) = submission
        .as_ref()
        .and_then(throttle::Submission::refusal)
    {
        return refusal;
    }
    // A first-class service action names what it invokes ITSELF, so it is reached before a
    // backend URI is resolved: such an integration carries none at all.
    if let Some(subtype) = crate::state::service_action_subtype(
        &integration.integration_type,
        integration.integration_subtype.as_deref(),
    ) {
        return service_action(
            engine,
            ctx,
            &dispatch,
            &integration,
            subtype,
            integration_latency,
        )
        .await;
    }
    let Some(upstream) = resolved_uri(&dispatch, &integration) else {
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    };
    // The request the BACKEND receives is this integration's own `RequestParameters` applied
    // to the one that arrived — "a key-value map specifying how to transform HTTP requests
    // before sending them to the backend" ([`request_parameters`]) — so both legs below state
    // the mapped request: the event a Lambda proxy integration is handed, and the message an
    // `HTTP_PROXY` integration forwards. The route was matched above, on the path the CLIENT
    // sent, so no mapping of it can change which route serves the request; a subtype
    // integration's parameters are a different language and were reached above.
    let mapped_path = request_parameters::transformed(
        &mut dispatch.request,
        &integration.request_parameters,
        &dispatch.invocation,
    );
    if crate::state::is_lambda_proxy_integration(
        &integration.integration_type,
        integration.integration_subtype.as_deref(),
    ) {
        return lambda_proxy(ctx, &dispatch, &integration, &upstream, integration_latency).await;
    }
    if integration.integration_type == crate::state::HTTP_PROXY_INTEGRATION_TYPE {
        return http_proxy_hop(
            engine,
            ctx,
            &dispatch,
            &integration,
            &upstream,
            mapped_path.as_deref(),
            hop,
            integration_latency,
        )
        .await;
    }
    ::tracing::warn!(
        api_id = %dispatch.request.api_id,
        stage = %dispatch.request.stage_name,
        route_key = %dispatch.route_key,
        integration_type = %integration.integration_type,
        integration_subtype = ?integration.integration_subtype,
        "the route's integration names a backend this emulator cannot call, so the request \
         is answered as an integration that produced no response"
    );
    json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None)
}

/// The answer for an invoke served from a seam that carries no in-process transport: the
/// Lambda proxy integration cannot be performed from there, so the class it falls into is
/// reported.
///
/// Everything that does not need the transport is unchanged — the throttle, and the `307`
/// that sends the caller to an `HTTP_PROXY` upstream, which from here is the answer for EVERY
/// upstream: forwarding the request into this plane's own serving performs the route's
/// integration, which is what this seam has no transport for.
pub fn unperformable<Eng: Send + Sync + 'static>(engine: &Eng, dispatch: &Dispatch) -> Performed {
    let submission = throttle::metered(engine, dispatch);
    if let Some(refusal) = submission
        .as_ref()
        .and_then(throttle::Submission::refusal)
    {
        return Performed::gateway(refusal);
    }
    if let Some(integration) = dispatch.integration.as_ref() {
        if integration.integration_type == crate::state::HTTP_PROXY_INTEGRATION_TYPE {
            if let Some(upstream) = resolved_uri(dispatch, integration) {
                return Performed::gateway(http_proxy::sent_to_the_upstream(&upstream));
            }
        }
    }
    ::tracing::warn!(
        api_id = %dispatch.request.api_id,
        stage = %dispatch.request.stage_name,
        route_key = %dispatch.route_key,
        "this invoke is being served from a seam that carries no in-process transport, so \
         the route's integration was not performed"
    );
    Performed::gateway(json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None))
}

/// The integration's `IntegrationUri` with THIS stage's variables interpolated, or `None`
/// when it names a variable the stage does not carry (or resolves past the length the
/// member may hold), which names no backend at all. `None` also for an integration carrying
/// no URI, which likewise names none.
fn resolved_uri(dispatch: &Dispatch, integration: &Integration) -> Option<String> {
    let uri = integration
        .integration_uri
        .as_deref()
        .map(str::trim)
        .filter(|uri| !uri.is_empty())?;
    let resolved = resolve_stage_variables(uri, &dispatch.request.stage_variables);
    if resolved.is_none() {
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            "this route's integration URI names a stage variable this stage does not carry, \
             or resolves past the length its member may hold, so it resolves to no backend"
        );
    }
    resolved
}

/// The deadline the integration gives its backend: its own `TimeoutInMillis`, which the
/// create and update of an integration bound to the window the member declares, so the
/// stored value is always a duration.
fn deadline(integration: &Integration) -> Duration {
    Duration::from_millis(u64::try_from(integration.timeout_in_millis.max(0)).unwrap_or_default())
}

/// How many `HTTP_PROXY` integrations of THIS plane's own endpoints one invoke may be
/// forwarded through.
///
/// An integration whose upstream names the very stage it is reached at — or two that name
/// each other — would otherwise re-enter this plane's serving for ever, so the chain is cut
/// once it reaches this many hops, as the "produced no response" class every other
/// integration this gateway cannot perform falls into. API Gateway publishes no hop count, so
/// this is the emulator's own bound and is deliberately small: a front api in front of a
/// backend one is one hop. The bound is checked where the hop would be MADE, because that is
/// where the circle would be closed.
pub(super) const MAX_FORWARDED_HOPS: u8 = 4;

/// Serve the route with its `HTTP_PROXY` integration — "for integrating the route or method
/// request with an HTTP endpoint, with the client request passed through as-is".
///
/// Where this plane itself serves the upstream the integration resolved
/// ([`super::serves_this_plane`]), the hop is PERFORMED: the message the gateway forwards
/// carries the client's request with the gateway's own hop fields on it
/// ([`http_proxy::forwarded_request`]) and is handed back to this plane's own serving
/// ([`super::served_hop`]) — no connection is opened, no name is resolved and no address a
/// stored configuration named is dialled — so what the client reads is the answer that
/// upstream's own records produced, and the integration's `ResponseParameters` transform it
/// like any other backend's answer.
///
/// `mapped_path` is the path an `overwrite:path` entry of the integration's own
/// `RequestParameters` put on the request ([`request_parameters`]), which is the path the
/// forwarded message is addressed at in place of the one the upstream names.
///
/// Every OTHER upstream is a URI the CALLER can reach and this emulator makes no request of
/// its own to one, so the caller is sent there instead ([`http_proxy::sent_to_the_upstream`]).
/// The same answer covers a stored upstream that names no address a request can be made at,
/// where there is no hop to perform at all.
#[allow(clippy::too_many_arguments)]
async fn http_proxy_hop<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    dispatch: &Dispatch,
    integration: &Integration,
    upstream: &str,
    mapped_path: Option<&str>,
    hop: u8,
    integration_latency: &mut Option<Duration>,
) -> RtResponse {
    let Some(forwarded) =
        http_proxy::forwarded_request(&dispatch.request, integration, upstream, mapped_path)
            .filter(|forwarded| super::serves_this_plane(engine, forwarded))
    else {
        ::tracing::info!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            "sending the caller to this route's HTTP_PROXY upstream: this emulator serves \
             nothing at that address, so it makes no request of its own to it"
        );
        return http_proxy::sent_to_the_upstream(upstream);
    };
    if hop >= MAX_FORWARDED_HOPS {
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            hops = MAX_FORWARDED_HOPS,
            "this request has been forwarded through as many HTTP_PROXY integrations of this \
             emulator's own endpoints as one invoke may be, so it is not forwarded further"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    }
    // MEASURED from the hop being made: the upstream ANSWERED, whatever its answer was worth,
    // while an upstream that had not answered when the integration's own deadline elapsed has
    // no latency to report — the same reading every other backend of this plane is measured
    // by.
    let relayed = ::std::time::Instant::now();
    let answered = ::tokio::time::timeout(
        deadline(integration),
        super::served_hop(engine, &forwarded, ctx, hop.saturating_add(1)),
    )
    .await;
    let Ok(answered) = answered else {
        ::tracing::info!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            "refusing an invoke: this route's HTTP_PROXY upstream did not answer within the \
             integration's timeout"
        );
        return json_response(UNAVAILABLE_STATUS, UNAVAILABLE_MESSAGE, None);
    };
    *integration_latency = Some(relayed.elapsed());
    response_parameters::transformed(
        answered,
        &integration.response_parameters,
        &response_parameters::Call {
            request: &dispatch.request,
            invocation: &dispatch.invocation,
        },
    )
}

/// Serve the route with its Lambda proxy integration: the event in, the function run, its
/// answer out.
async fn lambda_proxy(
    ctx: &OperationContext<'_>,
    dispatch: &Dispatch,
    integration: &Integration,
    upstream: &str,
    integration_latency: &mut Option<Duration>,
) -> RtResponse {
    let payload_format = integration
        .payload_format_version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .unwrap_or(proxy_event::PAYLOAD_FORMAT_2_0)
        .to_string();
    if !proxy_response::is_known_format(&payload_format) {
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            route_key = %dispatch.route_key,
            payload_format = %payload_format,
            "this route's integration names a payload format version this gateway cannot \
             state an event in"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    }
    let Some(target) = lambda::function_target(upstream) else {
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            route_key = %dispatch.route_key,
            "this route's AWS_PROXY integration names no Lambda function to invoke"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    };
    let event = if payload_format == proxy_event::PAYLOAD_FORMAT_2_0 {
        proxy_event::event_2_0(&dispatch.request, &dispatch.invocation)
    } else {
        proxy_event::event_1_0(&dispatch.request, &dispatch.invocation)
    };
    // The gateway invokes the function as ITS OWN service principal, so the grant that
    // admits the invocation is the function's resource-based policy, narrowed to the route
    // this request was served by. An invocation it does not admit is never made, and the
    // route falls into the "produced no response" class — which is what makes a stripped
    // `lambda:InvokeFunction` permission fail the route closed instead of serving it.
    let source_arn = request_authorizer::route_arn(&dispatch.request, &dispatch.request.method);
    if !lambda::invocation_admitted(ctx.callback(), &target, &source_arn).await {
        ::tracing::info!(
            api_id = %dispatch.request.api_id,
            stage = %dispatch.request.stage_name,
            route_key = %dispatch.route_key,
            function = %target.arn,
            "refusing an invoke: this route's function allows the gateway no invocation for \
             it, so none was made"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    }
    let payload = match serde_json::to_vec(&event) {
        Ok(payload) => payload,
        Err(error) => {
            ::tracing::warn!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                %error,
                "the proxy event for this request could not be stated, so the function was \
                 not invoked"
            );
            return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
        }
    };
    // MEASURED from the invocation being relayed to the function: a run that ANSWERED has a
    // latency to report, whatever its answer was worth — a handler that raised answered with
    // Lambda's own error document, and a reply this payload format cannot be read as is still
    // a reply the backend gave — while a function the invocation never reached, and one that
    // had not answered when the integration's deadline elapsed, have none.
    let relayed = ::std::time::Instant::now();
    let ran = match lambda::invoke_function(ctx.callback(), &target, payload, deadline(integration))
        .await
    {
        Ok(ran) => {
            *integration_latency = Some(relayed.elapsed());
            ran
        }
        Err(unreached) => return unanswered(dispatch, unreached),
    };
    if ran.failed() {
        // "An unhandled error is still HTTP 200" to Lambda's own caller, so the signal is
        // what tells a run that ended in an error from one that answered: the payload is
        // Lambda's error document, not a response to map.
        ::tracing::warn!(
            api_id = %dispatch.request.api_id,
            route_key = %dispatch.route_key,
            function_error = ?ran.function_error,
            "this route's function ended in an unhandled error, so the integration produced \
             no response"
        );
        return json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None);
    }
    match proxy_response::map(&ran.payload, &payload_format) {
        // The backend ANSWERED, so the integration's own `ResponseParameters` transform that
        // answer before the client reads it ([`response_parameters`]).
        Ok(response) => response_parameters::transformed(
            response,
            &integration.response_parameters,
            &response_parameters::Call {
                request: &dispatch.request,
                invocation: &dispatch.invocation,
            },
        ),
        Err(malformed) => {
            ::tracing::warn!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                payload_format = %payload_format,
                reason = %malformed.reason,
                "this route's function answered something this payload format cannot be \
                 read as, so the integration produced no response"
            );
            json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None)
        }
    }
}

/// Serve the route with its first-class service-action integration: the mapped parameters
/// in, the action called under the integration's role, its own answer out
/// ([`super::service_action`]).
///
/// The classes an unperformed action falls into are the ones every other integration has,
/// plus the two only a CALLED service and a MAPPED request can report: an action that was
/// called and did not answer within the integration's own deadline is the timeout; one that
/// service ANSWERED a refusal for is that answer, relayed, because the call was served and
/// its answer is what the gateway has to report; one the REQUEST carried no required
/// parameter for is a bad request, because the mapping is the api's own and the request is
/// what failed to satisfy it; and everything else — a subtype this emulator cannot perform, a
/// mapping the action cannot be called with, a call that reached no service — is the
/// "produced no response" class. The reason is logged, never told to the caller.
async fn service_action<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    dispatch: &Dispatch,
    integration: &Integration,
    subtype: &str,
    integration_latency: &mut Option<Duration>,
) -> RtResponse {
    // MEASURED from the action being called, like every other backend this plane relays a
    // request to: the service ANSWERED both the call it performed and the call it refused, so
    // both have a latency to report, while a call that did not answer within the integration's
    // deadline, one the request carried no required parameter for (nothing was called at all)
    // and one that reached no service have none.
    let called = ::std::time::Instant::now();
    let performed = service_action::perform(
        engine,
        ctx,
        dispatch,
        integration,
        subtype,
        deadline(integration),
    )
    .await;
    if matches!(
        &performed,
        Ok(_) | Err(Unperformed::Answered(_))
    ) {
        *integration_latency = Some(called.elapsed());
    }
    let unperformed = match performed {
        Ok(answered) => return answered,
        Err(unperformed) => unperformed,
    };
    let reason = unperformed.reason();
    match &unperformed {
        Unperformed::TimedOut(_) => {
            ::tracing::info!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                integration_subtype = %subtype,
                reason = %reason,
                "refusing an invoke: this route's service-action integration did not answer \
                 within its timeout"
            );
            json_response(UNAVAILABLE_STATUS, UNAVAILABLE_MESSAGE, None)
        }
        Unperformed::Answered(answered) => {
            ::tracing::info!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                integration_subtype = %subtype,
                reason = %reason,
                "the service serving this route's integration refused the action, so its own \
                 answer is what the caller is told"
            );
            answered.relayed()
        }
        Unperformed::Unmappable(_) => {
            ::tracing::info!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                integration_subtype = %subtype,
                reason = %reason,
                "refusing an invoke: this request carries no value for a parameter this \
                 route's service-action integration maps onto the action it requires it for"
            );
            json_response(BAD_REQUEST_STATUS, BAD_REQUEST_MESSAGE, None)
        }
        Unperformed::NotPerformed(_) => {
            ::tracing::warn!(
                api_id = %dispatch.request.api_id,
                route_key = %dispatch.route_key,
                integration_subtype = %subtype,
                reason = %reason,
                "refusing an invoke: this route's service-action integration produced no \
                 response"
            );
            json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None)
        }
    }
}

/// The refusal for an invocation that produced no answer: the timeout class for a run that
/// overran the integration's own deadline, and the "produced no response" class for every
/// other way the function was not reached.
fn unanswered(dispatch: &Dispatch, unreached: Unreached) -> RtResponse {
    let timed_out = matches!(unreached, Unreached::TimedOut(_));
    let reason = unreached.reason();
    if timed_out {
        ::tracing::info!(
            api_id = %dispatch.request.api_id,
            route_key = %dispatch.route_key,
            reason = %reason,
            "refusing an invoke: this route's integration did not answer within its timeout"
        );
        return json_response(UNAVAILABLE_STATUS, UNAVAILABLE_MESSAGE, None);
    }
    ::tracing::warn!(
        api_id = %dispatch.request.api_id,
        route_key = %dispatch.route_key,
        reason = %reason,
        "refusing an invoke: this route's integration produced no response"
    );
    json_response(UNPLACEABLE_STATUS, INTERNAL_MESSAGE, None)
}
