// SPDX-License-Identifier: CC0-1.0

//! The ENVIRONMENT data plane: how a request a client sends to the address an
//! environment publishes is recognised, and how the application the environment
//! runs answers it.
//!
//! An environment publishes the address its traffic goes to —
//! `EnvironmentDescription$CNAME`, "the URL to the CNAME for this environment",
//! and the `EndpointURL` beside it — and a client READS that address and dials
//! it. Both are HOSTS this emulator announces in a zone of its own
//! ([`crate::state::published_addresses`]): the CNAME is the name the
//! environment's prefix forms in its Region, and the `EndpointURL` the host of
//! the load balancer in front of it or the address its instances answer at. A
//! client that resolves such a name to this emulator is served by the leading
//! labels of the host it named ([`crate::state::published_host`]), and a client
//! that resolves nothing but the emulator's own endpoint carries in the PATH what
//! it cannot carry in the host: the plane path naming the Region and the prefix
//! ([`crate::state::environment_plane_path`]) or the environment's own id
//! ([`crate::state::environment_own_plane_path`]).
//!
//! Neither addressing names a modelled operation of this service: the front door
//! could resolve no operation for such a request and answered a routing failure,
//! for the one address this service told the caller to use. So this plane claims
//! it ([`claims_parts`], read by `Service::can_handle`) and serves it
//! ([`EnvironmentPlane`], returned by `Service::override_operation`).
//!
//! What it answers is what the DEPLOYED APPLICATION answers: the version the
//! environment reports running is RUN, in process, and the response its own
//! request handler ends is the response — or, for a bundle that starts no server
//! of its own, the file of that bundle the request addresses
//! ([`crate::state::DeployedApplication`]). WHICH environment answers follows
//! from HOW the request was addressed ([`Addressing`]): a NAME is answered by the
//! environment carrying it now, and an environment's own address by that
//! environment. Nothing is invented: an environment running no application this
//! emulator could start answers as an address with nothing behind it.
//!
//! Serving traffic moves no status, consumes no quota and bumps no deployment, so
//! a client dialling the address leaves every later caller reading the same
//! environment. What it does leave behind is what the APPLICATION did: an
//! application's own output goes to the instance's standard-output log file on a
//! real instance, so an environment whose configuration streams its instance logs
//! delivers the lines one request produced into the group that file is streamed to
//! ([`crate::state::stream_instance_logs`]) — and the STATUS the application
//! answered with is what the health agent on the instance observes of it
//! ([`crate::state::served_requests`]), which is what the enhanced-health reports
//! are about. Both are the application's doing, not a reading of this service's
//! state.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::{
    environment_at_own_address, environment_behind_load_balancer,
    environment_serving_cname_prefix, service_state, stream_instance_logs, Environment,
    GuestRequest, GuestResponse, InstanceLogStreaming, PublishedHost,
};
/// What this plane answers for an address no environment serves — the prefix of
/// an environment the store does not hold, or one the clock has terminated
/// ("Terminated: Environment is not running").
const NOT_SERVED_STATUS: u16 = 404;

/// What it answers when the environment IS there but runs no application that
/// answered: the status a proxy answers for a request it has no upstream for,
/// which is what an environment whose application this emulator could not start
/// is.
const NO_APPLICATION_STATUS: u16 = 502;

/// The media type an answer this plane renders itself carries.
const PLAIN_TEXT: &str = "text/plain; charset=utf-8";

/// The headers of the client's own request that describe a request BODY, which is
/// not handed to the application: no body travels to it, so a length or a transfer
/// encoding stating one would describe a body that is not there.
const HEADERS_DESCRIBING_A_BODY: &[&str] = &["content-length", "transfer-encoding"];

/// How a request ADDRESSES an environment, which is what decides WHICH
/// environment answers it.
///
/// The two addressings an environment publishes are not the same question. The
/// `CNAME` is a NAME, and a name is answered by whatever carries it now — which
/// is the point of `SwapEnvironmentCNAMEs`, where the names move and the
/// environments do not. The `EndpointURL` is the ADDRESS the environment itself
/// is served at ("the URL to the LoadBalancer", "the IP address of the
/// instance"), which no swap moves: it goes on reaching its own environment.
/// Which one a request used is in the path it addressed
/// ([`crate::state::addressed_environment_path`]), or in the labels of the host it
/// named ([`crate::state::published_host`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing<'r> {
    /// The NAME the environment publishes: the prefix it carries, in its Region.
    Name {
        region: &'r str,
        cname_prefix: &'r str,
    },
    /// The ADDRESS the environment itself is served at, which names it by its
    /// `EnvironmentId`.
    OwnAddress {
        region: &'r str,
        environment_id: &'r str,
    },
    /// The host of the LOAD BALANCER in front of one environment, which is the
    /// `EndpointURL` of a load-balanced environment: its own address too, since no
    /// swap moves the load balancer an environment sits behind.
    LoadBalancer {
        region: &'r str,
        load_balancer: &'r str,
    },
}

/// What one request addresses: which environment, how it named it, and the
/// request path under that addressing.
#[derive(Debug, Clone, Copy)]
struct Addressed<'r> {
    /// The application's own request path.
    path: &'r str,
    addressing: Addressing<'r>,
}

impl Addressed<'_> {
    /// The Region the address names, which is the Region the environment's own
    /// records (its log groups among them) live in — read off the ADDRESS, since a
    /// client of it presents no credential to read a Region from.
    fn region(&self) -> &str {
        match self.addressing {
            Addressing::Name { region, .. }
            | Addressing::OwnAddress { region, .. }
            | Addressing::LoadBalancer { region, .. } => region,
        }
    }
}

/// Whether `path` and `host` address this plane at all — the predicate the SOCKET
/// an environment's address is served on
/// ([`crate::state::environment_plane_endpoint`]) reads before handing a request
/// to the front door, so that port serves environment traffic and nothing else.
///
/// The same reading the claim uses, minus the store: a request addressed by one of
/// the plane PATHS, or by the leading labels of a resolved name.
pub fn addresses_environment_plane(path: &str, host: Option<&str>) -> bool {
    addressed_by(path, host).is_some()
}

/// What a request addresses, or `None` when it addresses no environment of this
/// plane at all.
///
/// Read from the two addressings the module docs state: the PATH the published
/// address carries (`/_aws/elasticbeanstalk/<region>/<cname prefix>` for the
/// NAME, `/_aws/elasticbeanstalk/<region>/_environment/<environment id>` for the
/// environment's own address, each followed by the application's own request
/// path), and otherwise the leading labels of the HOST the request named
/// ([`crate::state::published_host`]) — the name a prefix forms or the host of an
/// environment's load balancer, for a client that resolves one of them to this
/// emulator — whose whole path is then the application's own.
fn addressed_by<'r>(path: &'r str, host: Option<&'r str>) -> Option<Addressed<'r>> {
    if let Some((addressed, path)) = crate::state::addressed_environment_path(path) {
        return Some(Addressed {
            path,
            addressing: match addressed {
                crate::state::AddressedEnvironment::Name {
                    region,
                    cname_prefix,
                } => Addressing::Name {
                    region,
                    cname_prefix,
                },
                crate::state::AddressedEnvironment::OwnAddress {
                    region,
                    environment_id,
                } => Addressing::OwnAddress {
                    region,
                    environment_id,
                },
            },
        });
    }
    let addressing = match crate::state::published_host(host?)? {
        PublishedHost::Name {
            region,
            cname_prefix,
        } => Addressing::Name {
            region,
            cname_prefix,
        },
        PublishedHost::LoadBalancer {
            region,
            load_balancer,
        } => Addressing::LoadBalancer {
            region,
            load_balancer,
        },
    };
    Some(Addressed { path, addressing })
}

/// The live environment that answers what `addressed` addresses: the one CARRYING
/// that name for a request addressed by name, and the one the address belongs to
/// for a request addressed by its own address. The two differ exactly for a name a
/// `SwapEnvironmentCNAMEs` has moved, which is what makes the swap observable in
/// the traffic.
fn environment_answering<'store>(
    store: &'store crate::state::ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState,
    addressed: &Addressed<'_>,
) -> Option<&'store Environment> {
    match addressed.addressing {
        Addressing::Name {
            region,
            cname_prefix,
        } => environment_serving_cname_prefix(store, region, cname_prefix),
        Addressing::OwnAddress {
            region,
            environment_id,
        } => environment_at_own_address(store, region, environment_id),
        Addressing::LoadBalancer {
            region,
            load_balancer,
        } => environment_behind_load_balancer(store, region, load_balancer),
    }
}

/// Whether this plane serves the request `parts` addresses: its path addresses a
/// CNAME prefix some live environment of this store carries.
///
/// The claim is narrowed to an environment this service HOLDS, read from the one
/// shared store, rather than to a path shape alone, so one request keeps one
/// claimant.
pub fn claims_parts<Eng: Send + Sync + 'static>(engine: &Eng, parts: &http::request::Parts) -> bool {
    claims(engine, parts.uri.path(), addressed_authority_of_parts(parts))
}

/// The authority a request's PARTS ADDRESSED, which is what the name addressing is
/// read from: the tree's one reading of "WHICH RESOURCE a request addresses"
/// ([`::service_common::data_plane_addressing::addressed_authority_of`]) — the
/// request target's own authority, and the `Host` header for a request that
/// carries none. Not the host a client STATED, which for a relayed request is the
/// host that relay's own client addressed.
fn addressed_authority_of_parts(parts: &http::request::Parts) -> Option<&str> {
    ::service_common::data_plane_addressing::addressed_authority_of(
        parts.uri.authority().map(::http::uri::Authority::as_str),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
}

/// [`claims_parts`] for the runtime-API request the serving seams are handed,
/// read by the same rule, so the claim and the serving cannot disagree about
/// which requests belong to this plane.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    claims(
        engine,
        ::service_common::request_path(req.uri()),
        addressed_authority_of_request(req),
    )
}

/// The authority a runtime-API request addressed, by the same reading.
fn addressed_authority_of_request(req: &RtRequest) -> Option<&str> {
    ::service_common::data_plane_addressing::addressed_authority(
        req.uri(),
        req.headers().get(::http::header::HOST.as_str()),
    )
}

/// The one rule both of those read.
fn claims<Eng: Send + Sync + 'static>(engine: &Eng, path: &str, host: Option<&str>) -> bool {
    let Some(addressed) = addressed_by(path, host) else {
        return false;
    };
    let Some(state) = service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    // A termination the clock has already completed is what makes an address stop
    // being served, and it was due whether or not anybody dialled.
    store.materialize_due_transitions();
    environment_answering(&store, &addressed).is_some()
}

/// The data plane as the ROUTER serves it: the operation
/// `Service::override_operation` hands back for a request addressed to an
/// environment this service holds.
///
/// An environment's own traffic is no modelled operation — no routing key names
/// it — so it is mounted the way an emulated data plane is: claimed by
/// `Service::can_handle` ([`claims_parts`]) and served here. It declares NO auth
/// scheme, because a client of the address an environment publishes carries no
/// AWS credential and has none to sign with.
pub struct EnvironmentPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a
    /// request served here sees exactly what the control plane persisted.
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> EnvironmentPlane<Eng> {
    /// Mount the plane over the shared engine.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Whether this plane serves `req`, by the one rule the claim uses.
    pub fn claims(&self, req: &RtRequest) -> bool {
        claims_request(&*self.engine, req)
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for EnvironmentPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { serve(&*self.engine, req, ctx).await })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // A client that sends traffic to the address an environment published
        // presents no AWS credential: this plane serves the request however it
        // arrived.
        Some(&[])
    }
}

/// Serve one request the environment's own address received.
async fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    let path = ::service_common::request_path(req.uri());
    let Some(addressed) = addressed_by(path, addressed_authority_of_request(req)) else {
        return rendered(NOT_SERVED_STATUS, PLAIN_TEXT, Vec::new());
    };
    // The query string belongs to the request the application reads off
    // `req.url`, the way Node.js hands it over.
    let application_path = match req.uri().split_once('?') {
        Some((_, query)) => format!("{}?{query}", addressed.path),
        None => addressed.path.to_string(),
    };
    let request = GuestRequest {
        method: req.method().to_string(),
        path: application_path,
        // No request BODY travels to the application: nothing of this plane reads
        // one, so the headers that DESCRIBE one are left out too — an application
        // is handed a request whose headers match the request it was handed,
        // rather than one declaring a body that was never delivered.
        headers: req
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .filter(|(name, _)| !HEADERS_DESCRIBING_A_BODY.contains(&name.as_str()))
            .collect(),
    };
    // The deployment is read out of the store and the store released: the
    // application then RUNS with no guard held.
    let (environment_id, deployed, streaming) = {
        let Some(state) = service_state(engine) else {
            return rendered(NO_APPLICATION_STATUS, PLAIN_TEXT, Vec::new());
        };
        let mut store = state.lock();
        store.materialize_due_transitions();
        let Some(environment) = environment_answering(&store, &addressed) else {
            return rendered(NOT_SERVED_STATUS, PLAIN_TEXT, Vec::new());
        };
        let environment_id = environment.environment_id.clone();
        // Where this environment's instance logs go, read while the store is held
        // — the delivery itself calls another service and so happens below, with
        // no guard taken.
        let streaming = store.instance_log_streaming(addressed.region(), &environment_id);
        let deployed = store.deployed_application(&environment_id);
        (environment_id, deployed, streaming)
    };
    let Some(deployed) = deployed else {
        return rendered(NO_APPLICATION_STATUS, PLAIN_TEXT, Vec::new());
    };
    let answered = deployed.serve(&request).await;
    delivered(ctx, streaming.as_ref(), &answered.output).await;
    match answered.response {
        Some(response) => {
            // What the APPLICATION answered is what the health agent on the
            // instance observes of it ([`crate::state::served_requests`]): the
            // status the application set is recorded, so the enhanced-health
            // reports are about the traffic it really served. Nothing of the
            // environment's own state moves here — no status, no quota and no
            // deployment — and an answer this plane rendered itself is none of the
            // application's and is not recorded.
            if let Some(state) = service_state(engine) {
                state
                    .lock()
                    .record_served_answer(&environment_id, response.status);
            }
            served(&response)
        }
        // The application ran and answered nothing at this address, and its
        // bundle carries no file for it either.
        None => rendered(NOT_SERVED_STATUS, PLAIN_TEXT, Vec::new()),
    }
}

/// Deliver what the application wrote while answering one request into the group
/// the instance's standard-output log file is streamed to — for an environment
/// whose configuration streams its instance logs, and where the application wrote
/// anything at all.
///
/// The REGION the groups are held in is the environment's own, which is what the
/// streaming record carries — read off the address the request was sent to, since
/// a client of that address presents no credential to read a Region from.
async fn delivered(
    ctx: &OperationContext<'_>,
    streaming: Option<&InstanceLogStreaming>,
    output: &[String],
) {
    let Some(streaming) = streaming else {
        return;
    };
    if output.is_empty() {
        return;
    }
    stream_instance_logs(ctx, streaming, output).await;
}

/// The response the application ended, as the client reads it: its status, every
/// header it set, and its body under the length it has.
fn served(response: &GuestResponse) -> RtResponse {
    let mut answer = RtResponse::new(status(response.status), SdkBody::from(response.body.clone()));
    for (name, value) in &response.headers {
        // Only a header a client can actually read: a name or a value HTTP
        // cannot carry is left out rather than ending the response.
        if ::http::HeaderName::from_bytes(name.as_bytes()).is_ok()
            && ::http::HeaderValue::from_str(value).is_ok()
        {
            answer.headers_mut().insert(name.clone(), value.clone());
        }
    }
    answer
        .headers_mut()
        .insert("content-length", response.body.len().to_string());
    answer
}

/// One answer this plane renders itself, for an address nothing is served at.
fn rendered(code: u16, media_type: &str, body: Vec<u8>) -> RtResponse {
    let mut answer = RtResponse::new(status(code), SdkBody::from(body));
    answer.headers_mut().insert("content-type", media_type.to_string());
    answer
}

/// A status this plane answers with. Every call site passes a constant or a status
/// the engine already held as a `u16`, so an out-of-range value falls back to the
/// one a proxy with no upstream answers.
fn status(code: u16) -> StatusCode {
    StatusCode::try_from(code)
        .unwrap_or_else(|_| StatusCode::try_from(NO_APPLICATION_STATUS).expect("a valid status"))
}
