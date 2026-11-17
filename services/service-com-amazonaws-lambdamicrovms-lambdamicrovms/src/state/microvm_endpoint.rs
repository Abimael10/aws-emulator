// SPDX-License-Identifier: CC0-1.0

//! The MicroVM endpoint: the ADDRESS a MicroVM is published on, and what this
//! emulator answers there.
//!
//! `RunMicrovmResponse$endpoint` and `GetMicrovmResponse$endpoint` are documented as
//! "The HTTPS endpoint URL for communicating with the MicroVM. Include a valid
//! authentication token in the X-aws-proxy-auth header when sending requests." A
//! published endpoint is therefore an ADDRESS A CLIENT USES, and the recorded value is
//! the AUTHORITY of that address — a host, optionally `host:port`, with no scheme and no
//! path — because a client composes its request URL by prefixing the scheme and
//! appending the guest's own path to it. So this emulator publishes an authority it
//! answers on itself: the core's own loopback HTTPS listener
//! (`engine.listeners().http(..)`, which owns the socket, the bind interface and the
//! TLS), opened PER MICROVM, rather than a hostname that resolves off this machine.
//! AWS gives each MicroVM an address of its own, so each gets its own listener here and
//! the published value carries nothing but that listener's authority: the whole path of
//! a request is the GUEST's, exactly as it is against AWS.
//!
//! What the address answers:
//!
//! * an address whose MicroVM this store no longer holds, or holds in a state nothing
//!   can connect to ([`super::is_connectable_microvm_state`]) — nothing is published
//!   there, so `404`;
//! * no `X-aws-proxy-auth` token, or one this store does not hold live for the MicroVM
//!   the address names (another MicroVM's, one the clock has expired, or one whose
//!   `allowedPorts` do not cover the port THE REQUEST ADDRESSES) — `403`, the answer
//!   `CreateMicrovmAuthToken`'s token is what avoids;
//! * a request a token of `CreateMicrovmAuthToken` granted — the GUEST's own HTTP response
//!   belongs here, so the request is handed to the guest the MicroVM's image version holds
//!   ([`super::guest_program`]) and RUN in the engine embedded in this process
//!   ([`super::guest_runtime`]): the status, the headers and the body a client reads
//!   back are the ones the guest's own program produced. Nothing is synthesised in the
//!   guest's name — a MicroVM whose image holds no guest this engine can run, a MicroVM
//!   whose guest is not running (one still provisioning, and one resting under
//!   `SuspendMicrovm`) and a guest that produced no response are each answered with the
//!   status the PROXY itself answers for a MicroVM it reached nothing inside of
//!   ([`NO_GUEST_RESPONSE_STATUS`]);
//! * a request a token of `CreateMicrovmShellAuthToken` granted — that token grants the
//!   MicroVM's SHELL rather than a port of its endpoint, so the program the request
//!   carries is performed in the MicroVM's own machine and answered with what it wrote
//!   ([`super::microvm_shell`], which states the form of that channel).
//!
//! A guest's outbound connections are decided by the MicroVM's `egressNetworkConnectors`
//! ([`super::microvm_network`]), which this listener answers from the emulated network
//! while the guest waits on them — so what a guest reports of a path it opened is what
//! the account's own connectors, subnets and security groups allow.
//!
//! A token is held as a DIGEST, never as the token itself: the caller keeps the only
//! copy of the secret, and this store keeps just enough to recognise it again.
//!
//! `allowedPorts` is "the ports on the MicroVM endpoint the token grants access to", so
//! what a presented token is judged against is the port THE REQUEST ADDRESSES on that
//! endpoint: the port named in [`PROXY_PORT_HEADER`], and where the request names none,
//! the port the endpoint FRONTS — the port the MicroVM's own guest bound its server on,
//! read from the running guest itself, and [`DEFAULT_PROXIED_PORT`] where it bound none.
//! No metadata of the image the MicroVM runs takes part in that decision: a token scoped
//! to the port its guest listens on is the least privilege the API invites, and it grants
//! exactly the requests addressing that port. The addressed port is then the port of the
//! guest's own server the request is handed to, so a port nothing bound inside the MicroVM
//! reaches nothing there.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    is_connectable_microvm_state, ComAmazonawsLambdamicrovmsLambdamicrovmsState as ServiceState,
    MAX_MAXIMUM_DURATION_IN_SECONDS,
};
use crate::ops::types::PortSpecification;

/// The header a client presents its authentication token in, as
/// `CreateMicrovmAuthTokenResponse$authToken` documents: "Use the value at key
/// \"X-aws-proxy-auth\" as the header value when connecting to the MicroVM endpoint."
pub const AUTH_TOKEN_HEADER: &str = "X-aws-proxy-auth";


/// The header a client names the port it addresses on the MicroVM endpoint in, which is
/// the port a presented token's `allowedPorts` is judged against.
pub const PROXY_PORT_HEADER: &str = "X-aws-proxy-port";

/// The port the MicroVM endpoint FRONTS where neither the request nor the MicroVM's own
/// guest names one: the port a request addresses when it carries no
/// [`PROXY_PORT_HEADER`] and the guest bound no port of its own (one still starting, one
/// that registered no server, or one listening without naming a port). A MicroVM whose
/// guest DID bind a port is fronted on that port, which is what the endpoint reads from
/// the running guest itself.
pub const DEFAULT_PROXIED_PORT: i32 = 8080;

/// The ports a `PortSpecification` can name: `PortNumber` is `@range min 1, max 65535`,
/// so a request naming anything outside it addresses no port at all.
const PORT_RANGE: ::std::ops::RangeInclusive<i32> = 1..=65535;

/// How many live authentication tokens one store keeps.
///
/// `CreateMicrovmAuthToken` mints a token per call and the model puts no ceiling on
/// how often it may be called, so the digests are capped as well as expired: at the
/// cap the token closest to expiry makes room, the same way an idempotency record does
/// ([`ServiceState::record_minted`]).
pub const MAX_LIVE_AUTH_TOKENS: usize = 1_000;

/// How many MicroVM endpoints one store keeps OPEN at a time.
///
/// Each published endpoint is a socket this process holds, and a launch is what opens one,
/// so the ceiling on them is the ceiling on the records they belong to
/// ([`super::MAX_ACTIVE_MICROVMS`]) rather than the number of regions a caller can reach
/// multiplied by it: a launch that would open one past this answers its modelled refusal
/// instead, and an endpoint is closed again as soon as nothing can connect to its MicroVM
/// ([`ServiceState::reap_microvm_endpoints`]).
pub const MAX_PUBLISHED_ENDPOINTS: usize = super::MAX_ACTIVE_MICROVMS;

/// What a published endpoint answers for a path that names no MicroVM of this store.
/// One sentence carrying nothing from the request, so a caller cannot probe with it.
const NO_MICROVM_AT_THIS_ADDRESS: &str = "No MicroVM is published at this address.\n";

/// What it answers a request that carries NO authentication at all: the proxy's own
/// sentence naming what the request is missing, which is what AWS answers such a request
/// with.
const MISSING_AUTHENTICATION: &str = "Request missing authentication";

/// What it answers a request that PRESENTED a token this store does not hold live for the
/// MicroVM the address names, or whose scope does not cover the port the request
/// addresses. One sentence for every way a presented token fails, so the answer does not
/// say which.
const AUTH_TOKEN_REQUIRED: &str = "Present a valid authentication token for this \
                                   MicroVM in the X-aws-proxy-auth header. Create one \
                                   with CreateMicrovmAuthToken.\n";

/// What a published endpoint answers a request it reached nothing inside the MicroVM
/// with: the MicroVM's image holds no guest program this emulator's engine can run, or
/// the guest it ran produced no response at all. The PROXY is answering for a guest that
/// did not, which is the gateway's own status.
pub const NO_GUEST_RESPONSE_STATUS: u16 = 502;

/// What it answers such a request. One sentence carrying nothing from the request: why
/// the guest produced no response is the operator's to read in this emulator's log
/// rather than a client's to probe for.
const NO_GUEST_RESPONSE: &str = "The MicroVM returned no response.\n";

/// What one live token of a MicroVM's endpoint GRANTS — the two things this API mints a
/// token for, so a token grants its own and nothing else.
#[derive(Debug, Clone)]
pub enum TokenGrant {
    /// `CreateMicrovmAuthToken`'s `allowedPorts`: "The ports on the MicroVM endpoint the
    /// token grants access to", judged against the port the request addresses.
    Ports(Vec<PortSpecification>),
    /// `CreateMicrovmShellAuthToken`: "interactive shell access to a running MicroVM",
    /// which is the MicroVM's shell rather than a port of the server its guest bound
    /// ([`super::microvm_shell`]).
    Shell,
}

/// One live authentication token for one MicroVM's endpoint, held in
/// [`ServiceState::auth_tokens`] under [`auth_token_digest`] of the token.
#[derive(Debug, Clone)]
pub struct MicrovmAuthToken {
    /// The REGION whose endpoint authorised the mint — the partition holding the MicroVM
    /// the token grants access to. Every resource of this service is regional
    /// ([`super::regions`]), so the identity a presented token is matched on is this
    /// region AND [`Self::microvm_id`] together: a token can authenticate nothing in
    /// another region's partition.
    pub region: String,
    /// The `microvmId` the token grants access to — "The ID of the MicroVM to create
    /// an authentication token for", so it authenticates at that MicroVM's address and
    /// at no other.
    pub microvm_id: String,
    /// What the token grants at that address: the ports of it, or the MicroVM's shell.
    pub grant: TokenGrant,
    /// When `expirationInMinutes` runs out, never later than the MicroVM's own
    /// lifetime ([`ServiceState::record_auth_token`]).
    pub expires_at: Instant,
}

/// The digest a token is recognised by. The token itself is the caller's secret and is
/// never stored, logged or returned again, so the store keeps only this.
pub fn auth_token_digest(token: &str) -> String {
    ::hex::encode(Sha256::digest(token.as_bytes()))
}

/// ONE MicroVM's endpoint listener: the address that MicroVM alone is published on.
///
/// The state OWNS the listener: dropping it closes the port, so it is held for as long as
/// the MicroVM is one anything can connect to ([`ServiceState::reap_microvm_endpoints`]).
pub struct MicrovmProxy {
    /// `<loopback host>:<port>` — the authority the control plane publishes, with no
    /// scheme and no path.
    endpoint: String,
    /// Held only to keep the port open; the core owns the socket and its TLS.
    _listener: ::emulator_server::ResourceListener,
}

impl MicrovmProxy {
    /// The authority this MicroVM is published on.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Written by hand rather than derived: [`MicrovmProxy`] holds a core
/// `ResourceListener`, which is not `Debug`. The listener is reported by the address it
/// announced, which is what a reader needs.
impl std::fmt::Debug for MicrovmProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrovmProxy")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// The authority the MicroVM `microvm_id` is published on, opening its listener on first
/// use.
///
/// Opening it changes no MicroVM and is idempotent: the listener a MicroVM already holds
/// is kept, so its published endpoint is the same address for as long as it can be
/// connected to. `None` when the listener could not be opened at all, which leaves the
/// caller to answer with a modelled server error rather than to publish an address
/// nothing serves.
pub async fn ensure_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ServiceState>,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    microvm_id: &str,
) -> Option<String> {
    // The guard is taken and dropped before the await below: a lock held across an
    // await would make this future non-`Send`.
    let existing = {
        let store = state.lock();
        if store.microvm_proxies.len() >= MAX_PUBLISHED_ENDPOINTS
            && !store.microvm_proxies.contains_key(microvm_id)
        {
            // The sockets this store holds are capped, so nothing is bound past the
            // ceiling: the caller answers its modelled refusal rather than publishing an
            // address this process could not open.
            return None;
        }
        store
            .microvm_proxies
            .get(microvm_id)
            .map(|proxy| proxy.endpoint.clone())
    };
    if let Some(endpoint) = existing {
        return Some(endpoint);
    }

    let listener = super::shared_engine(engine)?
        .listeners()
        .http(
            MicrovmEndpointServer {
                state: state.clone(),
                // The stores and the in-process handle a guest's outbound connection is
                // decided against, taken where the listener is opened: the listener is
                // not a second emulator, it is a second ADDRESS of this one.
                connectors: super::microvm_network::connector_state(engine),
                callback,
                microvm_id: microvm_id.to_string(),
            },
            ::emulator_server::HttpPolicy::default(),
        )
        .await
        .ok()?;
    let endpoint = authority(&listener);

    let mut store = state.lock();
    // Another request may have opened one for this MicroVM while this one was binding;
    // keep the first, so a published endpoint never changes under a client. Dropping the
    // loser closes its port.
    if let Some(proxy) = store.microvm_proxies.get(microvm_id) {
        return Some(proxy.endpoint.clone());
    }
    // The cap is checked again on this side of the bind, because other launches ran while
    // this one was binding: dropping the listener closes the port it took.
    if store.microvm_proxies.len() >= MAX_PUBLISHED_ENDPOINTS {
        return None;
    }
    store.microvm_proxies.insert(
        microvm_id.to_string(),
        MicrovmProxy {
            endpoint: endpoint.clone(),
            _listener: listener,
        },
    );
    Some(endpoint)
}

/// The `host:port` an endpoint addresses the listener by, with an IPv6 host bracketed
/// as RFC 3986 requires (`[::1]:8443`), so the URL parses wherever the operator bound
/// it.
fn authority(listener: &::emulator_server::ResourceListener) -> String {
    let host = listener.host();
    let port = listener.port();
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Serves ONE MicroVM's endpoint on the core's listener. The core runs the browser
/// checks and owns CORS; this answers for the MicroVM the address belongs to.
struct MicrovmEndpointServer {
    state: ::engine::ServiceState<ServiceState>,
    /// The lambda-core store a MicroVM's `egressNetworkConnectors` are read from, and
    /// `None` in a deployment carrying none — which resolves no path at all.
    connectors: Option<::engine::ServiceState<super::microvm_network::ConnectorState>>,
    /// The in-process handle the emulated network is asked over.
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    /// The `microvmId` this address is the endpoint OF — the discriminator AWS puts in
    /// the host, which is why the published member carries none.
    microvm_id: String,
}

impl ::emulator_server::HttpHandler for MicrovmEndpointServer {
    fn handle(
        &self,
        req: ::http::Request<Vec<u8>>,
    ) -> Pin<Box<dyn Future<Output = ::http::Response<Vec<u8>>> + Send>> {
        let state = self.state.clone();
        let connectors = self.connectors.clone();
        let callback = self.callback.clone();
        let microvm_id = self.microvm_id.clone();
        Box::pin(async move {
            serve_request(&state, connectors.as_ref(), &callback, &microvm_id, &req).await
        })
    }
}

/// One request to a published MicroVM endpoint, answered from this store — the
/// decision the listener's handler serves, and the one this service's tests drive.
///
/// `microvm_id` is the MicroVM the ADDRESS belongs to: AWS publishes a host per MicroVM,
/// so the listener the request arrived on is what names it and the whole request path is
/// the guest's own. The MicroVM is resolved FIRST and the token second, which is the
/// order the real address works in: only an address that still publishes a MicroVM
/// answers at all, and then the token decides. An authenticated request is then handed to
/// the MicroVM's GUEST and answered with what the guest's own program produced.
///
/// The writes are the clock's own ([`ServiceState::materialize_due`], and the same on the
/// connector store a guest's connection is decided against), owed at this instant whether
/// or not anybody connected — plus the one write INBOUND TRAFFIC itself owes, which is
/// what an `idlePolicy` measures ([`ServiceState::record_endpoint_traffic`]): a request a
/// token granted reached the MicroVM, so the MicroVM is not idle and auto-resumes where
/// its policy says so.
///
/// `connectors` is the lambda-core store the MicroVM's `egressNetworkConnectors` are read
/// from and `callback` the in-process handle the emulated network is asked over; a
/// deployment carrying neither resolves no path, so every outbound connection a guest
/// opens is one nothing carried.
pub async fn serve_request(
    state: &::engine::ServiceState<ServiceState>,
    connectors: Option<&::engine::ServiceState<super::microvm_network::ConnectorState>>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    microvm_id: &str,
    req: &::http::Request<Vec<u8>>,
) -> ::http::Response<Vec<u8>> {
    let head = req.method() == ::http::Method::HEAD;
    let presented = req
        .headers()
        .get(AUTH_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(auth_token_digest);
    // A request carrying no authentication at all is refused for that, which is the one
    // thing the proxy's answer does name.
    let carried_authentication = presented.is_some();

    // Everything the request is answered from is copied out under the lock, which is
    // dropped before the guest runs: no store lock is held across an await.
    let now = Instant::now();
    let resolved = {
        let mut store = state.lock();
        store.materialize_due(now);

        // The address names the MicroVM's own id rather than a region, so the record is
        // resolved wherever this store holds it (see `super::regions`) — and the region
        // that holds it is half the identity a presented token has to match.
        let Some((region, microvm_id)) = store
            .microvm_anywhere(microvm_id)
            .filter(|(_, vm)| is_connectable_microvm_state(&vm.state))
            .map(|(region, vm)| (region.to_string(), vm.microvm_id.clone()))
        else {
            return endpoint_response(head, 404, NO_MICROVM_AT_THIS_ADDRESS);
        };

        // What the presented token GRANTS, where this store holds it live for the MicroVM
        // the address names. Which ports it covers is decided below, against the port the
        // request addresses; the identity and the clock are decided here.
        let granted = presented
            .and_then(|digest| store.auth_tokens.get(&digest))
            .filter(|token| {
                // The token's identity is the REGION and the `microvmId` together: a
                // token minted against one region's partition grants nothing at
                // another's.
                token.region == region
                    && token.microvm_id == microvm_id
                    && token.expires_at > now
            })
            .map(|token| token.grant.clone());
        // INBOUND TRAFFIC the token let through: "idle time is measured by inbound
        // traffic through the MicroVM proxy endpoint", so this is what resets the idle
        // clock and what auto-resumes a MicroVM resting on it. A refused request reached
        // the MicroVM with nothing, so it counts as no traffic.
        if granted.is_some() {
            store.record_endpoint_traffic(&region, &microvm_id, now);
        }
        let vm = store.microvm(&region, &microvm_id);
        // The run state the MicroVM is in AT THIS INSTANT, after the clock's own moves
        // have been materialised: whether a guest is executing inside it at all.
        let run_state = vm.map(|vm| vm.state.clone()).unwrap_or_default();
        let egress = super::microvm_network::GuestEgress {
            region: region.clone(),
            connectors: vm
                .map(|vm| vm.egress_network_connectors.clone())
                .unwrap_or_default(),
            link_local_address: vm
                .map(|vm| vm.link_local_address.clone())
                .unwrap_or_default(),
        };
        // The MACHINE the MicroVM's shell runs in, for a request a shell token granted:
        // its tree, its environment and the identity its kernel published, which a
        // MicroVM holds from the instant it reached `RUNNING` until it terminates.
        let machine = match &granted {
            Some(TokenGrant::Shell) => vm.and_then(|vm| vm.guest_machine.clone()),
            _ => None,
        };
        // The guest is the PROCESS this MicroVM runs its image version's program in,
        // started where it is not running yet and reached again where it is — which is
        // what makes what the guest holds survive from one request to the next. A request
        // presenting no live token starts nothing, and neither does one presenting a SHELL
        // token: the shell is performed in the MicroVM's machine rather than by the
        // program the guest process runs.
        let process = (matches!(granted, Some(TokenGrant::Ports(_)))
            && run_state == super::MICROVM_STATE_RUNNING)
            .then(|| store.guest_process(&region, &microvm_id, now))
            .flatten();
        (microvm_id, granted, run_state, egress, process, machine)
    };
    let (microvm_id, granted, run_state, egress, process, machine) = resolved;

    // Every attempt on a published endpoint is attributable: the MicroVM addressed and the
    // outcome. The presented token is NEVER logged — only the caller holds it — and
    // neither is the path, which carries whatever the client sent.
    let refused = |head: bool| {
        ::tracing::info!(
            audit = true,
            operation = "MicrovmEndpointRequest",
            outcome = "refused",
            microvm_id = %microvm_id,
            "MicroVM endpoint request without a live authentication token for the port it \
             addressed"
        );
        let message = if carried_authentication {
            AUTH_TOKEN_REQUIRED
        } else {
            MISSING_AUTHENTICATION
        };
        endpoint_response(head, 403, message)
    };

    let Some(granted) = granted else {
        return refused(head);
    };

    // What the token grants decides which of the MicroVM's two channels this request
    // reaches: a SHELL token grants the MicroVM's shell and no port of its endpoint, so
    // the request is performed in the MicroVM's machine rather than handed to the server
    // its guest bound.
    let allowed_ports = match granted {
        TokenGrant::Shell => {
            return shell_answer(head, &microvm_id, run_state.as_str(), machine, req).await;
        }
        TokenGrant::Ports(allowed_ports) => allowed_ports,
    };

    // The port the request ADDRESSES, which is what the token's `allowedPorts` has to
    // cover: the port it names in `X-aws-proxy-port`, and otherwise the port this endpoint
    // FRONTS — the port the MicroVM's own guest bound ([`PROXY_PORT_HEADER`],
    // [`DEFAULT_PROXIED_PORT`]).
    let fronted = match &process {
        Some(process) => process
            .fronted_port()
            .await
            .unwrap_or(DEFAULT_PROXIED_PORT),
        None => DEFAULT_PROXIED_PORT,
    };
    let Some(addressed_port) = addressed_port(req, fronted) else {
        return refused(head);
    };
    if !grants_port(&allowed_ports, addressed_port) {
        return refused(head);
    }

    // A guest answers only while the MicroVM is RUNNING it: one still provisioning has
    // not started it yet, and one resting under `SuspendMicrovm` ("preserving its full
    // memory and disk state") is not executing it, so the proxy reaches nothing inside
    // either — the same answer as a MicroVM with no guest to reach.
    if run_state != super::MICROVM_STATE_RUNNING {
        ::tracing::debug!(
            microvm_id = %microvm_id,
            state = %run_state,
            "the MicroVM is not running its guest, so nothing inside it answers"
        );
        return endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE);
    }

    // The guest's own response, or the proxy's answer for a MicroVM it reached nothing
    // inside of.
    let Some(process) = process else {
        ::tracing::debug!(
            microvm_id = %microvm_id,
            "the MicroVM runs no guest process this emulator could hand the request to"
        );
        return endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE);
    };
    match run_guest(
        connectors,
        callback,
        &egress,
        &process,
        guest_request(req, addressed_port),
    )
    .await
    {
        super::guest_runtime::GuestOutcome::Answered(answer) => guest_response(head, &answer),
        other => {
            ::tracing::debug!(
                microvm_id = %microvm_id,
                outcome = ?other,
                "a MicroVM's guest produced no response"
            );
            endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE)
        }
    }
}

/// What a published endpoint answers a request a SHELL token granted: what the MicroVM's
/// own shell ([`super::microvm_shell`]) wrote for the program the request carried, and the
/// status that program ended on.
///
/// A MicroVM that is not RUNNING has no shell to reach — one still provisioning has not
/// booted the machine its shell runs in, and one resting under `SuspendMicrovm` is
/// executing nothing — which the endpoint answers for exactly as it does for a guest it
/// reached nothing inside of.
///
/// The program is performed on a BLOCKING worker: it is the caller's own text, bounded
/// ([`super::microvm_shell`]) but synchronous, so performing it on the async worker would
/// hold a runtime thread for the whole of it. A shell that ended without answering (a
/// panic inside the session) is answered as a MicroVM nothing was reached inside of.
async fn shell_answer(
    head: bool,
    microvm_id: &str,
    run_state: &str,
    machine: Option<::std::sync::Arc<super::guest_machine::GuestMachine>>,
    req: &::http::Request<Vec<u8>>,
) -> ::http::Response<Vec<u8>> {
    let Some(machine) = machine.filter(|_| run_state == super::MICROVM_STATE_RUNNING) else {
        ::tracing::debug!(
            microvm_id = %microvm_id,
            state = %run_state,
            "the MicroVM is not running, so it has no shell to reach"
        );
        return endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE);
    };
    // The program is the request's own body, one command per line
    // ([`super::microvm_shell`] states the form of the channel), and it is performed in
    // the MicroVM's machine.
    let program = String::from_utf8_lossy(req.body()).to_string();
    let performed = ::tokio::task::spawn_blocking(move || {
        super::microvm_shell::run_in_machine(&machine, &program)
    })
    .await;
    let Ok(answered) = performed else {
        ::tracing::debug!(
            microvm_id = %microvm_id,
            "the MicroVM's shell ended without answering the program it was given"
        );
        return endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE);
    };
    // Attributable like every other attempt on the address: the MicroVM and the status its
    // shell ended on. The program is the client's own text and is not logged, and neither
    // is anything the shell wrote.
    ::tracing::info!(
        audit = true,
        operation = "MicrovmShellRequest",
        outcome = "performed",
        microvm_id = %microvm_id,
        status = answered.status,
        "a MicroVM's shell performed the program a shell token granted"
    );
    shell_response(head, &answered)
}

/// One answer from a MicroVM's shell: the lines the program wrote, and the status it ended
/// on in [`super::microvm_shell::SHELL_STATUS_HEADER`].
///
/// The HTTP status is the PROXY's — whether the shell was reached at all — so a program
/// that ended on a failure is still an answer the channel delivered; what the program
/// itself answered is the shell status beside it.
fn shell_response(
    head: bool,
    answered: &super::microvm_shell::ShellAnswer,
) -> ::http::Response<Vec<u8>> {
    let mut written = answered.output.join("\n");
    if !written.is_empty() {
        written.push('\n');
    }
    let length = written.len();
    let body = if head { Vec::new() } else { written.into_bytes() };
    ::http::Response::builder()
        .status(200)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", length.to_string())
        .header(
            super::microvm_shell::SHELL_STATUS_HEADER,
            answered.status.to_string(),
        )
        .body(body)
        // Every header this builds is this module's own constant and the status is a
        // number, so the builder cannot fail; answer the proxy's own status rather than
        // panicking if it ever does.
        .unwrap_or_else(|_| endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE))
}

/// Hand `request` to the MicroVM's guest PROCESS, answering the outbound connections it
/// opens from the emulated network as it waits on them.
///
/// The guest runs on its own thread and this future is what decides its connections, so
/// the two run together: the run asks, this answers, and the run's own bounds
/// ([`super::guest_runtime`]) are what end it either way.
async fn run_guest(
    connectors: Option<&::engine::ServiceState<super::microvm_network::ConnectorState>>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    egress: &super::microvm_network::GuestEgress,
    process: &super::guest_runtime::GuestProcess,
    request: super::guest_runtime::GuestRequest,
) -> super::guest_runtime::GuestOutcome {
    let (asks, mut answers) = ::tokio::sync::mpsc::unbounded_channel();
    let mut running = Box::pin(process.serve(request, asks));
    loop {
        ::tokio::select! {
            Some(ask) = answers.recv() => {
                let verdict = super::microvm_network::resolve(
                    connectors,
                    callback,
                    egress,
                    &ask.host,
                    ask.port,
                )
                .await;
                // A run that has already given up on this connection leaves nothing to
                // send to, which its own ceiling has already reported as unreached.
                let _ = ask.reply.send(verdict);
            }
            outcome = &mut running => return outcome,
        }
    }
}

/// The request a client sent, as the guest's own HTTP server is handed it.
///
/// `port` is the port of the MicroVM the request addressed, so the guest's server bound to
/// THAT port is the one that answers it. `req.url` is the path the client appended to the
/// published endpoint — which is the GUEST's own path in full, because the published
/// endpoint is an authority and carries none of it — and the headers are lowercased, as a
/// Node server reports them. The body is read as the TEXT a guest reads.
fn guest_request(
    req: &::http::Request<Vec<u8>>,
    port: i32,
) -> super::guest_runtime::GuestRequest {
    let query = req
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let path = req.uri().path();
    let path = if path.is_empty() { "/" } else { path };
    super::guest_runtime::GuestRequest {
        port,
        method: req.method().as_str().to_string(),
        url: format!("{path}{query}"),
        headers: req
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect(),
        body: String::from_utf8_lossy(req.body()).to_string(),
    }
}

/// One response from a guest, as the client reads it back: the status, the headers and
/// the body the guest's own program produced.
fn guest_response(head: bool, answer: &super::guest_runtime::GuestAnswer) -> ::http::Response<Vec<u8>> {
    let mut builder = ::http::Response::builder().status(answer.status);
    for (name, value) in &answer.headers {
        // The framing is the proxy's: a length or encoding the guest named describes a
        // body this leg re-frames itself.
        let framing = name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding");
        if !framing {
            builder = builder.header(name, value);
        }
    }
    let length = answer.body.len();
    let body = if head {
        Vec::new()
    } else {
        answer.body.clone()
    };
    builder
        .header("content-length", length.to_string())
        .body(body)
        // A guest that answered a header name or status no HTTP response can carry
        // answered nothing a client can read, which is the proxy's own answer.
        .unwrap_or_else(|_| {
            endpoint_response(head, NO_GUEST_RESPONSE_STATUS, NO_GUEST_RESPONSE)
        })
}

/// The port a request ADDRESSES on the MicroVM endpoint: the one it names in
/// [`PROXY_PORT_HEADER`], and `fronted` — the port this endpoint fronts — where it names
/// none.
///
/// `None` where the header carries something that is no port at all (a value that is not
/// a number, or one outside [`PORT_RANGE`]): such a request addresses no port of the
/// MicroVM, so no scope covers it.
fn addressed_port(req: &::http::Request<Vec<u8>>, fronted: i32) -> Option<i32> {
    let Some(named) = req.headers().get(PROXY_PORT_HEADER) else {
        return Some(fronted);
    };
    named
        .to_str()
        .ok()
        .map(str::trim)
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|port| PORT_RANGE.contains(port))
}

/// Whether `allowed` grants access to `port`, the port the request addresses — each arm
/// of the `PortSpecification` union as the model declares it (`port`, a `range` inclusive
/// of both endpoints, or `allPorts`).
fn grants_port(allowed: &[PortSpecification], port: i32) -> bool {
    allowed.iter().any(|spec| match spec {
        PortSpecification::AllPorts(_) => true,
        PortSpecification::Port(allowed) => *allowed == port,
        PortSpecification::Range(range) => range.start_port <= port && port <= range.end_port,
    })
}

/// One response from a published MicroVM endpoint.
///
/// The endpoint is plain HTTPS rather than a modelled operation — it is the address the
/// MicroVM is reached on — so it answers in its own plain-text form instead of this
/// service's restJson1 envelope. Every message is a constant of this module, never
/// built from the request.
///
/// A `HEAD` carries the metadata of what a `GET` of the same address would answer — its
/// status and its length — without the body, which is what the method means.
fn endpoint_response(head: bool, status: u16, message: &'static str) -> ::http::Response<Vec<u8>> {
    let body = if head {
        Vec::new()
    } else {
        message.as_bytes().to_vec()
    };
    ::http::Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", message.len().to_string())
        .body(body)
        // Every argument is this function's own constant, so the builder cannot fail;
        // answer a bare 500 rather than panicking if it ever does.
        .unwrap_or_else(|_| {
            let mut response = ::http::Response::new(Vec::new());
            *response.status_mut() = ::http::StatusCode::INTERNAL_SERVER_ERROR;
            response
        })
}

impl ServiceState {
    /// Record the token `CreateMicrovmAuthToken` or `CreateMicrovmShellAuthToken` just
    /// minted for `microvm_id`: its digest, what it grants and when it stops being
    /// accepted.
    ///
    /// `expirationInMinutes` is "the duration in minutes before the authentication
    /// token expires" and the model gives it no maximum, so the stored expiry is capped
    /// by the MicroVM's OWN lifetime: a MicroVM is terminated by the platform at
    /// `maximumDurationInSeconds` and a terminated MicroVM accepts no connection, so a
    /// token cannot grant access past it.
    pub fn record_auth_token(
        &mut self,
        region: &str,
        microvm_id: &str,
        token: &str,
        expiration_in_minutes: i32,
        grant: TokenGrant,
        now: Instant,
    ) -> Instant {
        let requested_seconds = u64::try_from(expiration_in_minutes)
            .unwrap_or_default()
            .saturating_mul(60);
        let platform_limit = Duration::from_secs(
            u64::try_from(MAX_MAXIMUM_DURATION_IN_SECONDS).unwrap_or_default(),
        );
        let microvm_end = self
            .microvm(region, microvm_id)
            .and_then(|vm| vm.expiry_due_at)
            .or_else(|| now.checked_add(platform_limit))
            .unwrap_or(now);
        let expires_at = now
            .checked_add(Duration::from_secs(requested_seconds))
            .map_or(microvm_end, |requested| requested.min(microvm_end));

        if self.auth_tokens.len() >= MAX_LIVE_AUTH_TOKENS {
            let evict = self
                .auth_tokens
                .iter()
                .min_by_key(|(_, token)| token.expires_at)
                .map(|(digest, _)| digest.clone());
            if let Some(digest) = evict {
                self.auth_tokens.remove(&digest);
            }
        }
        self.auth_tokens.insert(
            auth_token_digest(token),
            MicrovmAuthToken {
                region: region.to_string(),
                microvm_id: microvm_id.to_string(),
                grant,
                expires_at,
            },
        );
        expires_at
    }

    /// Forget every authentication token the clock has expired, and every one whose
    /// MicroVM can no longer be connected to — both owed at the same instant whether
    /// or not anybody called. Applied by [`ServiceState::materialize_due`].
    ///
    /// A token is kept only while the MicroVM of ITS OWN region is still connectable, so
    /// an identically-named MicroVM of another partition keeps nothing alive.
    pub(super) fn expire_auth_tokens(&mut self, now: Instant) {
        let connectable: BTreeSet<(String, String)> = self
            .regions
            .iter()
            .flat_map(|(region, regional)| {
                regional
                    .microvms
                    .values()
                    .filter(|vm| is_connectable_microvm_state(&vm.state))
                    .map(|vm| (region.clone(), vm.microvm_id.clone()))
            })
            .collect();
        self.auth_tokens.retain(|_, token| {
            token.expires_at > now
                && connectable.contains(&(token.region.clone(), token.microvm_id.clone()))
        });
    }
}
