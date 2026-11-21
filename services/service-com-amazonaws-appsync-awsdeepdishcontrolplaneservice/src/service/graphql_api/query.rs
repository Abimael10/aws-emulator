// SPDX-License-Identifier: CC0-1.0

//! A GraphQL API's `GRAPHQL` endpoint — `GraphqlApi$uris`'s `GRAPHQL` entry — and the
//! GraphQL request it serves.
//!
//! What it serves, over HTTP/1.1 on the API's own socket:
//!
//! > `POST /graphql` with `{"query": …, "variables": …, "operationName": …}` executes that
//! > document against the API's own resolvers ([`super::execution`]) and answers the GraphQL
//! > result: `data` for what resolved, and `errors` for what did not.
//!
//! Two decisions are made before any of the document is read, in this order, because that is
//! the order a request meets them on the way to an AppSync API:
//!
//! 1. The WEB ACL in front of the API, if WAF was asked to put one there. `AssociateWebACL`
//!    binds a web ACL to "an AppSync GraphQL API" by the API's own ARN, and the decision is
//!    WAFv2's own ([`::service_com_amazonaws_wafv2_awswaf_20190729::service::edge::decide`])
//!    — this module holds no second reading of a rule tree. A terminating action is answered
//!    HERE and nothing of the API is reached: a `Block` with no `CustomResponse` is the
//!    `403` AppSync's own edge answers, carrying the one error AppSync words it with
//!    ([`WAF_FORBIDDEN_ERROR_TYPE`], [`WAF_FORBIDDEN_MESSAGE`]).
//! 2. The AUTHORIZATION the request presents, by the rule AppSync applies to a call on a
//!    GraphQL API ([`super::authorized`]): the same rule the API's real-time endpoint
//!    applies to a connection, over the same members.
//!
//! The transport-safety decisions the core makes for its own HTTP listeners are applied here
//! from the core's own predicates — the `Host` allowlist that guards against DNS rebinding,
//! the WebSocket refusal (the real-time endpoint is the other socket) and the browser
//! (`Origin` / `Sec-Fetch-Site`) check — and the request is read under one deadline and one
//! body ceiling, both the core's, so a slow or oversized client cannot hold a connection
//! permit open. The browser check admits ONE client more than a core listener does, because
//! this endpoint is one a PAGE calls: it answers the CORS preflight itself
//! ([`preflight_response`]) and serves the real request of a page served from THIS MACHINE
//! ([`served_from_this_machine`]), stating what that page may read of the answer
//! ([`cross_origin_permission`]); a page of any other site is refused before anything of the
//! API is reached.
//!
//! Every answer this endpoint writes — the result, and each refusal above — carries the
//! REQUEST ID of the request it answers in [`REQUEST_ID_FIELD`], minted per request by the
//! API's own data plane (`crate::state::GraphqlDataPlane::mint_request_id`). It is the value
//! a client correlates its answer with what the API LOGGED about producing it by, because
//! every record an API with a `logConfig` writes carries that same id
//! ([`super::field_logs`]). A request the API itself answered is also a request it LOGS;
//! four are not, because AppSync produced no answer of its own for them: one the WEB ACL
//! decided at the edge, one this endpoint never read (shed for want of a permit, past the
//! body ceiling, or past the read deadline), one refused by the transport-safety rules
//! before any of it was dispatched, and one answered at the CORS preflight, which reaches
//! neither the API's authorization nor a document.

use ::std::future::Future;
use ::std::pin::Pin;
use ::std::sync::Arc;
use ::std::time::{Duration, Instant, SystemTime};

use ::bytes::Bytes;
use ::emulator_server::{listener_caps, ListenerConn, NativeHandler, TlsUpgrader};
use ::http_body_util::{BodyExt as _, Full, Limited};
use ::serde_json::{Map as JsonMap, Value as Json};
use ::service_com_amazonaws_wafv2_awswaf_20190729::service::edge as waf_edge;
use ::tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::execution::{self, GraphqlRequest};
use super::{authorizer, caller, field_auth, field_logs, user_pool, PlaneContext};
use crate::service::event_api::{self, PresentedAuth};

/// The response field every answer this endpoint writes carries the request's own id in, as
/// AppSync names it.
pub const REQUEST_ID_FIELD: &str = "x-amzn-requestid";

/// The path the GraphQL endpoint serves its API at — the path `uris`'s `GRAPHQL` entry
/// carries, which is the same `/graphql` the real-time endpoint is addressed at: the two are
/// distinct endpoints by ADDRESS, not by path.
pub const GRAPHQL_PATH: &str = "/graphql";

/// The `errorType` and the sentence AppSync's edge answers a request a web ACL BLOCKED
/// with: the fixed envelope of a WAF-forbidden GraphQL request, which names nothing of the
/// request or of the rule that blocked it.
pub const WAF_FORBIDDEN_ERROR_TYPE: &str = "WAFForbiddenException";
pub const WAF_FORBIDDEN_MESSAGE: &str = "403 Forbidden";

/// The status that refusal carries, which is also the status WAF serves a `Block` under
/// (`waf_edge::EdgeAction::default_status`).
const FORBIDDEN_STATUS: u16 = 403;

/// Serves one GraphQL API's `GRAPHQL` endpoint: the socket that endpoint's published address
/// names.
pub struct QuerySocket {
    /// What every request on this socket is answered from.
    context: PlaneContext,
    /// The core's TLS server, for a client that dialled `https://`.
    upgrader: Option<TlsUpgrader>,
    /// How many requests this endpoint reads at once, which is what bounds the memory the
    /// bodies in flight on it hold: the core's own HTTP in-flight cap, one permit per
    /// request, taken before a body is read and released when the answer is written.
    in_flight: Arc<Semaphore>,
}

impl QuerySocket {
    /// The endpoint of one API.
    pub fn new(context: PlaneContext, upgrader: Option<TlsUpgrader>) -> Self {
        Self {
            context,
            upgrader,
            in_flight: Arc::new(Semaphore::new(listener_caps::MAX_HTTP_INFLIGHT)),
        }
    }
}

impl NativeHandler for QuerySocket {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let context = self.context.clone();
        let upgrader = self.upgrader.clone();
        let in_flight = Arc::clone(&self.in_flight);
        Box::pin(async move {
            let Some(conn) = event_api::negotiated(conn, upgrader).await else {
                return;
            };
            serve_connection(context, in_flight, conn).await;
        })
    }
}

/// Speak HTTP/1.1 on one accepted connection until the client closes it.
async fn serve_connection(context: PlaneContext, in_flight: Arc<Semaphore>, conn: ListenerConn) {
    let api_id = context.api_id.clone();
    let io = ::hyper_util::rt::TokioIo::new(conn);
    let service = ::hyper::service::service_fn(move |request| {
        let context = context.clone();
        let in_flight = Arc::clone(&in_flight);
        async move {
            // The id of THIS request, minted by the API's own data plane before anything of
            // the request is read, so every answer below — including one this endpoint
            // refuses and a contained panic — names the request it is of.
            let request_id = context.state.lock().graphql_data_plane.mint_request_id();
            // Every request's whole answer is driven through the core's panic containment,
            // because this socket is served by this module rather than by the front door's
            // router: there is no middleware layer to carry it.
            let facts = ::emulator_server::RequestFacts::from_request(&request);
            // What the answer may state about reading it from a PAGE, read before the request
            // is consumed: `None` for a request this endpoint routes nowhere
            // ([`cross_origin_permission`]).
            let permission = cross_origin_permission(
                request.method(),
                request.uri().path(),
                request.headers(),
                &context.remote_origins,
            );
            let answered = contained(
                facts,
                answer(context, in_flight, request, request_id.clone()),
            )
            .await;
            Ok::<_, ::std::convert::Infallible>(stamped(answered, &request_id, permission))
        }
    });
    if let Err(error) = ::hyper::server::conn::http1::Builder::new()
        .timer(::hyper_util::rt::TokioTimer::new())
        .header_read_timeout(request_deadline())
        .serve_connection(io, service)
        .await
    {
        // A connection the client dropped, or a request this endpoint could not frame,
        // ends here and takes nothing else with it.
        ::tracing::debug!(
            %api_id,
            %error,
            "a connection to a GraphQL API's endpoint ended before its response"
        );
    }
}

/// Drive one request's whole answer under the core's own PANIC CONTAINMENT
/// (`emulator_server::contain_panics`), and word a contained panic in this endpoint's own
/// envelope — for the reason the Event API's HTTP endpoint does it
/// ([`crate::service::event_api::publish`] states it in full).
async fn contained<F>(
    facts: ::emulator_server::RequestFacts,
    answer: F,
) -> ::http::Response<Full<Bytes>>
where
    F: Future<Output = ::http::Response<Full<Bytes>>> + Send + 'static,
{
    let guarded: ::http::Response<Bytes> = ::emulator_server::contain_panics(facts, async move {
        let (parts, body) = answer.await.into_parts();
        let bytes = body
            .collect()
            .await
            .map_or_else(|_| Bytes::new(), |collected| collected.to_bytes());
        ::http::Response::from_parts(parts, bytes)
    })
    .await;
    let (parts, bytes) = guarded.into_parts();
    if parts.status == ::http::StatusCode::INTERNAL_SERVER_ERROR {
        return error_response(
            500,
            event_api::publish::INTERNAL_FAILURE_ERROR_TYPE,
            event_api::publish::INTERNAL_FAILURE_MESSAGE,
        );
    }
    ::http::Response::from_parts(parts, Full::new(bytes))
}

/// `answered` with the request's own id written on it, and — where the request reached a
/// route of the API — with what a page may read of the answer.
///
/// A minted id is a UUID, so it is always a value a response field can carry; an answer that
/// somehow carries one already keeps it rather than being stamped twice. The cross-origin
/// permission is on every answer the API's own route produced, because a page that made the
/// request has to be able to READ it — an error of this endpoint as much as a result — and on
/// none of the answers to a request that reached no operation of the API, which no route of it
/// produced ([`cross_origin_permission`] decides both).
fn stamped(
    mut answered: ::http::Response<Full<Bytes>>,
    request_id: &str,
    permission: Option<Vec<(&'static str, ::http::HeaderValue)>>,
) -> ::http::Response<Full<Bytes>> {
    let name = ::http::HeaderName::from_static(REQUEST_ID_FIELD);
    if let Ok(value) = ::http::HeaderValue::from_str(request_id) {
        answered.headers_mut().entry(name).or_insert(value);
    }
    for (name, value) in permission.into_iter().flatten() {
        answered
            .headers_mut()
            .entry(::http::HeaderName::from_static(name))
            .or_insert(value);
    }
    answered
}

/// What the answer to this request states about reading it FROM A PAGE — the origin it permits
/// and the response fields a script may read off it — or `None` for a request this endpoint
/// routes nowhere ([`reaches_the_api`]), whose answer no route of the API produced.
///
/// The origin permitted is the REQUEST's own where a page served from that origin may drive
/// this endpoint at all ([`admitted_origin`]) — a page served from this machine, and one the
/// operator asked for — and [`ALLOWED_ORIGIN`] otherwise: for a client that is no page at all
/// (which sends no `Origin`), for the preflight, and for the refusal a page of another site
/// earns, which is what lets that page read WHY it was refused. Because that value DEPENDS on
/// the request's own `Origin`, the answer also says so ([`VARY_FIELD`]), as the core's own
/// HTTP listeners do: a cache holding one answer must not hand it to a page of another origin.
/// A page may read only the safelisted response fields of a cross-origin answer unless the
/// answer says otherwise (Fetch, §3.2.3's `Access-Control-Expose-Headers`), and the two fields
/// this endpoint writes ABOUT the answer — the request's own id and the class of a refusal —
/// are not among them, so they are named under the very names this endpoint writes them under.
fn cross_origin_permission(
    method: &::http::Method,
    path: &str,
    headers: &::http::HeaderMap,
    remote_origins: &::emulator_server::RemoteOrigins,
) -> Option<Vec<(&'static str, ::http::HeaderValue)>> {
    if !reaches_the_api(method, path, headers) {
        return None;
    }
    let origin = headers
        .get(::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| admitted_origin(origin, remote_origins))
        .and_then(|origin| ::http::HeaderValue::from_str(origin).ok())
        .unwrap_or_else(|| ::http::HeaderValue::from_static(ALLOWED_ORIGIN));
    Some(vec![
        (ALLOW_ORIGIN_FIELD, origin),
        (
            EXPOSE_HEADERS_FIELD,
            ::http::HeaderValue::from_static(EXPOSED_FIELDS),
        ),
        (
            VARY_FIELD,
            ::http::HeaderValue::from_static(::http::header::ORIGIN.as_str()),
        ),
    ])
}

/// Whether the request addresses a ROUTE of this endpoint at all: the path the API is served
/// at, addressed with anything but an `OPTIONS` that is no preflight — the preflight being the
/// only `OPTIONS` this endpoint routes ([`is_preflight`]).
///
/// The ONE reading of that question, so what an answer states about reading it from a page
/// cannot drift from the route that produced the answer: [`served_request`] answers each
/// request of a route it does route, and [`cross_origin_permission`] states nothing about one
/// it does not.
fn reaches_the_api(
    method: &::http::Method,
    path: &str,
    headers: &::http::HeaderMap,
) -> bool {
    serves_path(path) && (method != ::http::Method::OPTIONS || is_preflight(method, headers))
}

/// Whether `path` is the one this endpoint serves its API at. The ONE reading of that
/// question, taken by the route ([`reaches_the_api`]) and by the answer
/// ([`served_request`]), so a change to the path cannot leave an answer stating a permission
/// for a route nothing serves.
fn serves_path(path: &str) -> bool {
    path == GRAPHQL_PATH
}

/// Whether the request is a CORS PREFLIGHT: an `OPTIONS` asking about the method a browser is
/// about to send (Fetch, §4.8's "CORS-preflight request"). The ONE reading of that question,
/// which the answer ([`preflight_response`]), the route ([`reaches_the_api`]) and the browser
/// check ([`refused_before_dispatch`]) all take.
fn is_preflight(method: &::http::Method, headers: &::http::HeaderMap) -> bool {
    method == ::http::Method::OPTIONS && headers.contains_key(REQUEST_METHOD_FIELD)
}

/// Whether `origin` is a page served from THIS MACHINE: an origin whose host is a loopback
/// one, by the core's own reading of that question (`emulator_server::is_loopback_host`, which
/// is also what the `Host` allowlist is decided by).
///
/// Such a page is the browser-based client of the emulator — a development server on a
/// loopback port, the console opened against it — and is who this endpoint's CORS answer is
/// for. A page of any OTHER site is not, and is refused before anything of the API is reached
/// ([`refused_before_dispatch`]).
fn served_from_this_machine(origin: &str) -> bool {
    let authority = origin
        .split_once("://")
        .map_or(origin, |(_scheme, authority)| authority);
    ::emulator_server::is_loopback_host(
        ::service_common::data_plane_addressing::host_only(authority),
    )
}

/// Answer one request to the GraphQL endpoint.
async fn answer(
    context: PlaneContext,
    in_flight: Arc<Semaphore>,
    request: ::http::Request<::hyper::body::Incoming>,
    request_id: String,
) -> ::http::Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if let Some(refused) = refused_before_dispatch(&parts, &context.remote_origins) {
        return refused;
    }
    // One permit per request IN FLIGHT, taken before a byte of the body is read: a request
    // that finds none is shed, which is what the core's own HTTP listener answers such a
    // request with.
    let Ok(permit) = Arc::clone(&in_flight).try_acquire_owned() else {
        return error_response(503, "ServiceUnavailableException", SERVER_BUSY);
    };
    let limited = Limited::new(body, body_ceiling());
    let served = ::tokio::time::timeout(request_deadline(), async move {
        // Held for the whole read and the execution, and released with them.
        let _permit: OwnedSemaphorePermit = permit;
        let Ok(collected) = limited.collect().await else {
            return error_response(413, BAD_REQUEST_ERROR_TYPE, BODY_TOO_LARGE);
        };
        served_request(&context, &parts, &collected.to_bytes(), &request_id).await
    })
    .await;
    served.unwrap_or_else(|_| error_response(408, BAD_REQUEST_ERROR_TYPE, REQUEST_TIMED_OUT))
}

/// Answer one framed request: the GraphQL route, and the refusals every other request to
/// this endpoint meets.
async fn served_request(
    context: &PlaneContext,
    parts: &::http::request::Parts,
    body: &[u8],
    request_id: &str,
) -> ::http::Response<Full<Bytes>> {
    // When this request began and the clock its latency is measured on: both are what the
    // API's own records of it state ([`field_logs`]).
    let started = SystemTime::now();
    let clock = Instant::now();
    if !serves_path(parts.uri.path()) {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            404,
            NOT_FOUND_ERROR_TYPE,
            NO_SUCH_ROUTE,
        )
        .await;
    }
    // The CORS PREFLIGHT a browser makes before the real request, which this endpoint
    // answers itself: nothing of the API is reached by it, so it presents no authorization
    // material, carries no document and is not a request the API executed
    // ([`preflight_response`] states the rule in full). It is also the ONLY `OPTIONS` this
    // endpoint routes, so one that asks about no method at all addresses nothing it serves.
    if is_preflight(&parts.method, &parts.headers) {
        return preflight_response(parts);
    }
    if parts.method == ::http::Method::OPTIONS {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            404,
            UNKNOWN_OPERATION_ERROR_TYPE,
            NO_SUCH_ROUTE,
        )
        .await;
    }
    if parts.method != ::http::Method::POST {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            405,
            BAD_REQUEST_ERROR_TYPE,
            A_QUERY_IS_A_POST,
        )
        .await;
    }
    // The API's own ARN, which is what a web ACL is associated with and what says the API
    // still exists at all.
    let Some(arn) = context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .map(|api| api.arn.clone())
    else {
        return error_response(
            404,
            NOT_FOUND_ERROR_TYPE,
            crate::state::API_NOT_FOUND_MESSAGE,
        );
    };
    if let Some(refused) = refused_by_web_acl(context, &arn, parts, body) {
        return refused;
    }
    let auth = PresentedAuth::from_fields(&parts.headers);
    let authorized = {
        let store = context.state.lock();
        super::authorized(&store, &context.api_id, &auth)
    };
    if !authorized {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            401,
            event_api::UNAUTHORIZED_ERROR_TYPE,
            event_api::UNAUTHORIZED_MESSAGE,
        )
        .await;
    }
    // WHO the call speaks for, resolved once: it is both what a resolver reads as
    // `$context.identity` and what the API's fields are authorized for ([`caller`]). A call
    // that presents no SigV4 material names no principal, which is every call made under
    // another of the API's authorization modes.
    let signed = caller::presented_caller(&context.engine, &parts.headers);
    let mut identity = signed
        .as_ref()
        .map_or(Json::Null, |caller| caller.identity.clone());
    // What the API's `AMAZON_COGNITO_USER_POOLS` mode decides about this call, which is taken
    // BEFORE any field of it resolves: the token the request presents is verified against the
    // user pool the API configured, and what that token CLAIMS is who the call speaks for
    // ([`user_pool`]). An API that configured no user pool decides nothing here. The groups an
    // admitted token claims are kept, because the API's schema may guard its fields by them.
    let mut user_pool_groups: Option<Vec<String>> = None;
    match user_pool::decided(
        context,
        presented_material(&parts.headers),
        caller::source_ip(&parts.headers),
        started,
    ) {
        user_pool::Decided::NotApplicable => {}
        user_pool::Decided::Refused(refused) => {
            ::tracing::info!(
                api_id = %context.api_id,
                reason = %refused.reason,
                "the token this call presented authorizes no call on this API"
            );
            return refusal_logged(
                context,
                request_id,
                started,
                clock,
                UNAUTHORIZED_STATUS,
                event_api::UNAUTHORIZED_ERROR_TYPE,
                refused.message,
            )
            .await;
        }
        user_pool::Decided::Admitted(admitted) => {
            identity = admitted.identity;
            user_pool_groups = Some(admitted.groups);
        }
    }
    let Ok(document) = ::serde_json::from_slice::<Json>(body) else {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            400,
            BAD_REQUEST_ERROR_TYPE,
            BODY_IS_A_DOCUMENT,
        )
        .await;
    };
    let Some(query) = document.get(QUERY_MEMBER).and_then(Json::as_str) else {
        return refusal_logged(
            context,
            request_id,
            started,
            clock,
            400,
            BAD_REQUEST_ERROR_TYPE,
            BODY_CARRIES_A_QUERY,
        )
        .await;
    };
    let request = GraphqlRequest {
        query: query.to_string(),
        operation_name: document
            .get(OPERATION_NAME_MEMBER)
            .and_then(Json::as_str)
            .map(str::to_string),
        variables: document
            .get(VARIABLES_MEMBER)
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default(),
        headers: request_fields(&parts.headers),
        identity,
        // The principal the call signed as, carried into the execution because it is also
        // what admits a call a resolver's data source makes under the CALLER's own
        // credentials — the same answer the API's own fields are judged against.
        caller: signed.clone(),
        unauthorized: Vec::new(),
        // The groups an admitted user pool token claims, which the schema's own group guards
        // are judged against as each field resolves ([`user_pool::refused_fields`]).
        user_pool_groups: user_pool_groups.clone(),
        // Filled in below, once every mode that decides a call has had its say: an API that
        // set up more than one mode says per field which of them may reach it
        // ([`field_auth`]).
        auth_mode: None,
    };
    let mut request = request;
    // What the API's `AWS_LAMBDA` authorization mode decides about this call, which is taken
    // BEFORE any field of it resolves: the authorizer is invoked with the token presented and
    // what it returned admits the call, withholds fields of it, or refuses it outright
    // ([`authorizer`]). An API that configured no Lambda authorizer decides nothing here.
    let presented = authorizer::Presented {
        token: presented_material(&parts.headers),
        request_id,
        query: &request.query,
        operation_name: request.operation_name.as_deref(),
        variables: &request.variables,
    };
    let mut lambda_admitted = false;
    match authorizer::decided(context, &presented).await {
        authorizer::Decided::NotApplicable => {}
        authorizer::Decided::Refused => {
            ::tracing::info!(
                api_id = %context.api_id,
                "this API's Lambda authorizer did not authorize this call"
            );
            return refusal_logged(
                context,
                request_id,
                started,
                clock,
                UNAUTHORIZED_STATUS,
                event_api::UNAUTHORIZED_ERROR_TYPE,
                event_api::UNAUTHORIZED_MESSAGE,
            )
            .await;
        }
        authorizer::Decided::Admitted { identity, denied } => {
            // What the authorizer published about the caller IS this call's identity, and the
            // fields it withheld resolve to nothing.
            request.identity = identity;
            request.unauthorized.extend(denied);
            lambda_admitted = true;
        }
    }
    // WHICH of the API's authorization modes this call is of, resolved once now that every
    // mode that decides a call has had its say: it is what the schema's own `@aws_*`
    // directives are judged against as each field resolves ([`field_auth`]).
    request.auth_mode = context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .and_then(|api| {
            field_auth::resolved_mode(
                api,
                &field_auth::Presented {
                    api_key: auth.api_key.as_deref(),
                    authorization: auth.authorization.as_deref(),
                    signed: signed.is_some(),
                    user_pool_admitted: user_pool_groups.is_some(),
                    lambda_admitted,
                },
            )
        });
    // What IAM says about the fields this call selects. A call NONE of whose fields it
    // permits is refused outright; one it permits some of is served, with the rest reported
    // as fields the caller may not access.
    let judged = judged_fields(context, &arn, signed.as_ref(), &request);
    if judged.refuses_the_call() {
        ::tracing::info!(
            api_id = %context.api_id,
            "this caller's IAM policies permit no field this call selects"
        );
        return error_response(401, event_api::UNAUTHORIZED_ERROR_TYPE, PERMISSION_DENIED);
    }
    // Beside the fields the authorizer withheld, where there was one.
    request.unauthorized.extend(judged.refused);
    let answered = execution::execute(context, &request).await;
    field_logs::delivered(
        context,
        &field_logs::Served {
            request_id,
            status: 200,
            started,
            latency: clock.elapsed(),
            answer: Some(&answered),
        },
    )
    .await;
    json_response(200, &answered.document())
}

/// One refusal the API's own logs report: the answer, and the record the API logs about
/// having refused the request.
///
/// A request the API itself refused is one it SERVED — the status it was answered with is
/// what a request-level record states — so it is logged like any other, with no execution to
/// report because none ran.
async fn refusal_logged(
    context: &PlaneContext,
    request_id: &str,
    started: SystemTime,
    clock: Instant,
    status: u16,
    error_type: &str,
    message: &str,
) -> ::http::Response<Full<Bytes>> {
    field_logs::delivered(
        context,
        &field_logs::Served {
            request_id,
            status,
            started,
            latency: clock.elapsed(),
            answer: None,
        },
    )
    .await;
    error_response(status, error_type, message)
}

/// What IAM decided about the FIELDS one call selects.
struct Judged {
    /// The `(typeName, fieldName)` pairs the caller's policies do not permit.
    refused: Vec<(String, String)>,
    /// Whether any field the call selects IS permitted.
    any_permitted: bool,
}

impl Judged {
    /// Whether the whole call is refused: the caller may reach no field of it at all, which
    /// is the request AppSync answers `401` rather than serving a result with no data in it.
    fn refuses_the_call(&self) -> bool {
        !self.any_permitted && !self.refused.is_empty()
    }
}

/// Judge each field this call selects for `appsync:GraphQL`.
///
/// AppSync's `AWS_IAM` mode is the IAM layer in front of the API: "you can use IAM policies
/// to restrict access to … individual fields", so each field the call selects of the
/// operation's root type is judged on that field's own ARN ([`caller::field_resource`])
/// BEFORE any resolver runs. A field no statement allows, and one an explicit `Deny` covers,
/// is a field the caller may not access: it resolves to nothing and is reported as an error
/// of its own path, and no resolver of it runs. A call that may reach NO field it selects is
/// refused outright.
///
/// Judged only for a call the mode applies to: the API has to have configured `AWS_IAM` (as
/// its `authenticationType` or as one of its additional providers), and the call has to
/// present the SigV4 material that mode decides. Everything else permits every field, as
/// every other authorization mode decides a call as a whole.
fn judged_fields(
    context: &PlaneContext,
    arn: &str,
    signed: Option<&caller::SignedCaller>,
    request: &GraphqlRequest,
) -> Judged {
    let permitted = Judged {
        refused: Vec::new(),
        any_permitted: true,
    };
    let Some(signed) = signed else {
        return permitted;
    };
    if !context
        .state
        .lock()
        .graphql_apis
        .get(&context.api_id)
        .is_some_and(|api| super::configures(api, caller::IAM_AUTH_TYPE))
    {
        return permitted;
    }
    let selected = execution::selected_root_fields(context, request);
    let mut judged = Judged {
        refused: Vec::new(),
        any_permitted: false,
    };
    for (type_name, field_name) in selected {
        if caller::permits(
            signed,
            &caller::field_resource(arn, &type_name, &field_name),
        ) {
            judged.any_permitted = true;
            continue;
        }
        ::tracing::info!(
            api_id = %context.api_id,
            %type_name,
            %field_name,
            "this caller's IAM policies do not let it perform appsync:GraphQL on the field"
        );
        judged.refused.push((type_name, field_name));
    }
    judged
}

/// The material a request presents for an authorization mode to decide: the value of the
/// request's own `authorization` field, which is where AppSync reads a Lambda authorizer's
/// input from — and also where a SIGNED call presents its signature, which the mode that reads
/// one decides instead ([`authorizer::decided`]).
fn presented_material(headers: &::http::HeaderMap) -> &str {
    headers
        .get(::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The request's own fields as a resolver reads them (`ctx.request.headers`): one member per
/// field, under the lowercase name HTTP gives it.
fn request_fields(headers: &::http::HeaderMap) -> JsonMap<String, Json> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), Json::String(value.to_string())))
        })
        .collect()
}

/// What the web ACL in front of this API decided about this request, or `None` when it let
/// the request through to the API — which is also what a request with no web ACL in front of
/// it meets.
///
/// The decision is WAFv2's own: this hands it the request as that service inspects one
/// ([`waf_edge::InspectedRequest`]) and renders the answer it gets back. The two stores are
/// locked one after the other and never at the same time, and the wafv2 side locks nothing
/// of this service's, so the two cannot deadlock.
fn refused_by_web_acl(
    context: &PlaneContext,
    arn: &str,
    parts: &::http::request::Parts,
    body: &[u8],
) -> Option<::http::Response<Full<Bytes>>> {
    let decision = waf_edge::decide(&*context.engine, arn, &inspected_request(parts, body))?;
    if decision.action.is_allow() {
        return None;
    }
    ::tracing::info!(
        api_id = %context.api_id,
        rule = decision.rule_name.as_deref().unwrap_or("<default action>"),
        "the web acl in front of this API does not let the request through"
    );
    // A terminating action that configures its own `CustomResponse` is served BY WAF: the
    // status, the headers and the body are the web ACL's own, and this endpoint's envelope
    // is not used, because AppSync never produced the response.
    if let Some(custom) = decision.custom_response {
        return Some(edge_response(custom));
    }
    // A `Block` with no custom response is the refusal AppSync's own edge words. A `Captcha`
    // or `Challenge` with none is answered with the status WAF serves its interstitial
    // under; the interstitial ITSELF is a script WAF serves from its own endpoint, which
    // this emulator serves none of, so the answer carries the status and the same envelope
    // rather than a page nothing produced.
    Some(error_response(
        decision.action.default_status().unwrap_or(FORBIDDEN_STATUS),
        WAF_FORBIDDEN_ERROR_TYPE,
        WAF_FORBIDDEN_MESSAGE,
    ))
}

/// The request as WAFv2 inspects it: every component a `FieldToMatch` can name.
///
/// Read by WAFv2's own reading of an inbound request ([`waf_edge::InspectedRequest::of_parts`])
/// — including the address it came from — so this endpoint and every other protected
/// resource's own service cannot differ on what a `FieldToMatch` names. The path and the
/// query string are the ones the request arrived with, because this socket IS the API's
/// endpoint: a client addressing AWS sends the same `/graphql` to the API's own hostname, so
/// there is no emulator addressing prefix to strip.
fn inspected_request(parts: &::http::request::Parts, body: &[u8]) -> waf_edge::InspectedRequest {
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect();
    waf_edge::InspectedRequest::of_parts(
        parts.method.as_str(),
        parts.uri.path(),
        parts.uri.query().unwrap_or_default(),
        headers,
        body.to_vec(),
    )
}

/// The response a web ACL's own `CustomResponse` serves, as it leaves the edge.
fn edge_response(custom: waf_edge::EdgeResponse) -> ::http::Response<Full<Bytes>> {
    let body = Bytes::from(custom.body);
    let mut response = ::http::Response::new(Full::new(body.clone()));
    *response.status_mut() = ::http::StatusCode::from_u16(custom.status)
        .unwrap_or(::http::StatusCode::FORBIDDEN);
    let mut fields: Vec<(String, String)> = Vec::new();
    if let Some(content_type) = custom.content_type {
        fields.push((::http::header::CONTENT_TYPE.to_string(), content_type));
    }
    fields.extend(custom.headers);
    // The length is written LAST, so a configured header cannot leave the answer stating a
    // length that is not the body's.
    fields.push((
        ::http::header::CONTENT_LENGTH.to_string(),
        body.len().to_string(),
    ));
    for (name, value) in fields {
        match (
            ::http::HeaderName::from_bytes(name.as_bytes()),
            ::http::HeaderValue::from_str(&value),
        ) {
            (Ok(name), Ok(value)) => {
                response.headers_mut().insert(name, value);
            }
            _ => ::tracing::warn!(
                %name,
                "the web acl's custom response names a header a response cannot carry, and \
                 it is left off the answer"
            ),
        }
    }
    response
}

/// How long one request on this endpoint may take, from its first header byte to the
/// answer: the core's own HTTP request deadline.
fn request_deadline() -> Duration {
    Duration::from_secs(listener_caps::HTTP_REQUEST_DEADLINE_SECS)
}

/// How many bytes of one request this endpoint reads: the bound this service states for a
/// message on a data-plane endpoint (`crate::service::event_api::MAX_MESSAGE_BYTES`), which
/// is well under the core's own per-connection ceiling.
fn body_ceiling() -> usize {
    event_api::MAX_MESSAGE_BYTES
}

/// The transport-safety refusals the core applies to its own HTTP listeners, applied here
/// from the core's own predicates so this endpoint and a core listener decide them alike.
///
/// The BROWSER (`Origin` / `Sec-Fetch-Site`) check among them is applied to the REAL request
/// only, and admits one more client than a core listener does: a page served from THIS
/// MACHINE ([`served_from_this_machine`]) is the browser-based client an AppSync API's
/// `GRAPHQL` endpoint exists to be called from, so it reaches the API and is then decided by
/// the material it presents like any other call. A page of ANY OTHER site is refused here,
/// before anything of the API is reached, exactly as the core refuses one on its own
/// listeners — what guards the developer's machine against a site they happen to be visiting
/// driving this socket — and the CORS PREFLIGHT is answered before this check runs, because a
/// preflight presents nothing and reaches nothing ([`preflight_response`]).
fn refused_before_dispatch(
    parts: &::http::request::Parts,
    remote_origins: &::emulator_server::RemoteOrigins,
) -> Option<::http::Response<Full<Bytes>>> {
    let header = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());
    // HTTP/1.1 requires a `Host`, and without an authority the rebinding guard cannot be
    // run at all, so a request carrying none is refused rather than served: this fails
    // CLOSED, exactly as the core's own HTTP listener does.
    let Some(host) = header("host").or_else(|| parts.uri.host()) else {
        return Some(error_response(
            421,
            BAD_REQUEST_ERROR_TYPE,
            event_api::HOST_REQUIRED,
        ));
    };
    if !event_api::host_allowed(host) {
        return Some(error_response(
            421,
            BAD_REQUEST_ERROR_TYPE,
            event_api::HOST_NOT_ALLOWED,
        ));
    }
    if ::emulator_server::is_websocket_upgrade(
        header("connection"),
        header("upgrade"),
        header(":protocol"),
    ) {
        return Some(error_response(400, BAD_REQUEST_ERROR_TYPE, NO_WEBSOCKETS));
    }
    // The preflight is answered before the browser check, so a page learns what it may send
    // whatever site it was served from; what it may then DRIVE is this check's decision.
    if is_preflight(&parts.method, &parts.headers) {
        return None;
    }
    let origin = header("origin");
    if !origin.is_some_and(|origin| admitted_origin(origin, remote_origins)) {
        // The decision is the CORE's own reading of the browser-set fields
        // (`emulator_server::classify_browser_request`), driven with the origins the operator
        // asked for, so this endpoint and a core listener cannot differ on what another site
        // is. What it answers is this endpoint's OWN policy, in its own words.
        if let ::emulator_server::BrowserVerdict::Forbid(reason) =
            ::emulator_server::classify_browser_request(
                origin,
                header("sec-fetch-site"),
                remote_origins,
                true,
            )
        {
            ::tracing::info!(
                %reason,
                "refusing a request a browser made to a GraphQL API's endpoint from another site"
            );
            return Some(error_response(
                403,
                event_api::UNAUTHORIZED_ERROR_TYPE,
                &page_of_another_site(),
            ));
        }
    }
    None
}

/// What this endpoint answers a request a BROWSER made from another site: its own policy,
/// stated in its own words and naming the setting that admits the site, rather than a
/// sentence AppSync words for something else.
fn page_of_another_site() -> String {
    format!(
        "This endpoint serves a page served from this machine. Name another site in \
         {REMOTE_ORIGINS_ENV} to serve a page from it."
    )
}

/// Whether a page served from `origin` may DRIVE this endpoint: one served from this machine
/// ([`served_from_this_machine`]), and one the operator asked for ([`REMOTE_ORIGINS_ENV`]) —
/// which is the core's own allowlist reading of an origin (`RemoteOrigins::allows`, through
/// the predicate every core HTTP listener is guarded by), asked about the origin alone.
fn admitted_origin(origin: &str, remote_origins: &::emulator_server::RemoteOrigins) -> bool {
    served_from_this_machine(origin)
        || matches!(
            ::emulator_server::classify_browser_request(Some(origin), None, remote_origins, true),
            ::emulator_server::BrowserVerdict::Allow
        )
}

/// The setting an operator widens this endpoint's cross-origin policy with: the origins,
/// comma-separated, a page may DRIVE the API's data plane from besides a page served from this
/// machine.
///
/// Unset — the default — admits none of them, so no site the developer happens to be visiting
/// can reach an API of this emulator; setting it is announced ([`configured_remote_origins`]).
pub const REMOTE_ORIGINS_ENV: &str = "APPSYNC_GRAPHQL_REMOTE_ORIGINS";

/// The origins [`REMOTE_ORIGINS_ENV`] asks this endpoint to serve a page from, as the CORE's
/// own policy for that question: read ONCE when an API's sockets are opened
/// (`super::open_for_api`) and never in the request path, and ANNOUNCED where it asks for any,
/// because what it widens is who can drive and read the API's data plane.
#[must_use]
pub fn configured_remote_origins() -> ::emulator_server::RemoteOrigins {
    let asked: Vec<String> = ::std::env::var(REMOTE_ORIGINS_ENV)
        .unwrap_or_default()
        .split(',')
        .map(|origin| origin.trim().to_string())
        .filter(|origin| !origin.is_empty())
        .collect();
    if asked.is_empty() {
        return ::emulator_server::RemoteOrigins::SameOriginOnly;
    }
    ::tracing::warn!(
        origins = %asked.join(","),
        setting = %REMOTE_ORIGINS_ENV,
        "a page served from these sites may drive and read every GraphQL API of this \
         emulator; unset this to admit only a page served from this machine"
    );
    ::emulator_server::RemoteOrigins::Exact(asked)
}

/// The answer to one CORS PREFLIGHT on this endpoint — the request [`is_preflight`] admits,
/// which is the only `OPTIONS` this endpoint routes.
///
/// A browser asks, with `OPTIONS` and `Access-Control-Request-Method`, whether the request it
/// is about to make may be made (Fetch, §4.8: the "CORS-preflight request"). An AppSync API's
/// `GRAPHQL` endpoint answers that question itself, before anything that decides the real
/// request: the preflight carries no authorization material — a browser sends none on it —
/// and no document, so it is neither authorized nor executed, and nothing of the API is
/// reached. What it answers is what the real request is permitted to carry: the METHOD and
/// the FIELDS the browser asked about, which this endpoint places no restriction of its own
/// on ([`EVERY_FIELD`] where it asked about none), and how long the answer may be reused. What
/// a page may then READ of an answer is stated on every answer of this endpoint
/// ([`cross_origin_permission`]), this one included.
fn preflight_response(parts: &::http::request::Parts) -> ::http::Response<Full<Bytes>> {
    let header = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());
    // No BODY: the answer is the permission itself, and a preflight's answer carries no
    // payload for a browser to read.
    let mut answered = ::http::Response::builder()
        .status(200)
        .header(::http::header::CONTENT_LENGTH, "0")
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| ::http::Response::new(Full::new(Bytes::new())));
    let allowed = [
        (
            ALLOW_METHODS_FIELD,
            header(REQUEST_METHOD_FIELD).unwrap_or_default().to_string(),
        ),
        (
            ALLOW_HEADERS_FIELD,
            header(REQUEST_HEADERS_FIELD)
                .unwrap_or(EVERY_FIELD)
                .to_string(),
        ),
        (MAX_AGE_FIELD, PREFLIGHT_MAX_AGE_SECONDS.to_string()),
    ];
    for (name, value) in allowed {
        if let Ok(value) = ::http::HeaderValue::from_str(&value) {
            answered
                .headers_mut()
                .insert(::http::HeaderName::from_static(name), value);
        }
    }
    answered
}

/// The fields a browser asks a preflight with, and the ones the answer states the permission
/// in (Fetch, §3.2.2 and §3.2.3).
const REQUEST_METHOD_FIELD: &str = "access-control-request-method";
const REQUEST_HEADERS_FIELD: &str = "access-control-request-headers";
const ALLOW_ORIGIN_FIELD: &str = "access-control-allow-origin";
const ALLOW_METHODS_FIELD: &str = "access-control-allow-methods";
const ALLOW_HEADERS_FIELD: &str = "access-control-allow-headers";
const EXPOSE_HEADERS_FIELD: &str = "access-control-expose-headers";
const MAX_AGE_FIELD: &str = "access-control-max-age";

/// The field an answer names the request fields its own value DEPENDS on in (RFC 9110 §12.5.5),
/// which the permitted origin does.
const VARY_FIELD: &str = "vary";

/// What a preflight's answer permits when the browser asked about no FIELD in particular:
/// every field, as Fetch §3.2.3 spells that — a list of field names, which is why it is not
/// the permitted ORIGIN's own spelling of "any" ([`ALLOWED_ORIGIN`]) even though both are
/// written the same.
const EVERY_FIELD: &str = "*";

/// The response fields a page may read off an answer of this endpoint: the request's own id
/// ([`REQUEST_ID_FIELD`]) and the class of a refusal ([`ERROR_TYPE_FIELD`]), which are the two
/// this endpoint writes about the answer itself.
const EXPOSED_FIELDS: &str = "x-amzn-requestid,x-amzn-errortype";

/// How long a browser may reuse this endpoint's preflight answer without asking again — the
/// two days AppSync's own preflight states, which is the maximum a browser caches one for
/// (`Access-Control-Max-Age`, Fetch §3.2.3: "the number of seconds the information provided
/// … can be cached"). A value of the ENDPOINT rather than of any request, so it is the same
/// on every preflight it answers.
const PREFLIGHT_MAX_AGE_SECONDS: u32 = 172_800;

/// The origin this endpoint permits where it names no single one: ANY, which is what an
/// answer carrying no API state states — the preflight, the refusal a page of another site
/// earns, and the answer to a client that is no page at all. An answer to a page this
/// endpoint does serve names THAT page's origin instead ([`cross_origin_permission`]).
const ALLOWED_ORIGIN: &str = "*";

/// One answer carrying a document.
fn json_response(status: u16, document: &Json) -> ::http::Response<Full<Bytes>> {
    let body = Bytes::from(document.to_string());
    ::http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
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

/// One refusal, in the envelope AppSync answers a data-plane error in: a list of errors,
/// each naming its type and stating its sentence — and no `data`, because nothing of the
/// API was reached.
fn error_response(
    status: u16,
    error_type: &str,
    message: &str,
) -> ::http::Response<Full<Bytes>> {
    let mut response = json_response(
        status,
        &event_api::publish::errors_document(error_type, message),
    );
    // AppSync names the type of a data-plane error in a field of the answer as well as in
    // the error itself, which is where a client that reads neither body nor status looks.
    if let Ok(value) = ::http::HeaderValue::from_str(error_type) {
        response
            .headers_mut()
            .insert(::http::HeaderName::from_static(ERROR_TYPE_FIELD), value);
    }
    response
}

/// The field AppSync names the class of a data-plane error in.
const ERROR_TYPE_FIELD: &str = "x-amzn-errortype";

/// The members a GraphQL request carries.
const QUERY_MEMBER: &str = "query";
const VARIABLES_MEMBER: &str = "variables";
const OPERATION_NAME_MEMBER: &str = "operationName";

/// The `errorType` values this endpoint's refusals carry, as AppSync's own errors name them.
const BAD_REQUEST_ERROR_TYPE: &str = "BadRequestException";
const NOT_FOUND_ERROR_TYPE: &str = "NotFoundException";

/// The `errorType` AppSync names a request that reaches no operation of the API with: an
/// `OPTIONS` that is not the CORS preflight, which is the only `OPTIONS` this endpoint routes.
const UNKNOWN_OPERATION_ERROR_TYPE: &str = "UnknownOperationException";

/// The sentences this endpoint's own refusals state.
const NO_SUCH_ROUTE: &str = "This endpoint serves POST /graphql.";
const A_QUERY_IS_A_POST: &str = "A GraphQL request is made with POST.";
const BODY_IS_A_DOCUMENT: &str = "The request body must be a JSON document.";
const BODY_CARRIES_A_QUERY: &str = "The request must carry a GraphQL document in query.";
const BODY_TOO_LARGE: &str = "This request is larger than this endpoint accepts.";
const REQUEST_TIMED_OUT: &str = "This request took longer than this endpoint allows.";
const SERVER_BUSY: &str = "This endpoint is serving as many requests as it can. Try again.";
const NO_WEBSOCKETS: &str =
    "A WebSocket connection is served at the API's real-time endpoint.";

/// The sentence AppSync states when the caller's IAM policies do not permit the call: the
/// refusal of the `AWS_IAM` authorization mode, which names neither the field it refused nor
/// the policy that refused it.
pub const PERMISSION_DENIED: &str = "Permission denied";

/// The status a call this API's Lambda authorizer did not authorize is answered with: the
/// request reached no field of the API, so it is refused rather than served — the status every
/// unauthorized call on a GraphQL API's data plane is answered with.
const UNAUTHORIZED_STATUS: u16 = 401;

#[cfg(test)]
mod tests {
    use super::*;

    /// Which ORIGINS a page may drive this endpoint from, over the forms an origin is written
    /// in and the two sources of admission: a page served from this machine, which is admitted
    /// with nothing configured, and one the operator asked for.
    #[test]
    fn a_page_drives_the_endpoint_from_this_machine_and_from_the_origins_asked_for() {
        let nothing_asked = ::emulator_server::RemoteOrigins::SameOriginOnly;
        let asked = ::emulator_server::RemoteOrigins::Exact(vec![
            "https://studio.apollographql.com".to_string(),
            "https://console.example.aws".to_string(),
        ]);
        // (origin, admitted with nothing configured, admitted with `asked` configured)
        let rows: &[(&str, bool, bool)] = &[
            ("http://localhost:5173", true, true),
            ("http://127.0.0.1:3000", true, true),
            ("https://localhost", true, true),
            ("http://[::1]:8080", true, true),
            ("http://127.0.0.2", true, true),
            ("https://studio.apollographql.com", false, true),
            ("https://console.example.aws", false, true),
            ("https://attacker.example", false, false),
            // An exact list admits exactly what it names: neither a prefix of an origin it
            // names nor a name merely ending in one.
            ("https://studio.apollographql.com.attacker.example", false, false),
            ("https://notlocalhost", false, false),
            // The opaque origin of a sandboxed frame names no site at all.
            ("null", false, false),
        ];
        for (origin, alone, configured) in rows {
            assert_eq!(
                admitted_origin(origin, &nothing_asked),
                *alone,
                "case {origin:?}: with nothing configured, expected admitted={alone}"
            );
            assert_eq!(
                admitted_origin(origin, &asked),
                *configured,
                "case {origin:?}: with {asked:?} configured, expected admitted={configured}"
            );
        }
    }

    /// The fields an answer says a page may read are the ones this endpoint WRITES about an
    /// answer, so renaming either cannot silently stop exposing it.
    #[test]
    fn every_field_this_endpoint_writes_about_an_answer_is_one_a_page_may_read() {
        let exposed: Vec<&str> = EXPOSED_FIELDS.split(',').map(str::trim).collect();
        assert_eq!(
            exposed,
            vec![REQUEST_ID_FIELD, ERROR_TYPE_FIELD],
            "the exposed fields must be exactly the two this endpoint writes, got {exposed:?}"
        );
    }
}
