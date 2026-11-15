// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS one function URL is published on, and the listener that answers there.
//!
//! `FunctionUrlConfig$FunctionUrl` is "the HTTP URL endpoint for your function", handed to a
//! caller to FETCH. AWS puts the identity of that endpoint in its HOST
//! (`<url-id>.lambda-url.<region>.on.aws`), and that is what every consumer of a published
//! endpoint relies on: a consumer carries a HOST and supplies the request path itself — a
//! CloudFront `Origin$DomainName`, an API Gateway integration's host, an operator's
//! `curl https://<host>/<path>` — so an address whose identity lives in a path segment
//! resolves to nothing for them.
//!
//! So each function URL gets a listener of the EMULATOR's own, opened through the core
//! (`engine.listeners().tcp_plaintext(..)`, which owns the socket and the bind interface),
//! and the URL is announced at the `host:port` that listener answers on: ONE listener per
//! function URL, so the ADDRESS identifies the URL and nothing else has to be carried
//! alongside it. A name under a zone this emulator does not serve would resolve to nothing on
//! the machine it runs on, and the emulator's own shared endpoint names no function URL at
//! all.
//!
//! A client reaches that address over HTTP or over HTTPS — a function URL is "a dedicated
//! HTTP(S) endpoint" — so both are served at the one address: the connection's first byte
//! says which the client opened with, and a TLS record is handed to the core's own TLS server
//! before the request is read ([`::service_common::in_band_tls`], so this service never
//! touches a key or a certificate).
//!
//! The listener is the ADDRESS, not the data plane: everything a request to a function URL is
//! answered BY — the stored configuration, the `AuthType` the emulator verifies the caller's
//! signature under, the function's own resource policy, the CORS answer and the invocation
//! itself — stays in [`crate::service::function_url`], which serves the same request however
//! it was addressed. So the handler here hands the request it accepts to that one plane,
//! through the in-process transport this emulator reaches itself over, addressed as the
//! RESERVED data-plane path of the URL it was opened for
//! (`/_aws/lambda-url/{url-id}/{request-path}`): one serving path, reached from several
//! addressings.
//!
//! An address OUTLIVES the configuration published on it, as a function URL's own host does:
//! the handle is held on the store under the url id
//! ([`crate::state::ComAmazonawsLambdaAwsgirapiserviceState::function_url_endpoints`]), so a
//! client reading an endpoint whose configuration was deleted is told it is not admitted
//! rather than that nothing is there. Dropping the handle closes the port, and that happens
//! when the room it holds is needed
//! ([`crate::state::ComAmazonawsLambdaAwsgirapiserviceState::make_room_for_a_function_url_endpoint`]).
//!
//! The address it answers at is recorded engine-wide as one THIS service answers at
//! ([`::service_common::published_data_planes`]) and withdrawn when the port closes. Nobody
//! can read a service off a `host:port`, so that record is what lets anything else placing a
//! request addressed there resolve it to this service.
//!
//! # Which clients an address admits
//!
//! This address is the emulator's own loopback port, so a page the developer's browser visits
//! can send requests to it. Every request is therefore put through the core's own browser
//! check before the plane sees it, against the origins the URL's own `Cors$AllowOrigins`
//! names — a statement made through the CONTROL plane, which no browser reaches — so a
//! request a browser sent from another site is refused unless the configuration admits that
//! origin, and a URL configured with no CORS settings admits none, which is the core's own
//! default.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ::bytes::Bytes;
use ::emulator_server::{
    listener_caps, BrowserVerdict, ListenerConn, RemoteOrigins, ResourceListener, TlsUpgrader,
};
use ::http_body_util::{BodyExt as _, Full, Limited};
use ::smithy_server_runtime::EmbeddedEmulator;
use ::tokio::sync::{OwnedSemaphorePermit, Semaphore};
use aws_smithy_runtime_api::client::http::HttpConnector;
use aws_smithy_runtime_api::http::Request as RtRequest;
use aws_smithy_types::body::SdkBody;

use super::ComAmazonawsLambdaAwsgirapiserviceState as ServiceState;

/// How many request BODIES this service reads at once across every function URL's listener:
/// the core's own HTTP in-flight cap, one permit per request, taken before a byte of a body
/// is read and released when the answer is written.
///
/// ONE semaphore for the whole service, because a caller chooses how many function URLs
/// exist and each one would otherwise raise the peak again. A request that finds no permit is
/// shed, which is what the core's own HTTP listener answers such a request.
fn in_flight() -> Arc<Semaphore> {
    static IN_FLIGHT: ::std::sync::OnceLock<Arc<Semaphore>> = ::std::sync::OnceLock::new();
    Arc::clone(IN_FLIGHT.get_or_init(|| Arc::new(Semaphore::new(listener_caps::MAX_HTTP_INFLIGHT))))
}

/// How long one request on this address may take, and how much of a body it may carry: the
/// core's own ceilings for its HTTP listeners, so this address and a core listener bound a
/// request alike.
fn request_deadline() -> Duration {
    Duration::from_secs(listener_caps::HTTP_REQUEST_DEADLINE_SECS)
}

/// See [`request_deadline`].
fn body_ceiling() -> usize {
    usize::try_from(listener_caps::MAX_BYTES_PER_CONN).unwrap_or(usize::MAX)
}

/// The reserved path a request is handed to the plane under, ahead of the url id:
/// `/_aws/lambda-url/{url-id}/{request-path}`.
///
/// The segments are [`crate::state::function_url`]'s own LOCAL-ENDPOINT addressing, which
/// that module reads on whatever host a request arrives at — so the request this listener
/// forwards resolves to exactly the function URL it was opened for, whatever the request path
/// spells.
fn forwarded_prefix(url_id: &str) -> String {
    format!(
        "/{marker}/{label}/{url_id}",
        marker = ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
        label = super::function_url::FUNCTION_URL_LABEL,
    )
}

/// This service's own endpoint host in `region`, which the forwarded leg is addressed at:
/// `lambda.<region>.amazonaws.com`, the host AWS serves this API at and the one the
/// emulator's front door resolves this service from.
fn service_endpoint(region: &str) -> String {
    format!("lambda.{region}.amazonaws.com")
}

/// The headers the forwarded leg does not copy from the client's request.
///
/// `Host` is the listener's own address, which the forwarded request replaces with this
/// service's endpoint (the plane resolves the function URL from the reserved path instead),
/// and the two framing headers describe a body this leg re-frames itself.
///
/// The BROWSER-SET headers are not copied either, and for a different reason: they are the
/// browser's own classification of the request it sent to THIS address, which is where that
/// classification is read and the request admitted or refused
/// ([`refused_before_the_plane`]). The leg below is this emulator calling itself, not a
/// browser calling anything, and a leg carrying them is one the emulator's own front door
/// refuses as a cross-site browser request — for a request this address has already admitted.
/// The one value the plane still needs, the client's `Origin`, is what its CORS answer is
/// built from, so it travels under the reserved name of this emulator's own below.
const HEADERS_NOT_FORWARDED: [&str; 7] = [
    "host",
    "content-length",
    "transfer-encoding",
    "origin",
    "sec-fetch-site",
    "sec-fetch-mode",
    "sec-fetch-dest",
];

/// The reserved name the client's `Origin` is forwarded under, read by the plane that answers
/// the CORS half of a function URL request.
pub const CLIENT_ORIGIN_HEADER: &str = "x-amz-emulator-function-url-origin";

/// One function URL's published address, and the handle that keeps it open.
pub struct FunctionUrlEndpoint {
    /// The `host:port` a client dials, which is the authority of the announced
    /// `FunctionUrlConfig$FunctionUrl`.
    authority: String,
    /// The engine-wide record of which service answers at [`Self::authority`], withdrawn when
    /// this handle is dropped.
    published: Option<::service_common::published_data_planes::PublishedPlanes>,
    /// The listener itself. Never read: it is held because dropping it closes the port.
    _listener: ResourceListener,
}

/// Withdraws the engine-wide record with the port: the listener closes when this handle is
/// dropped, so nothing answers at that address any more and a caller addressed there must not
/// be routed as though something did.
impl Drop for FunctionUrlEndpoint {
    fn drop(&mut self) {
        if let Some(published) = &self.published {
            published.withdraw(&self.authority);
        }
    }
}

/// Written by hand rather than derived: the handle holds a core `ResourceListener`, which is
/// a live socket and carries no `Debug`. What is diagnosable is the address it announced.
impl ::std::fmt::Debug for FunctionUrlEndpoint {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("FunctionUrlEndpoint")
            .field("authority", &self.authority)
            .finish_non_exhaustive()
    }
}

impl FunctionUrlEndpoint {
    /// The `host:port` a client reaches this function URL at.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// Open the listener one function URL is served on, and report the address it answers at — or
/// `None` when this engine vends no listeners, in which case the caller falls back to the
/// address the emulator's own endpoint carries.
///
/// `callback` is the handle into the emulator this request is running within
/// (`ctx.callback()`), which is how a request the listener accepts reaches the data plane: the
/// listener is not a second emulator, it is a second ADDRESS of this one.
pub async fn open_function_url_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: EmbeddedEmulator,
    url_id: &str,
    region: &str,
) -> Option<FunctionUrlEndpoint> {
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let handler = FunctionUrlFrontDoor {
        url_id: url_id.to_string(),
        endpoint: service_endpoint(region),
        state: shared.state::<ServiceState>(),
        callback,
        // The core's own TLS server, lent to this plaintext listener so the `https://` half
        // of the address is terminated by the core rather than here. A listener this emulator
        // could prepare none for serves the `http://` half alone.
        upgrader: listeners.tls_upgrader().ok(),
    };
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one built
    // with no IO driver, which is what a host that drives this emulator in process may hand a
    // request — leaves this create without an address of its own instead of taking the
    // request down with it: the join answers `Err`, this answers `None`, and the caller falls
    // back to the address the emulator's own endpoint carries.
    let opened = ::tokio::spawn(async move { listeners.tcp_plaintext(handler).await.ok() }).await;
    let listener = opened.ok().flatten()?;
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);
    // An address nobody can read the service off — it is a `host:port`, not an endpoint
    // hostname — so the one record of WHICH service answers there is the engine-wide one,
    // written here and withdrawn when the port closes.
    let published = ::service_common::published_data_planes::PublishedPlanes::of(shared);
    if let Some(published) = &published {
        published.publish(&authority, crate::ROUTE_KEY);
    }
    Some(FunctionUrlEndpoint {
        authority,
        published,
        _listener: listener,
    })
}

/// Serves ONE function URL's published address: it hands every request it accepts to this
/// service's own function URL plane, over the in-process transport, and answers with what
/// that plane answered.
struct FunctionUrlFrontDoor {
    url_id: String,
    /// This service's own endpoint host, which the forwarded leg is addressed at.
    endpoint: String,
    /// This service's store, read for the origins the URL's CORS settings admit.
    state: ::engine::ServiceState<ServiceState>,
    callback: EmbeddedEmulator,
    upgrader: Option<TlsUpgrader>,
}

impl ::emulator_server::NativeHandler for FunctionUrlFrontDoor {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(serve_connection(
            self.url_id.clone(),
            self.endpoint.clone(),
            self.state.clone(),
            self.callback.clone(),
            self.upgrader.clone(),
            conn,
        ))
    }
}

/// Speak HTTP/1.1 on one accepted connection until the client closes it, answering each
/// request from this service's own function URL plane.
///
/// The encryption is negotiated in band first ([`::service_common::in_band_tls`]), so one
/// address serves the `http://` and the `https://` a client may dial. HTTP/1.1 only, as the
/// emulator's own front door offers: every request is fully buffered, so nothing relies on
/// HTTP/2 streaming.
async fn serve_connection(
    url_id: String,
    endpoint: String,
    state: ::engine::ServiceState<ServiceState>,
    callback: EmbeddedEmulator,
    upgrader: Option<TlsUpgrader>,
    conn: ListenerConn,
) {
    let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
        return;
    };
    let io = ::hyper_util::rt::TokioIo::new(conn);
    let service = ::hyper::service::service_fn(move |request: ::http::Request<_>| {
        let url_id = url_id.clone();
        let endpoint = endpoint.clone();
        let state = state.clone();
        let callback = callback.clone();
        async move {
            // A panic anywhere behind this address — in the dispatch machinery or in the
            // handler of the operation the forwarded leg reaches — is CONTAINED here and
            // answered as the protocol-shaped internal failure, exactly as the endpoint a
            // control call arrives on answers the same request: one client cannot take the
            // connection, the listener or the process down with it.
            let facts = ::emulator_server::RequestFacts::from_request(&request);
            Ok::<_, ::std::convert::Infallible>(
                ::emulator_server::contain_panics(
                    facts,
                    answer(url_id, endpoint, state, callback, request),
                )
                .await,
            )
        }
    });
    if let Err(error) = ::hyper::server::conn::http1::Builder::new()
        .timer(::hyper_util::rt::TokioTimer::new())
        .header_read_timeout(request_deadline())
        .serve_connection(io, service)
        .await
    {
        // A connection the client dropped, or a request this emulator could not frame, ends
        // here and takes nothing else with it: the next connection is served normally.
        ::tracing::debug!(%error, "a connection to a function URL address ended before its response");
    }
}

/// Answer one request to the published address of the function URL this listener was opened
/// for: the plane's own answer, reached over the in-process transport.
async fn answer(
    url_id: String,
    endpoint: String,
    state: ::engine::ServiceState<ServiceState>,
    callback: EmbeddedEmulator,
    request: ::http::Request<::hyper::body::Incoming>,
) -> ::http::Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if let Some(refused) = refused_before_the_plane(&parts, &state, &url_id) {
        return refused;
    }
    // One permit per request IN FLIGHT, taken BEFORE a byte of the body is read.
    let Ok(permit) = in_flight().try_acquire_owned() else {
        return generated(SERVICE_UNAVAILABLE, SERVER_BUSY);
    };
    let limited = Limited::new(body, body_ceiling());
    let served = ::tokio::time::timeout(request_deadline(), async move {
        // Held for the whole read and dispatch, and released with it.
        let _permit: OwnedSemaphorePermit = permit;
        let Ok(collected) = limited.collect().await else {
            return generated(PAYLOAD_TOO_LARGE, BODY_TOO_LARGE);
        };
        let Some(forwarded) =
            forwarded(parts, collected.to_bytes(), &url_id, &endpoint)
        else {
            return generated(BAD_REQUEST, UNREADABLE_REQUEST);
        };
        match callback.http_connector().call(forwarded).await {
            Ok(response) => {
                let status = response.status().as_u16();
                let mut answer = ::http::Response::builder().status(status);
                for (name, value) in response.headers().iter() {
                    answer = answer.header(name, value);
                }
                let body = Bytes::from(
                    response
                        .into_body()
                        .bytes()
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default(),
                );
                answer
                    .body(Full::new(body.clone()))
                    .unwrap_or_else(|_| ::http::Response::new(Full::new(body)))
            }
            // The plane is this same process, so this is no answer about a function URL; it
            // is reported as the request this address could not have served.
            Err(_) => generated(BAD_GATEWAY, NO_PLANE),
        }
    })
    .await;
    served.unwrap_or_else(|_| generated(GATEWAY_TIMEOUT, REQUEST_TIMED_OUT))
}

/// The request the plane is handed for one client request: the same method, body and headers,
/// addressed to this service's own endpoint under the reserved data-plane path of the url id
/// this listener serves — so the request path travels where this client can carry it.
///
/// The url-id segment is the only thing that says WHICH function URL the leg reaches, so a
/// request path that could walk out from under it is not forwarded at all
/// ([`within_this_url`]), and the built path is checked to still begin with this URL's own
/// prefix before it is sent. `None` for such a path and for a request that does not render as
/// one — in both cases this listener addresses nothing.
fn forwarded(
    parts: ::http::request::Parts,
    body: Bytes,
    url_id: &str,
    endpoint: &str,
) -> Option<RtRequest> {
    let request_path = within_this_url(parts.uri.path())?;
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let within = forwarded_prefix(url_id);
    let path = format!("{within}{request_path}");
    // The built path still names THIS function URL's plane: a path that does not is not
    // forwarded at all, whatever it would have resolved to.
    if !path.starts_with(&format!("{within}/")) {
        return None;
    }
    let uri = format!("https://{endpoint}{path}{query}");
    let mut builder = ::http::Request::builder()
        .method(parts.method.clone())
        .uri(uri)
        .header(::http::header::HOST, endpoint);
    for (name, value) in parts.headers.iter() {
        if HEADERS_NOT_FORWARDED.contains(&name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    // The client's `Origin`, under the reserved name of this emulator's own: the plane decides
    // the CORS answer from it, and this leg stays a request no browser sent.
    if let Some(origin) = parts
        .headers
        .get(::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        builder = builder.header(CLIENT_ORIGIN_HEADER, origin);
    }
    let built = builder.body(SdkBody::from(body)).ok()?;
    RtRequest::try_from(built).ok()
}

/// The request path as this listener may forward it, or `None` for one that could address
/// something other than the function URL this address was opened for.
///
/// The forwarded leg carries the request path under `/_aws/lambda-url/{url-id}`, and a
/// relative path reference (RFC 3986 `.` and `..` segments, percent-encoded or not) is
/// resolved by whoever resolves that target — so such a path could name a prefix above this
/// URL's own, on the endpoint host this service's CONTROL plane answers on. It is refused
/// here instead: a client addresses an object under a function URL, never a path above the
/// URL it addressed.
///
/// The dot-segment reading is the workspace's one reading of it
/// ([`::service_common::data_plane_addressing::carries_a_dot_segment`]), so this address and
/// every other plane served on a reserved path refuse the same targets.
fn within_this_url(path: &str) -> Option<&str> {
    if !path.starts_with('/') {
        return None;
    }
    (!::service_common::data_plane_addressing::carries_a_dot_segment(path)).then_some(path)
}

/// The transport-safety refusals the core applies to its OWN HTTP listeners, applied here
/// from the core's own predicates so this address and a core listener decide them alike: the
/// `Host` allowlist (the DNS-rebinding guard a browser needs), the WebSocket refusal and the
/// browser check. `None` lets the request through to the plane.
///
/// The browser check runs against the origins the function URL's own `Cors$AllowOrigins`
/// names, carried by the core's own [`RemoteOrigins::Exact`] — read from this service's own
/// records and never from the request. A URL configured with no CORS settings names none,
/// which is the core's own default, admitting none.
///
/// The `allow_remote` the check is given is `true` because this address is a function URL's
/// data plane and nothing else: it invokes one function, never this emulator's control plane
/// or its administrative endpoint, which is what that flag keeps a configured remote origin
/// away from.
fn refused_before_the_plane(
    parts: &::http::request::Parts,
    state: &::engine::ServiceState<ServiceState>,
    url_id: &str,
) -> Option<::http::Response<Full<Bytes>>> {
    let header = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());

    // HTTP/1.1 requires a `Host`, and without an authority the rebinding guard cannot be run
    // at all, so a request carrying none is refused rather than served: this fails CLOSED,
    // exactly as the core's own HTTP listener does.
    let Some(host) = header("host").or_else(|| parts.uri.host()) else {
        return Some(generated(MISDIRECTED_REQUEST, HOST_REQUIRED));
    };
    if !::service_common::data_plane_addressing::host_names_the_bound_interface(host) {
        return Some(generated(MISDIRECTED_REQUEST, HOST_NOT_ALLOWED));
    }
    if ::emulator_server::is_websocket_upgrade(
        header("connection"),
        header("upgrade"),
        header(":protocol"),
    ) {
        return Some(generated(NOT_IMPLEMENTED, NO_WEBSOCKETS));
    }
    if let BrowserVerdict::Forbid(reason) = ::emulator_server::classify_browser_request(
        header("origin"),
        header("sec-fetch-site"),
        &RemoteOrigins::Exact(admitted_origins(state, url_id)),
        true,
    ) {
        return Some(generated(FORBIDDEN, reason));
    }
    None
}

/// The origins the function URL `url_id`'s own `Cors$AllowOrigins` admits, read out of this
/// store: the one statement about cross-origin access its configuration carries. Empty for a
/// URL this store no longer holds and for one configured with no CORS settings.
fn admitted_origins(state: &::engine::ServiceState<ServiceState>, url_id: &str) -> Vec<String> {
    let store = state.lock();
    store
        .functions
        .values()
        .flat_map(|function| function.url_configs.values())
        .find(|config| config.url_id == url_id)
        .and_then(|config| config.cors.as_ref())
        .map(|cors| cors.allow_origins.clone().unwrap_or_default())
        .unwrap_or_default()
}

/// What this address answers a request whose `Host` is not the address it serves, or that
/// names none.
const MISDIRECTED_REQUEST: u16 = 421;

/// What it answers a WebSocket upgrade, which this address does not serve.
const NOT_IMPLEMENTED: u16 = 501;

/// What it answers a request a browser made cross-site.
const FORBIDDEN: u16 = 403;

/// What it answers a request that arrived while this service was already reading as many
/// bodies as it reads at once.
const SERVICE_UNAVAILABLE: u16 = 503;

/// What it answers a request carrying more than one connection may.
const PAYLOAD_TOO_LARGE: u16 = 413;

/// What it answers a request it could not render as one for the plane at all.
const BAD_REQUEST: u16 = 400;

/// What it answers when the plane itself could not be reached.
const BAD_GATEWAY: u16 = 502;

/// What it answers a request that did not finish within one request deadline.
const GATEWAY_TIMEOUT: u16 = 504;

/// The body of an answer for a request shed at the in-flight cap.
const SERVER_BUSY: &str = "This address is serving as many requests as it can. Try again.\n";

/// The body of an answer for a request whose `Host` is not the address it serves.
const HOST_NOT_ALLOWED: &str = "This address does not serve that host.\n";

/// The body of an answer for a request that names no host at all, which no HTTP/1.1 request
/// may.
const HOST_REQUIRED: &str = "This address requires the host it is addressed by.\n";

/// The body of an answer for a WebSocket upgrade, which no function URL serves.
const NO_WEBSOCKETS: &str = "This address does not serve WebSocket connections.\n";

/// The body of an answer for a request larger than this address accepts.
const BODY_TOO_LARGE: &str = "This request is larger than this address accepts.\n";

/// The body of an answer for a request this address could not read as one.
const UNREADABLE_REQUEST: &str = "This request could not be read as an HTTP request.\n";

/// The body of an answer for a request the plane never saw.
const NO_PLANE: &str = "This function URL could not be reached.\n";

/// The body of an answer for a request that outlasted one request deadline.
const REQUEST_TIMED_OUT: &str = "This request took longer than this address allows.\n";

/// One answer of this address's own, for a request the plane never saw. Plain text, and every
/// message is a constant of this module rather than anything built from the request.
fn generated(status: u16, message: &str) -> ::http::Response<Full<Bytes>> {
    let body = Bytes::from(message.to_string());
    ::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Full::new(body.clone()))
        // Every argument is this function's own, so the builder cannot fail; answer a bare
        // status rather than panicking if it ever does.
        .unwrap_or_else(|_| {
            let mut response = ::http::Response::new(Full::new(body));
            *response.status_mut() = ::http::StatusCode::BAD_GATEWAY;
            response
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forwarded leg addresses the LOCAL-ENDPOINT path of the url id this listener was
    /// opened for, whatever path the client dialled it with, so the plane resolves exactly
    /// that function URL.
    ///
    /// The rows span what the rule generalises over: the root, a path of several segments, a
    /// path carrying a query, and a path spelled like another function URL's addressing.
    #[test]
    fn a_request_is_forwarded_under_the_url_id_this_address_serves() {
        let url_id = "0123456789abcdef0123456789abcdef";
        let cases: [(&str, &str, &str, &str); 4] = [
            ("the root", "/", "/_aws/lambda-url/0123456789abcdef0123456789abcdef/", ""),
            (
                "an object",
                "/items/one.json",
                "/_aws/lambda-url/0123456789abcdef0123456789abcdef/items/one.json",
                "",
            ),
            (
                "a path carrying a query",
                "/items?page=2",
                "/_aws/lambda-url/0123456789abcdef0123456789abcdef/items",
                "page=2",
            ),
            (
                "a path spelled like another URL's addressing",
                "/_aws/lambda-url/elsewhere/x",
                "/_aws/lambda-url/0123456789abcdef0123456789abcdef/_aws/lambda-url/elsewhere/x",
                "",
            ),
        ];
        for (case, dialled, expected_path, expected_query) in cases {
            let request = ::http::Request::builder()
                .method("GET")
                .uri(format!("http://127.0.0.1:1/{}", dialled.trim_start_matches('/')))
                .header("host", "127.0.0.1:1")
                .body(())
                .expect("a well-formed request");
            let (parts, ()) = request.into_parts();
            let forwarded = forwarded(parts, Bytes::new(), url_id, "lambda.us-east-1.amazonaws.com")
                .unwrap_or_else(|| panic!("case {case:?}: the request renders for the plane"));
            let uri = forwarded.uri().to_string();
            let expected = if expected_query.is_empty() {
                format!("https://lambda.us-east-1.amazonaws.com{expected_path}")
            } else {
                format!("https://lambda.us-east-1.amazonaws.com{expected_path}?{expected_query}")
            };
            assert_eq!(
                uri, expected,
                "case {case:?}: expected the leg addressed at {expected:?}, got {uri:?}"
            );
        }
    }

    /// RULE: a request path that could walk out from under the url-id segment is not
    /// forwarded at all — that segment is the only thing that says which function URL (and
    /// which plane of this service's endpoint) the leg may reach. The dot-segment reading is
    /// the workspace's own, so what this address refuses is what every plane served on a
    /// reserved path refuses.
    ///
    /// The rows span WHERE the dot segment sits and HOW it is spelled — a leading, a middle
    /// and a trailing segment, a single dot, percent-encoded dots in either case, and a dot
    /// segment hidden behind an encoded separator — beside the paths that are still forwarded:
    /// a dot inside a name, a hidden file, the root, and a doubly-encoded dot segment, which
    /// is a literal segment to the one decoding this emulator makes of a target.
    #[test]
    fn a_path_that_could_leave_this_url_is_not_forwarded() {
        let cases: [(&str, &str, bool); 16] = [
            ("the root", "/", true),
            ("an object", "/items/one.json", true),
            ("a dot inside a segment", "/a..b/c", true),
            ("a hidden file", "/.well-known/x", true),
            ("three dots", "/.../x", true),
            ("an encoded space in a name", "/my%20items/one", true),
            ("a doubly-encoded dot segment, a literal segment", "/%252e%252e/x", true),
            ("a leading parent segment", "/../2015-03-31/functions", false),
            ("a parent segment in the middle", "/items/../../x", false),
            ("a trailing parent segment", "/items/..", false),
            ("a single dot segment", "/./items", false),
            ("a percent-encoded parent segment", "/%2e%2e/x", false),
            ("a percent-encoded parent segment, upper case", "/%2E%2E/x", false),
            ("a percent-encoded dot segment", "/%2e/x", false),
            ("a parent segment behind an encoded separator", "/items%2f..%2fx", false),
            ("a parent segment behind an encoded separator, upper case", "/items%2F..%2Fx", false),
        ];
        for (case, path, forwarded_on) in cases {
            let actual = within_this_url(path).is_some();
            assert_eq!(
                actual, forwarded_on,
                "case {case:?} ({path:?}): expected the path to be forwarded == \
                 {forwarded_on}, got {actual}"
            );
        }
    }

    /// The browser-set headers a request carried are not copied onto the forwarded leg — that
    /// leg is this emulator calling itself — while the client's `Origin` travels under the
    /// reserved name the plane reads its CORS answer from.
    #[test]
    fn the_forwarded_leg_carries_the_origin_under_the_reserved_name_alone() {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("http://127.0.0.1:1/invoke")
            .header("host", "127.0.0.1:1")
            .header("origin", "https://page.example")
            .header("sec-fetch-site", "cross-site")
            .header("authorization", "AWS4-HMAC-SHA256 Credential=x")
            .body(())
            .expect("a well-formed request");
        let (parts, ()) = request.into_parts();
        let forwarded = forwarded(parts, Bytes::new(), "abc", "lambda.us-east-1.amazonaws.com")
            .expect("the request renders for the plane");
        for (case, name, expected) in [
            ("the browser's own classification", "sec-fetch-site", None),
            ("the client's origin", "origin", None),
            (
                "the origin under the reserved name",
                CLIENT_ORIGIN_HEADER,
                Some("https://page.example"),
            ),
            (
                "the caller's credential",
                "authorization",
                Some("AWS4-HMAC-SHA256 Credential=x"),
            ),
        ] {
            let carried = forwarded.headers().get(name);
            assert_eq!(
                carried, expected,
                "case {case:?}: expected {name:?} to be {expected:?}, got {carried:?}"
            );
        }
    }
}
