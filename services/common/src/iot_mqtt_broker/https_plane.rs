// SPDX-License-Identifier: CC0-1.0

//! The HTTPS half of the broker's endpoint: the IoT data-plane API, served at the same
//! address MQTT is.
//!
//! AWS publishes ONE host for an account's data endpoint and serves MQTT over TLS and the
//! HTTPS data-plane API on it, so a client that discovered that address uses it for both —
//! an SDK data client built with it publishes, reads shadows and reports job executions
//! over HTTPS, while a device connects its MQTT session to it. The address this emulator
//! publishes is the broker's socket ([`super::endpoint`]), so that socket has to answer
//! HTTP as well, or discovering the endpoint would cost a client the API half.
//!
//! WHAT ANSWERS is not written here: every request this socket accepts is handed to the
//! router serving the call that opened it, over the in-process transport this emulator
//! reaches itself through, so the same surface answers whether a request arrived here or at
//! the emulator's own front door — and a request presenting a credential is verified by the
//! emulator's own authenticator, as it is everywhere else.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ::aws_smithy_runtime_api::client::http::HttpConnector;
use ::aws_smithy_runtime_api::http::Request as RuntimeRequest;
use ::aws_smithy_types::body::SdkBody;
use ::bytes::Bytes;
use ::emulator_server::{listener_caps, ListenerConn};
use ::http_body_util::{BodyExt as _, Full, Limited};
use ::smithy_server_runtime::EmbeddedEmulator;
use ::tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The scheme a request forwarded from this socket is written under: this emulator serves
/// TLS, and the address is published for a client that dials `https://`.
const FORWARDED_SCHEME: &str = "https";

/// Serve HTTP/1.1 on one accepted connection until the client closes it, answering each
/// request from this emulator's router.
///
/// HTTP/1.1 only, as the emulator's own front door offers and as an SDK client speaks:
/// every request is fully buffered, so nothing relies on HTTP/2 streaming. The header-read
/// deadline and the body ceiling are the core's, so a client that dribbles a request line,
/// or sends more than one connection may carry, cannot hold the connection permit the core
/// issued for it.
pub async fn serve(callback: EmbeddedEmulator, conn: ListenerConn) {
    let io = ::hyper_util::rt::TokioIo::new(conn);
    let service = ::hyper::service::service_fn(move |request: ::http::Request<_>| {
        let callback = callback.clone();
        async move {
            // A panic anywhere behind this address — in the dispatch machinery or in the
            // surface the forwarded request reaches — is CONTAINED here and answered as
            // the protocol-shaped internal failure, exactly as the endpoint a control call
            // arrives on answers the same request: one client cannot take the connection,
            // the listener or the process down with it.
            let facts = ::emulator_server::RequestFacts::from_request(&request);
            Ok::<_, ::std::convert::Infallible>(
                ::emulator_server::contain_panics(facts, answer(callback, request)).await,
            )
        }
    });
    if let Err(error) = ::hyper::server::conn::http1::Builder::new()
        .timer(::hyper_util::rt::TokioTimer::new())
        .header_read_timeout(request_deadline())
        .serve_connection(io, service)
        .await
    {
        // A connection the client dropped, or a request this emulator could not frame,
        // ends here and takes nothing else with it: the next connection is served
        // normally.
        ::tracing::debug!(
            %error,
            "a connection to the published IoT endpoint ended before its response"
        );
    }
}

/// How many request BODIES this address reads at once: the core's own HTTP in-flight cap,
/// one permit per request, taken before a byte of a body is read and released when the
/// answer is written.
fn in_flight() -> Arc<Semaphore> {
    static IN_FLIGHT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(
        IN_FLIGHT.get_or_init(|| Arc::new(Semaphore::new(listener_caps::MAX_HTTP_INFLIGHT))),
    )
}

/// How long one request on this address may take, from its first field byte to the answer:
/// the core's own HTTP request deadline.
fn request_deadline() -> Duration {
    Duration::from_secs(listener_caps::HTTP_REQUEST_DEADLINE_SECS)
}

/// How many bytes of one request this address reads: the core's per-connection ceiling.
fn body_ceiling() -> usize {
    usize::try_from(listener_caps::MAX_BYTES_PER_CONN).unwrap_or(usize::MAX)
}

/// Answer one request to the published address from this emulator's router.
async fn answer(
    callback: EmbeddedEmulator,
    request: ::http::Request<::hyper::body::Incoming>,
) -> ::http::Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if let Some(refused) = refused_before_dispatch(&parts) {
        return refused;
    }
    let Ok(permit) = in_flight().try_acquire_owned() else {
        return plain(503, SERVER_BUSY);
    };
    let limited = Limited::new(body, body_ceiling());
    let served = ::tokio::time::timeout(request_deadline(), async move {
        // Held for the whole read and dispatch, and released with it.
        let _permit: OwnedSemaphorePermit = permit;
        let Ok(collected) = limited.collect().await else {
            return plain(413, BODY_TOO_LARGE);
        };
        let Some(forwarded) = forwarded(parts, collected.to_bytes()) else {
            return plain(400, NOT_A_REQUEST);
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
            // The router is this same process, so this is no answer about an IoT
            // resource; it is reported as an answer this address could not produce.
            Err(_unserved) => plain(502, NOT_SERVED),
        }
    })
    .await;
    served.unwrap_or_else(|_| plain(504, REQUEST_TIMED_OUT))
}

/// The request the router is handed: the same method, path, query, body and fields — the
/// `Host` among them, so a service claims the request by its credential scope and path
/// exactly as it does at the front door.
///
/// `None` for a request that does not render as one at all, which addresses nothing.
fn forwarded(parts: ::http::request::Parts, body: Bytes) -> Option<RuntimeRequest> {
    let host = parts
        .headers
        .get(::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| parts.uri.host())?;
    let query = parts
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let uri = format!("{FORWARDED_SCHEME}://{host}{}{query}", parts.uri.path());
    let mut builder = ::http::Request::builder()
        .method(parts.method.clone())
        .uri(uri);
    for (name, value) in parts.headers.iter() {
        // The two framing fields describe a body this leg re-frames itself.
        if name == ::http::header::CONTENT_LENGTH || name == ::http::header::TRANSFER_ENCODING {
            continue;
        }
        builder = builder.header(name, value);
    }
    let built = builder.body(SdkBody::from(body)).ok()?;
    RuntimeRequest::try_from(built).ok()
}

/// The refusals this address decides itself, before any request reaches the router: the
/// transport-safety checks the core applies to its OWN HTTP listeners, taken from the
/// core's own predicates so this address and a core listener decide them alike. `None` lets
/// the request through.
fn refused_before_dispatch(
    parts: &::http::request::Parts,
) -> Option<::http::Response<Full<Bytes>>> {
    let sent = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());

    // HTTP/1.1 requires a `Host`, and without an authority the rebinding guard cannot be
    // run at all, so a request carrying none is refused rather than served: this fails
    // CLOSED, exactly as the core's own HTTP listener does.
    let Some(host) = sent("host").or_else(|| parts.uri.host()) else {
        return Some(plain(421, HOST_REQUIRED));
    };
    // The addresses of the interface this emulator's listeners are bound on, which is the
    // interface this socket is bound on and the host form the published address carries.
    // Any other host is one somebody else's resolver decides, which is what the rebinding
    // guard refuses.
    if !crate::data_plane_addressing::host_names_the_bound_interface(host) {
        return Some(plain(421, HOST_NOT_ALLOWED));
    }
    if ::emulator_server::is_websocket_upgrade(
        sent("connection"),
        sent("upgrade"),
        sent(":protocol"),
    ) {
        return Some(plain(501, NO_WEBSOCKETS));
    }
    if let ::emulator_server::BrowserVerdict::Forbid(reason) =
        ::emulator_server::classify_browser_request(
            sent("origin"),
            sent("sec-fetch-site"),
            // This address is the emulator's own, reached from the machine it runs on:
            // the same default a core HTTP listener carries, which admits no remote
            // origin.
            &::emulator_server::RemoteOrigins::SameOriginOnly,
            false,
        )
    {
        return Some(plain(403, reason));
    }
    // A target that resolves out of the path it names addresses another path entirely,
    // and the path is part of what says whose surface must answer.
    if crate::data_plane_addressing::carries_a_dot_segment(parts.uri.path()) {
        return Some(plain(400, NOT_A_REQUEST));
    }
    None
}

/// One answer of this address's own, for a request the router never saw. Plain text rather
/// than a modelled envelope: which protocol an answer belongs in is the claiming service's
/// to decide, and no service was asked about this one.
fn plain(status: u16, message: &str) -> ::http::Response<Full<Bytes>> {
    let body = Bytes::from(message.to_string());
    ::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", body.len().to_string())
        .body(Full::new(body.clone()))
        // Every argument is this function's own, so the builder cannot fail; answer a bare
        // status rather than panicking if it ever does.
        .unwrap_or_else(|_| {
            let mut response = ::http::Response::new(Full::new(body));
            *response.status_mut() = ::http::StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

/// What this address answers a request whose `Host` is not the address it serves.
const HOST_NOT_ALLOWED: &str = "This address does not serve that host.\n";

/// What it answers a request that names no host at all, which no HTTP/1.1 request may.
const HOST_REQUIRED: &str = "This address requires the host it is addressed by.\n";

/// What it answers a WebSocket upgrade, which this address does not serve.
const NO_WEBSOCKETS: &str = "This address does not serve WebSocket connections.\n";

/// What it answers a request carrying more than one connection may.
const BODY_TOO_LARGE: &str = "This request is larger than this address accepts.\n";

/// What it answers a request that did not finish within one request deadline.
const REQUEST_TIMED_OUT: &str = "This request took longer than this address allows.\n";

/// What it answers a request that arrived while this address was already reading as many
/// bodies as it reads at once.
const SERVER_BUSY: &str = "This address is serving as many requests as it can. Try again.\n";

/// What it answers a request it could not read as one at all.
const NOT_A_REQUEST: &str = "This request could not be read.\n";

/// What it answers a request the router did not answer.
const NOT_SERVED: &str = "This request was not served.\n";
