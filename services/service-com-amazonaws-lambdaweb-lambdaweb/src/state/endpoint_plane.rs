// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a web function endpoint is published on, and what this emulator answers
//! there.
//!
//! `FunctionEndpointSummary$domainName` and `GetWebFunctionEndpointResponse$domainName` are
//! "The domain name assigned to the endpoint", and that domain is the whole DATA PLANE of a
//! web function endpoint: a client reads it off the endpoint and issues its HTTP request to
//! it. A published address is therefore an address A CLIENT USES, so this emulator
//! publishes one it answers on ITSELF — a listener the core owns
//! (`engine.listeners().http(..)`, which owns the socket, the bind interface and the TLS) —
//! rather than a name under a zone this emulator does not serve, which resolves to nothing
//! on the machine it runs on and would send a client off the emulator entirely.
//!
//! ONE listener per endpoint, so the ADDRESS identifies the endpoint: a client that was
//! handed `domainName` dials it with the endpoint's own path and nothing else, so the
//! endpoint cannot be carried in a path segment. The handle lives in
//! [`super::ComAmazonawsLambdawebLambdawebState::endpoint_planes`] under the endpoint's
//! ARN, beside the records that say which endpoints exist: dropping it closes the port, so
//! a deleted endpoint stops answering ([`super::ComAmazonawsLambdawebLambdawebState::
//! prune_endpoint_planes`]). How many addresses one store can hold open is bounded by the
//! resources a caller may create: a web function carries at most
//! [`super::MAX_ENDPOINTS_PER_FUNCTION`] endpoints, each with one address, and the store
//! holds at most [`super::MAX_WEB_FUNCTIONS`] web functions. An address the host will not
//! lend a socket for is not opened, and the create answers a modelled server error rather
//! than storing an endpoint that announces a domain nothing serves.
//!
//! The socket, the bind interface and the TLS are the CORE's: the listener is opened
//! through `engine.listeners()`, so this plane binds the interface the operator chose
//! (loopback unless they asked otherwise) and never names an address of its own.
//!
//! What the address answers, in this order:
//!
//! * a request for an endpoint this store no longer holds, or one that is not yet (or no
//!   longer) `Active` — nothing is published there, so `404`;
//! * on an `IamAuth` endpoint ("Lambda uses IAM to authorize requests, which must be signed
//!   with Signature Version 4 (SigV4)"), a request presenting no SigV4 credential at all —
//!   `403` with the service's own `Forbidden` PAGE ([`forbidden_document`]), which is what
//!   an unsigned request to an IAM-authorized endpoint earns. What is checked is that the
//!   request PRESENTS a SigV4 credential (in the `Authorization` header or as the query
//!   parameters of a presigned URL); verifying a signature is the emulator's own concern
//!   and no service does it, so nothing here claims to. An `ApplicationManaged` endpoint
//!   ("Your application handles authorization") refuses nothing here;
//! * an authorized request — the web function's OWN handler response: the revision the
//!   endpoint routes the request to is RUN ([`super::web_runtime`]), its deployment
//!   package's code is handed the request, and the status, headers and body that code
//!   answered are what the client is served. Nothing is synthesised in the handler's name —
//!   a revision whose code answered nothing, was stopped by one of the engine's bounds, or
//!   was never read at all is reported as that rather than as a response.
//!
//! Every answer the ENDPOINT's own decision serves ([`serve_request`], the three above)
//! NAMES THE RESPONSE IT IS, in the [`REQUEST_ID_HEADER`] response header: an id minted for
//! that one response, so a client that logs or reports what the endpoint answered has a
//! value to correlate the answer by. The `Forbidden` page states that same id in its
//! `Request ID:` line, so the document and the header of one refusal never name two
//! different responses. The transport refusals above it
//! ([`refused_before_the_endpoint`], and the shed at the in-flight cap) are the CORE's own
//! predicates, decided before an endpoint is resolved, and carry what a core HTTP listener
//! carries.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use ::emulator_server::{ListenerConn, ResourceListener, TlsUpgrader};
use ::sha2::{Digest as _, Sha256};
use ::tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::code_package::DeploymentPackage;
use super::web_runtime::{self, HandlerOutcome, HandlerRequest};
use super::{ComAmazonawsLambdawebLambdawebState as ServiceState, EndpointState, RevisionState};

/// How many endpoint request BODIES this service reads at once, across every published
/// address: the core's own HTTP in-flight cap, one permit per request, taken before a byte
/// of a body is read and released when the answer is written.
///
/// What one in-flight request may buffer is the core's per-connection ceiling, so what the
/// bodies in flight hold at once is this count times that ceiling — the product the core's
/// own HTTP listeners bound — rather than the number of endpoints times it. ONE semaphore
/// for the whole service, because a caller chooses how many endpoints exist and each one
/// would otherwise raise the peak again. A request that finds no permit is shed, which is
/// what the core's own HTTP listener answers such a request.
fn in_flight() -> Arc<Semaphore> {
    static IN_FLIGHT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(IN_FLIGHT.get_or_init(|| {
        Arc::new(Semaphore::new(
            ::emulator_server::listener_caps::MAX_HTTP_INFLIGHT,
        ))
    }))
}

/// What the address answers when the endpoint it was opened for is gone, or is not serving
/// traffic yet. One sentence carrying nothing from the request, so a caller cannot probe
/// with it.
const NO_ENDPOINT_AT_THIS_ADDRESS: &str = "No web function endpoint is published at this \
                                           address.\n";

/// The response header every answer a published endpoint address serves names itself by:
/// the id of that one response, which the `Forbidden` page below also states. A client
/// correlates a refusal it was served by this value, so a refusal that carried none would
/// be unattributable.
const REQUEST_ID_HEADER: &str = "lambda-web-request-id";

/// What an `IamAuth` endpoint answers a request that presents no SigV4 credential: the
/// `403 Forbidden` page an unauthorized request to an IAM-authorized endpoint earns, whose
/// only variable part is `request_id` — the id of THIS response, reported in
/// [`REQUEST_ID_HEADER`] as well, so the page and the header name one response.
fn forbidden_document(request_id: &str) -> String {
    format!(
        "<!DOCTYPE html>\n\
         <html>\n\
         <head>\n    \
         <title>HTTP Error 403</title>\n\
         </head>\n\
         <body>\n    \
         <h1>403 Forbidden</h1>\n    \
         <p>You are not authorized to access this endpoint.</p>\n    \
         <p>Request ID: {request_id}</p>\n\
         </body>\n\
         </html>"
    )
}

/// What the address answers an AUTHORIZED request that reached a revision whose code this
/// emulator holds nothing to run: the endpoint serves no revision, the revision it routed
/// to is not ready to serve traffic, or the build never established the revision's package.
/// The sentence carries nothing from the request or from the revision's own code.
const NO_CODE_TO_SERVE: &str = "No web function code is serving this endpoint.\n";

/// What it answers a request whose run produced no response of its own: the code raised, it
/// registered no server, or it ended without answering. What the run came to is LOGGED; the
/// client is served this one sentence, so nothing the guest's own code worded reaches it.
const NO_HANDLER_RESPONSE: &str = "The web function did not produce a response.\n";

/// What it answers a request whose run was stopped by one of the engine's bounds.
const HANDLER_TIMED_OUT: &str = "The web function did not answer within its timeout.\n";

/// The status a request whose run produced no response of its own earns: the endpoint
/// reached the web function and the function answered nothing.
const BAD_GATEWAY: u16 = 502;

/// The status a request whose run was stopped by a bound earns.
const GATEWAY_TIMEOUT: u16 = 504;

/// What it answers a request that arrived while this service was already reading as many
/// endpoint bodies as it reads at once.
const SERVICE_UNAVAILABLE: u16 = 503;

/// The body of an answer for a request shed at the in-flight cap.
const SERVER_BUSY: &str = "This address is serving as many requests as it can. Try again.\n";

/// The query parameter a presigned SigV4 request carries its credential scope in, which is
/// the other place a request may present one.
const PRESIGNED_CREDENTIAL: &str = "X-Amz-Credential";

/// One endpoint's data-plane listener: the address it answers on, and the handle that keeps
/// it open.
pub struct EndpointPlane {
    /// The `host:port` a client dials, which is what the endpoint reports as its
    /// `domainName`.
    authority: String,
    /// The listener itself. Never read: it is held because dropping it closes the port.
    _listener: ResourceListener,
}

impl EndpointPlane {
    /// The `host:port` a client reaches this endpoint at — its `domainName`.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// Written by hand rather than derived: an [`EndpointPlane`] holds a core
/// `ResourceListener`, which is a live socket and carries no `Debug`. What is diagnosable
/// is the address it announced.
impl std::fmt::Debug for EndpointPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EndpointPlane")
            .field("authority", &self.authority)
            .finish()
    }
}

/// Open the listener the endpoint `endpoint_name` of `function_name` is served on.
///
/// `None` when this engine vends no listener, which leaves the caller to answer with a
/// modelled server error rather than publish an address nothing serves.
pub async fn open_endpoint_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ServiceState>,
    function_name: &str,
    endpoint_name: &str,
) -> Option<EndpointPlane> {
    let listeners = super::shared_engine(engine)?.listeners();
    let handler = EndpointFrontDoor {
        state: state.clone(),
        function_name: function_name.to_string(),
        endpoint_name: endpoint_name.to_string(),
        // The core's own TLS server, lent to this plaintext listener so the `https://`
        // half of the address is terminated by the core rather than here. A listener this
        // emulator could prepare none for serves the `http://` half alone.
        upgrader: listeners.tls_upgrader().ok(),
    };
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one
    // built with no IO driver, which is what a host driving this emulator in process may
    // hand a request — leaves the create without an address instead of taking the request
    // down with it.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(handler).await.ok() }).await;
    let listener = opened.ok().flatten()?;
    Some(EndpointPlane {
        // The core's OWN reading of the address a client dials, which names the loopback
        // interface for a wildcard bind — where `host()` would report `0.0.0.0`, an
        // address no client dials.
        authority: listener.authority(),
        _listener: listener,
    })
}

/// Serves ONE endpoint's published address: it answers every request it accepts from this
/// store, over the scheme the client opened with.
struct EndpointFrontDoor {
    state: ::engine::ServiceState<ServiceState>,
    function_name: String,
    endpoint_name: String,
    upgrader: Option<TlsUpgrader>,
}

impl ::emulator_server::NativeHandler for EndpointFrontDoor {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(serve_connection(
            self.state.clone(),
            self.function_name.clone(),
            self.endpoint_name.clone(),
            self.upgrader.clone(),
            conn,
        ))
    }
}

/// Speak HTTP/1.1 on one accepted connection until the client closes it, answering each
/// request from this store.
///
/// The encryption is negotiated in band first ([`::service_common::in_band_tls`]), so the
/// ONE address an endpoint publishes serves both the `https://` a client of AWS dials and
/// the `http://` a client that does not verify a self-signed certificate dials. HTTP/1.1
/// only, as the emulator's own front door offers: every request is fully buffered, so
/// nothing relies on HTTP/2 streaming. The header-read deadline and the body ceiling are
/// the core's, so a client that dribbles a request line or sends more than one connection
/// may carry cannot hold the connection permit the core issued for it.
async fn serve_connection(
    state: ::engine::ServiceState<ServiceState>,
    function_name: String,
    endpoint_name: String,
    upgrader: Option<TlsUpgrader>,
    conn: ListenerConn,
) {
    let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
        return;
    };
    let io = ::hyper_util::rt::TokioIo::new(conn);
    let service = ::hyper::service::service_fn(move |request: ::http::Request<_>| {
        let state = state.clone();
        let function_name = function_name.clone();
        let endpoint_name = endpoint_name.clone();
        async move {
            // A panic anywhere behind this address is CONTAINED here and answered as the
            // protocol-shaped internal failure, exactly as the endpoint a control call
            // arrives on answers the same request: one client cannot take the connection,
            // the listener or the process down with it.
            let facts = ::emulator_server::RequestFacts::from_request(&request);
            Ok::<_, ::std::convert::Infallible>(
                ::emulator_server::contain_panics(
                    facts,
                    answer(state, function_name, endpoint_name, request),
                )
                .await,
            )
        }
    });
    if let Err(error) = ::hyper::server::conn::http1::Builder::new()
        .timer(::hyper_util::rt::TokioTimer::new())
        .header_read_timeout(::std::time::Duration::from_secs(
            ::emulator_server::listener_caps::HTTP_REQUEST_DEADLINE_SECS,
        ))
        .serve_connection(io, service)
        .await
    {
        // A connection the client dropped, or a request this emulator could not frame,
        // ends here and takes nothing else with it: the next connection is served
        // normally.
        ::tracing::debug!(%error, "a connection to a web function endpoint ended before its response");
    }
}

/// Answer one request to the published address of the endpoint this listener was opened
/// for.
///
/// The body is read up to the core's per-connection ceiling before the decision is taken,
/// so a request larger than this address accepts is refused rather than buffered.
async fn answer(
    state: ::engine::ServiceState<ServiceState>,
    function_name: String,
    endpoint_name: String,
    request: ::http::Request<::hyper::body::Incoming>,
) -> ::http::Response<::http_body_util::Full<::bytes::Bytes>> {
    let (parts, body) = request.into_parts();
    if let Some(refused) = refused_before_the_endpoint(&parts) {
        return refused;
    }
    let ceiling =
        usize::try_from(::emulator_server::listener_caps::MAX_BYTES_PER_CONN).unwrap_or(usize::MAX);
    // One permit per request IN FLIGHT, taken BEFORE a byte of the body is read: what the
    // bodies being read hold at once is then the in-flight cap times the ceiling below,
    // rather than the number of published endpoints times it.
    let Ok(permit) = in_flight().try_acquire_owned() else {
        return served(plain(false, SERVICE_UNAVAILABLE, SERVER_BUSY));
    };
    // Held for the whole read and the decision, and released with them.
    let _permit: OwnedSemaphorePermit = permit;
    let Ok(collected) = ::http_body_util::BodyExt::collect(::http_body_util::Limited::new(
        body, ceiling,
    ))
    .await
    else {
        return served(plain(false, PAYLOAD_TOO_LARGE, BODY_TOO_LARGE));
    };
    let buffered = ::http::Request::from_parts(parts, collected.to_bytes().to_vec());
    served(serve_request(&state, &function_name, &endpoint_name, &buffered).await)
}

/// One answer of [`serve_request`] as the connection writes it.
fn served(
    response: ::http::Response<Vec<u8>>,
) -> ::http::Response<::http_body_util::Full<::bytes::Bytes>> {
    let (parts, body) = response.into_parts();
    ::http::Response::from_parts(parts, ::http_body_util::Full::new(::bytes::Bytes::from(body)))
}

/// The transport-safety refusals the core applies to its OWN HTTP listeners, applied here
/// from the core's own predicates so this address and a core listener decide them alike:
/// the `Host` allowlist (the DNS-rebinding guard a browser needs), the WebSocket refusal
/// and the browser check. `None` lets the request through to the endpoint's own decision.
fn refused_before_the_endpoint(
    parts: &::http::request::Parts,
) -> Option<::http::Response<::http_body_util::Full<::bytes::Bytes>>> {
    let header = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());

    // HTTP/1.1 requires a `Host`, and without an authority the rebinding guard cannot be
    // run at all, so a request carrying none is refused rather than served: this fails
    // CLOSED, exactly as the core's own HTTP listener does.
    let Some(host) = header("host").or_else(|| parts.uri.host()) else {
        return Some(served(plain(false, MISDIRECTED_REQUEST, HOST_REQUIRED)));
    };
    if !host_allowed(::service_common::data_plane_addressing::host_only(host)) {
        return Some(served(plain(false, MISDIRECTED_REQUEST, HOST_NOT_ALLOWED)));
    }
    if ::emulator_server::is_websocket_upgrade(
        header("connection"),
        header("upgrade"),
        header(":protocol"),
    ) {
        return Some(served(plain(false, 501, NO_WEBSOCKETS)));
    }
    if let ::emulator_server::BrowserVerdict::Forbid(reason) =
        ::emulator_server::classify_browser_request(
            header("origin"),
            header("sec-fetch-site"),
            // This address is the emulator's own, reached from the machine it runs on: the
            // same default a core HTTP listener carries, which admits no remote origin.
            &::emulator_server::RemoteOrigins::SameOriginOnly,
            false,
        )
    {
        let body = ::bytes::Bytes::from(reason.to_string());
        return Some(
            ::http::Response::builder()
                .status(FORBIDDEN)
                .header("content-type", "text/plain; charset=utf-8")
                .body(::http_body_util::Full::new(body.clone()))
                .unwrap_or_else(|_| {
                    let mut response = ::http::Response::new(::http_body_util::Full::new(body));
                    *response.status_mut() = ::http::StatusCode::FORBIDDEN;
                    response
                }),
        );
    }
    None
}

/// Whether a request's `Host` names the interface this address was bound on, by the rule a
/// core HTTP listener applies: a loopback `Host` on the default loopback bind (which is
/// what guards a browser against DNS rebinding), the operator's own address when they
/// bound a routable one, and any `Host` on a wildcard bind — there the operator exposed the
/// port deliberately and no single address names it.
fn host_allowed(host: &str) -> bool {
    if ::emulator_server::is_loopback_host(host) {
        return true;
    }
    let Ok(bound) = ::emulator_server::bind_address_from_env() else {
        return false;
    };
    bound.is_unspecified()
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<::std::net::IpAddr>()
            .is_ok_and(|named| named == bound)
}

/// What this address answers a request whose `Host` is not the address it serves, or that
/// names none.
const MISDIRECTED_REQUEST: u16 = 421;

/// What it answers a request a browser made cross-site, and the status an `IamAuth`
/// endpoint refuses a request presenting no SigV4 credential with.
const FORBIDDEN: u16 = 403;

/// What it answers a request carrying more than one connection may.
const PAYLOAD_TOO_LARGE: u16 = 413;

/// The body of an answer for a request whose `Host` is not the address it serves.
const HOST_NOT_ALLOWED: &str = "This address does not serve that host.\n";

/// The body of an answer for a request that names no host at all, which no HTTP/1.1
/// request may.
const HOST_REQUIRED: &str = "This address requires the host it is addressed by.\n";

/// The body of an answer for a WebSocket upgrade, which no web function endpoint serves.
const NO_WEBSOCKETS: &str = "This address does not serve WebSocket connections.\n";

/// The body of an answer for a request larger than this address accepts.
const BODY_TOO_LARGE: &str = "This request is larger than this address accepts.\n";

/// One request to a published endpoint, answered from this store — the decision the
/// listener's handler serves, and the one this service's tests drive.
///
/// The endpoint is resolved FIRST and the authorization second, which is the order the real
/// address works in: only an address that publishes an endpoint answers at all, and then
/// the endpoint's `authType` decides. An authorized request is answered by the web
/// function's own code: the revision the endpoint routes it to is run
/// ([`super::web_runtime`]) and what that code answered is served. Every answer names the
/// response it is ([`REQUEST_ID_HEADER`]), minted once here and used in both the header and
/// the `Forbidden` page, so the two cannot disagree. The one write is the clock's own
/// ([`ServiceState::materialise_due_transitions`]), owed at this instant whether or not
/// anybody connected, so reaching an endpoint changes nothing a later caller sees.
pub async fn serve_request(
    state: &::engine::ServiceState<ServiceState>,
    function_name: &str,
    endpoint_name: &str,
    req: &::http::Request<Vec<u8>>,
) -> ::http::Response<Vec<u8>> {
    let head = req.method() == ::http::Method::HEAD;
    let presented = presents_sigv4_credential(req);
    // One id per RESPONSE, minted before the decision and reported by whichever answer this
    // request earns: two answers are then distinguishable, which is what a client logging
    // or reporting a refusal correlates on.
    let request_id = ::service_common::query_error_envelope::new_request_id();

    // What the STORE says about this request, taken under the guard and released before any
    // of the revision's code runs: the endpoint's own decision, and the revision one
    // request routes to.
    let addressed = addressed(state, function_name, endpoint_name, &request_id);
    let Some(addressed) = addressed else {
        return attributed(plain(head, 404, NO_ENDPOINT_AT_THIS_ADDRESS), &request_id);
    };

    if addressed.auth_type == super::AUTH_TYPE_IAM && !presented {
        // Every attempt on a published endpoint is attributable: the endpoint addressed, the
        // outcome, and the id the client was answered under, so a refusal a client reports
        // is the refusal this log line records. Nothing the client sent is logged — not the
        // path, not a header.
        ::tracing::info!(
            audit = true,
            operation = "WebFunctionEndpointRequest",
            outcome = "refused",
            endpoint_arn = %addressed.endpoint_arn,
            request_id = %request_id,
            "web function endpoint request presenting no SigV4 credential"
        );
        return attributed(
            html(head, FORBIDDEN, &forbidden_document(&request_id)),
            &request_id,
        );
    }

    let Some(serving) = addressed.serving else {
        ::tracing::info!(
            audit = true,
            operation = "WebFunctionEndpointRequest",
            outcome = "no-code",
            endpoint_arn = %addressed.endpoint_arn,
            request_id = %request_id,
            "web function endpoint request reached no revision whose code this emulator holds"
        );
        return attributed(
            plain(head, SERVICE_UNAVAILABLE, NO_CODE_TO_SERVE),
            &request_id,
        );
    };

    // The revision's OWN code answers from here on: it is handed the request the client
    // sent and run under the engine's own bounds, with no store guard held.
    let outcome = web_runtime::serve_handler(
        serving.package,
        serving.environment,
        serving.timeout_seconds,
        serving.runtime,
        handler_request(req),
    )
    .await;
    let answer = match outcome {
        HandlerOutcome::Answered(answer) => {
            ::tracing::info!(
                audit = true,
                operation = "WebFunctionEndpointRequest",
                outcome = "served",
                endpoint_arn = %addressed.endpoint_arn,
                revision_id = %serving.revision_id,
                status = answer.status,
                request_id = %request_id,
                "web function endpoint request answered by the revision's code"
            );
            handler_answer(head, &answer)
        }
        HandlerOutcome::NoResponse(reason) => {
            // What the RUN came to, in its escaped form so a message the guest's code
            // worded cannot forge a log line of this emulator's. The client is served this
            // module's own sentence instead, so nothing of the code's own wording reaches
            // it.
            ::tracing::info!(
                audit = true,
                operation = "WebFunctionEndpointRequest",
                outcome = "no-response",
                endpoint_arn = %addressed.endpoint_arn,
                revision_id = %serving.revision_id,
                reason = ?reason,
                request_id = %request_id,
                "web function endpoint request produced no response"
            );
            plain(head, BAD_GATEWAY, NO_HANDLER_RESPONSE)
        }
        HandlerOutcome::Stopped(reason) => {
            ::tracing::info!(
                audit = true,
                operation = "WebFunctionEndpointRequest",
                outcome = "stopped",
                endpoint_arn = %addressed.endpoint_arn,
                revision_id = %serving.revision_id,
                reason = ?reason,
                request_id = %request_id,
                "web function endpoint request was stopped by one of the run's bounds"
            );
            plain(head, GATEWAY_TIMEOUT, HANDLER_TIMED_OUT)
        }
        HandlerOutcome::Unavailable(reason) => {
            ::tracing::warn!(
                audit = true,
                operation = "WebFunctionEndpointRequest",
                outcome = "unavailable",
                endpoint_arn = %addressed.endpoint_arn,
                revision_id = %serving.revision_id,
                reason = ?reason,
                request_id = %request_id,
                "web function endpoint request reached no run at all"
            );
            plain(head, SERVICE_UNAVAILABLE, NO_CODE_TO_SERVE)
        }
    };
    attributed(answer, &request_id)
}

/// One request as the revision's code is handed it: the method, the path and query, the
/// headers lowercased as a Node server reports them, and the body as text.
///
/// The body reaches the code as TEXT, which is what this layer carries (see
/// [`super::web_runtime`]): the bytes of a request body that is not UTF-8 are not carried
/// through to the code.
fn handler_request(req: &::http::Request<Vec<u8>>) -> HandlerRequest {
    let url = req
        .uri()
        .path_and_query()
        .map_or_else(|| req.uri().path().to_string(), ToString::to_string);
    let headers = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_lowercase(), value.to_string()))
        })
        .collect();
    HandlerRequest {
        method: req.method().as_str().to_string(),
        url,
        headers,
        // The body as the TEXT the code reads. A request this address accepts is already
        // bounded by the core's per-connection ceiling before it reaches here.
        body: String::from_utf8_lossy(req.body()).to_string(),
    }
}

/// The response a run answered with, as the connection writes it: the status, the headers
/// and the body the code produced, with the length of what a `GET` would carry reported on
/// a `HEAD` as well.
fn handler_answer(head: bool, answer: &web_runtime::HandlerResponse) -> ::http::Response<Vec<u8>> {
    let mut builder = ::http::Response::builder().status(answer.status);
    let mut stated_length = false;
    for (name, value) in &answer.headers {
        if name.eq_ignore_ascii_case("content-length") {
            stated_length = true;
        }
        builder = builder.header(name, value);
    }
    if !stated_length {
        builder = builder.header("content-length", answer.body.len().to_string());
    }
    let body = if head { Vec::new() } else { answer.body.clone() };
    builder.body(body).unwrap_or_else(|_| {
        // A status or a header the code produced that cannot be written is reported as the
        // run producing no response the connection can serve, rather than by failing the
        // answer the client is owed.
        plain(head, BAD_GATEWAY, NO_HANDLER_RESPONSE)
    })
}

/// What one request's own endpoint says about it, read under the store guard.
struct Addressed {
    /// The endpoint's `authType`, which decides what an unsigned request earns.
    auth_type: String,
    /// The endpoint's ARN, which every log line of this request names it by.
    endpoint_arn: String,
    /// The revision this request routes to, and everything running it needs — `None` when
    /// the endpoint serves no revision whose code this emulator holds.
    serving: Option<Serving>,
}

/// The revision one request routes to, copied out of the store so the run holds no guard.
struct Serving {
    /// The revision's id, which the log line of this request names.
    revision_id: String,
    /// The modules its build read out of its deployment package.
    package: Arc<DeploymentPackage>,
    /// The environment variables its service configuration published to the code: "a map of
    /// environment variable key-value pairs available to the web function at runtime"
    /// (`ServiceConfig$environmentVariables`).
    environment: BTreeMap<String, String>,
    /// Its configured `timeoutSeconds`.
    timeout_seconds: i32,
    /// The runtime identifier its build configuration names.
    runtime: String,
}

/// Resolve the endpoint this address publishes and the revision this request routes to, and
/// `None` when nothing is published here.
///
/// The one write is the clock's own, owed at this instant whether or not anybody connected.
fn addressed(
    state: &::engine::ServiceState<ServiceState>,
    function_name: &str,
    endpoint_name: &str,
    request_id: &str,
) -> Option<Addressed> {
    let mut store = state.lock();
    store.materialise_due_transitions(SystemTime::now());
    let function = store.functions.get(function_name)?;
    let endpoint = function
        .endpoints
        .get(endpoint_name)
        .filter(|endpoint| endpoint.state == EndpointState::Active)?;
    // Which revision's share of the traffic this request falls in, drawn from the id of the
    // response it will be answered under: a value of THIS request's own, so no counter is
    // bumped by serving it.
    let serving = endpoint
        .routed_revision(draw(request_id))
        .and_then(|revision_id| function.revisions.get(revision_id))
        // "Active: the revision is ready to serve traffic" (`RevisionState`), so a revision
        // that is still building or whose build failed takes none.
        .filter(|revision| revision.state == RevisionState::Active)
        .and_then(|revision| {
            Some(Serving {
                revision_id: revision.revision_id.clone(),
                package: revision.package.clone()?,
                environment: revision
                    .service_config
                    .environment_variables
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                timeout_seconds: revision.service_config.timeout_seconds,
                runtime: revision.build_config.runtime_config.runtime.clone(),
            })
        });
    Some(Addressed {
        auth_type: endpoint.auth_type.clone(),
        endpoint_arn: endpoint.endpoint_arn.clone(),
        serving,
    })
}

/// One request's draw on the endpoint's revision weights: the first 8 bytes of SHA-256 over
/// the id of the response it is answered under.
///
/// A value of the REQUEST's own, so two requests to one endpoint can fall in two different
/// shares and serving one changes nothing a later caller sees.
fn draw(request_id: &str) -> u64 {
    let digest = Sha256::digest(request_id.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head)
}

/// `response` with the id of the response it is named on it ([`REQUEST_ID_HEADER`]).
///
/// The id is this module's own minting ([`::service_common::query_error_envelope::
/// new_request_id`]), so it is always a legal header value; a value that somehow is not is
/// dropped rather than failing the answer the client is owed.
fn attributed(
    mut response: ::http::Response<Vec<u8>>,
    request_id: &str,
) -> ::http::Response<Vec<u8>> {
    if let Ok(value) = ::http::HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Whether `req` PRESENTS a SigV4 credential: the `Authorization` header a signed request
/// carries its credential scope in ([`::emulator_server::sigv4_credential_scope`]), or the
/// `X-Amz-Credential` query parameter a presigned URL carries it in.
///
/// Presenting a credential is not the same as a verified signature, and nothing here claims
/// otherwise: verifying a request's signature is the emulator's own concern and no service
/// does it.
fn presents_sigv4_credential(req: &::http::Request<Vec<u8>>) -> bool {
    if ::emulator_server::sigv4_credential_scope(req.headers()).is_some() {
        return true;
    }
    req.uri().query().is_some_and(|query| {
        query
            .split('&')
            .filter_map(|pair| pair.split('=').next())
            .any(|name| name.eq_ignore_ascii_case(PRESIGNED_CREDENTIAL))
    })
}

/// One plain-text answer of this address's own.
///
/// A published endpoint is plain HTTP rather than a modelled operation, so it answers in
/// its own form instead of this service's restJson1 envelope. Every message is a constant
/// of this module, never built from the request. A `HEAD` carries the metadata of what a
/// `GET` of the same address would answer — its status and its length — without the body,
/// which is what the method means.
fn plain(head: bool, status: u16, message: &str) -> ::http::Response<Vec<u8>> {
    text_answer(head, status, "text/plain; charset=utf-8", message)
}

/// One HTML answer of this address's own, for the refusal this service words as a PAGE
/// rather than a sentence ([`forbidden_document`]).
///
/// The media type carries no `charset` parameter, unlike the plain-text answers above: that
/// is the value a published endpoint reports for this page, so it is left as it is served.
fn html(head: bool, status: u16, document: &str) -> ::http::Response<Vec<u8>> {
    text_answer(head, status, "text/html", document)
}

/// The response both forms above are built from.
fn text_answer(
    head: bool,
    status: u16,
    content_type: &'static str,
    message: &str,
) -> ::http::Response<Vec<u8>> {
    let body = if head {
        Vec::new()
    } else {
        message.as_bytes().to_vec()
    };
    ::http::Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("content-length", message.len().to_string())
        .body(body)
        // Every argument is this module's own, none of it built from the request, so the
        // builder cannot fail; answer a bare 500 rather than panicking if it ever does.
        .unwrap_or_else(|_| {
            let mut response = ::http::Response::new(Vec::new());
            *response.status_mut() = ::http::StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use ::service_common::query_error_envelope::new_request_id;

    /// A request of `method` to `uri` carrying the headers `headers`.
    fn request(method: &str, uri: &str, headers: &[(&str, &str)]) -> ::http::Request<Vec<u8>> {
        let mut builder = ::http::Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Vec::new()).expect("a well-formed request")
    }

    /// One row: the case, the request URI, the headers it carries, and whether it presents
    /// a credential.
    type CredentialRow<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], bool);

    /// A SigV4 credential travels in the `Authorization` header or in a presigned URL's
    /// query, and nothing else presents one.
    #[test]
    fn a_sigv4_credential_is_recognised_wherever_a_client_presents_it() {
        let signed = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/lambda/\
                      aws4_request, SignedHeaders=host, Signature=deadbeef";
        let rows: &[CredentialRow<'_>] = &[
            ("a signed GET", "/", &[("authorization", signed)], true),
            (
                "a presigned GET",
                "/?X-Amz-Credential=AKIDEXAMPLE%2F20260101%2Fus-east-1%2Flambda%2Faws4_request\
                 &X-Amz-Signature=deadbeef",
                &[],
                true,
            ),
            (
                "a presigned GET whose parameter is lower-cased",
                "/?x-amz-credential=AKIDEXAMPLE",
                &[],
                true,
            ),
            ("an unsigned GET", "/", &[], false),
            ("an unsigned GET with a query", "/items?page=2", &[], false),
            (
                "another authorization scheme",
                "/",
                &[("authorization", "Basic dXNlcjpwYXNz")],
                false,
            ),
        ];
        for (case, uri, headers, expected) in rows {
            let presented = presents_sigv4_credential(&request("GET", uri, headers));
            assert_eq!(
                presented, *expected,
                "case {case:?}: expected presents-a-credential {expected}, got {presented}"
            );
        }
    }

    /// The `Forbidden` page's text is fixed but for the id of the response serving it: the
    /// whole document is written out here, independently of the one the code builds, so a
    /// drift in a tag, a sentence or a line break is caught.
    ///
    /// The rows differ only in the one variable part — the id — and they span what an id
    /// may look like, the minted form included.
    #[test]
    fn the_forbidden_page_states_only_the_responses_own_id() {
        for (case, request_id) in [
            ("a minted id", new_request_id()),
            ("another minted id", new_request_id()),
            ("a short id", "r-1".to_string()),
            ("an upper-cased id", "A1B2C3D4".to_string()),
        ] {
            let expected = format!(
                "<!DOCTYPE html>\n<html>\n<head>\n    <title>HTTP Error 403</title>\n</head>\n\
                 <body>\n    <h1>403 Forbidden</h1>\n    <p>You are not authorized to access \
                 this endpoint.</p>\n    <p>Request ID: {request_id}</p>\n</body>\n</html>"
            );
            let built = forbidden_document(&request_id);
            assert_eq!(
                built, expected,
                "case {case:?}: expected the Forbidden page, got {built:?}"
            );
        }
    }

    /// The page is served as the HTML document it is, with the answering response's id on
    /// the answer as well as in the page.
    #[test]
    fn the_forbidden_answer_carries_the_page_and_its_id() {
        let request_id = new_request_id();
        let answered = attributed(
            html(false, FORBIDDEN, &forbidden_document(&request_id)),
            &request_id,
        );
        assert_eq!(
            answered.status().as_u16(),
            403,
            "expected 403, got {}",
            answered.status()
        );
        let header = |name: &str| {
            answered
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            header("content-type"),
            "text/html",
            "expected the page to be served as HTML, got {:?}",
            header("content-type")
        );
        assert_eq!(
            header(REQUEST_ID_HEADER),
            request_id,
            "expected the answering response's id on the answer, got {:?}",
            header(REQUEST_ID_HEADER)
        );
        let body = String::from_utf8_lossy(answered.body()).to_string();
        assert!(
            body.contains(&format!("<p>Request ID: {request_id}</p>")),
            "expected the page to state the id of the response carrying it, got {body:?}"
        );
    }

    /// A `HEAD` carries the status and the length a `GET` would answer, and no body.
    #[test]
    fn a_head_answers_the_metadata_of_the_get() {
        for (case, head, expected_len) in [
            ("a GET", false, NO_HANDLER_RESPONSE.len()),
            ("a HEAD", true, 0),
        ] {
            let answered = plain(head, BAD_GATEWAY, NO_HANDLER_RESPONSE);
            assert_eq!(
                answered.status().as_u16(),
                BAD_GATEWAY,
                "case {case:?}: expected {BAD_GATEWAY}, got {}",
                answered.status()
            );
            assert_eq!(
                answered
                    .headers()
                    .get("content-length")
                    .and_then(|value| value.to_str().ok()),
                Some(NO_HANDLER_RESPONSE.len().to_string().as_str()),
                "case {case:?}: expected the length of the GET's body"
            );
            assert_eq!(
                answered.body().len(),
                expected_len,
                "case {case:?}: expected a body of {expected_len} bytes, got {}",
                answered.body().len()
            );
        }
    }

    /// RULE: the response a client is served at a published address is the one the
    /// revision's code produced — its status, its headers and its body — and a `HEAD`
    /// carries that answer's metadata without its body.
    #[test]
    fn a_runs_answer_is_served_as_the_code_produced_it() {
        let answer = web_runtime::HandlerResponse {
            status: 201,
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("x-served-by".to_string(), "the handler".to_string()),
            ],
            body: b"{\"ok\":true}".to_vec(),
        };
        for (case, head, expected_len) in [("a GET", false, answer.body.len()), ("a HEAD", true, 0)]
        {
            let served = handler_answer(head, &answer);
            let header = |name: &str| {
                served
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            assert_eq!(
                served.status().as_u16(),
                201,
                "case {case:?}: expected the status the code set, got {}",
                served.status()
            );
            assert_eq!(
                (header("content-type"), header("x-served-by")),
                (
                    "application/json".to_string(),
                    "the handler".to_string()
                ),
                "case {case:?}: expected the headers the code set, got {:?}",
                served.headers()
            );
            assert_eq!(
                header("content-length"),
                answer.body.len().to_string(),
                "case {case:?}: expected the length of what a GET would carry"
            );
            assert_eq!(
                served.body().len(),
                expected_len,
                "case {case:?}: expected a body of {expected_len} bytes, got {}",
                served.body().len()
            );
        }
    }
}
