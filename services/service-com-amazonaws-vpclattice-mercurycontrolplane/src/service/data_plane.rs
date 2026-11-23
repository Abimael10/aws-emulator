// SPDX-License-Identifier: CC0-1.0

//! The VPC Lattice DATA plane: how a request a client sends to the name a service PUBLISHES
//! is recognised, and how it is answered out of the listener, rule and target-group records
//! the control plane holds.
//!
//! A service this control plane creates publishes a name for clients to call it on —
//! `CreateService`'s `dnsEntry` is "The public DNS name of the service" — and a client in a
//! VPC associated with the service network the service is associated with "can be clients and
//! communicate with other services in the service network"
//! (`CreateServiceNetworkVpcAssociation`). So the published name is an address something has
//! to ANSWER at: a request to it is routed by the service's listener, matched against that
//! listener's rules, and forwarded to the target group the matched rule's action names, whose
//! target produces the response. Without that, the one address this service tells a caller to
//! use is answered by the emulator's front door, which can resolve no service from a host
//! whose first label names a lattice service, so every in-VPC consumer of a Lattice service
//! waits for an answer that never comes.
//!
//! # What a request addresses, and how it is resolved
//!
//! Two addressings resolve, and each is checked against the records this service holds, so
//! one request has one claimant:
//!
//! * HOST-STYLE, the addressing the published `dnsEntry` gives a client: the first label of
//!   the host the request was sent to is a stored service's own label — the label its
//!   announced name is minted from ([`crate::state::service_label`], which carries the
//!   service's id, so no two services share one) — or the host IS a stored service's
//!   `customDomainName`. The whole request path is then the path the rules are matched
//!   against;
//! * PREFIXED PATH-STYLE: the plane is named by the reserved namespace segment
//!   ([`::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`]) and this
//!   service's own ([`DATA_PLANE_SEGMENT`]), then the service's id, name or announced host,
//!   then the request path — `/_aws/vpc-lattice/{service}/{path}`. The namespace segment
//!   begins with `_`, which no modelled path of any service does, so the prefix collides
//!   with no control-plane route.
//!
//! Nothing host-independent is read: a request that names neither a service's own host nor
//! the reserved prefix is left to whatever it was really addressed to.
//!
//! Either way, the plane answers only for a service a client is in a position to REACH: a
//! service is reached through a service network it is associated with, and a VPC association
//! of that network is what "enables all the resources within that VPC to be clients and
//! communicate with other services in the service network"
//! (`CreateServiceNetworkVpcAssociation`). Until both associations exist, no client is in a
//! position to send the service traffic and nothing of this emulator serves its name
//! ([`reachable`]).
//!
//! # What answers it
//!
//! Every status, header and byte is computed from what a create or an update persisted, and
//! serving traffic moves no status, consumes no quota and bumps no counter.
//!
//! The LISTENER the request reached is the one on the port the client addressed, and — for a
//! client that reached this emulator's own endpoint, whose port is not the service's — the
//! lowest-numbered listener of the service that carries HTTP requests ([`SERVED_PROTOCOLS`]).
//! Its rules are evaluated in priority order, lowest value first: "each rule for a specific
//! listener must have a unique priority" (`CreateRule`'s `priority`), and the listener's
//! DEFAULT rule — the rule its `defaultAction` is stored as, which matches on nothing of its
//! own — carries [`crate::state::DEFAULT_RULE_PRIORITY`], so it is reached last and is what
//! answers a request no other rule matches ("the default rule is used if no other rules
//! match", `CreateListener`'s `defaultAction`).
//!
//! A `fixedResponse` action is answered in full: its `statusCode` is the response, with no
//! body, because the body of such a response is not a document this emulator holds.
//!
//! A `forward` IS performed. The target group it selects is the one the weights send the
//! requests to — "the weight determines how requests are distributed to the target group"
//! (`WeightedTargetGroup$weight`) — and what then answers depends on that group's type
//! (`TargetGroupType`):
//!
//! * `LAMBDA`: the registered function is INVOKED in process with the Lattice event and the
//!   client is answered from what the run returned ([`super::lambda_target`]);
//! * `IP` and `INSTANCE`: the registration names an address, or an instance of this
//!   emulator's own account, and what answers is the server that workload RUNS on the port
//!   the registration receives traffic on, read through the one register such a server is
//!   published in ([`::service_common::served_http_addresses`]). The client is answered with
//!   that server's own status and bytes.
//!
//! No body and no status is invented for a backend that never ran: a forward whose group
//! holds no target to send the request to is answered [`NO_TARGET_STATUS`], and one whose
//! target this emulator cannot reach — nothing listening at the address, a target of a kind
//! that is another service's own plane — [`TARGET_UNREACHABLE_STATUS`].

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_common::data_plane_addressing::{
    addressed_authority, addressed_authority_of, addressed_port, first_segment, host_only,
    EMULATOR_PATH_NAMESPACE,
};
use ::smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::ops::types::{
    ForwardAction, HeaderMatch, HeaderMatchType, HttpMatch, PathMatchType, RuleAction, RuleMatch,
};
use crate::state::{
    service_state, ComAmazonawsVpclatticeMercurycontrolplaneState as LatticeState, LatticeService,
    Listener, TargetGroup,
};

/// The segment that names THIS service's data plane behind the reserved namespace: the
/// `arnNamespace`/`endpointPrefix` this model publishes, which is also its signing name.
const DATA_PLANE_SEGMENT: &str = crate::state::ARN_SERVICE;

/// The PREFIXED PATH-STYLE address of one service's plane:
/// `/_aws/vpc-lattice/{service}`, which a request path follows.
///
/// The one place that addressing is written, beside [`prefixed_addressing`], which is the one
/// place it is read.
#[must_use]
pub fn data_plane_path(named: &str) -> String {
    format!("/{EMULATOR_PATH_NAMESPACE}/{DATA_PLANE_SEGMENT}/{named}")
}

/// The listener protocols that carry a request this plane can read and answer: the two
/// `ListenerProtocol` values whose listeners route HTTP. A `TLS_PASSTHROUGH` listener hands
/// the connection on without reading a request out of it, so it answers none of these.
pub const SERVED_PROTOCOLS: &[&str] = &["HTTP", "HTTPS"];

/// What this plane answers while nothing is serving the name the request addressed: the
/// service is still being created, or the listener the request reached is gone.
///
/// THE EMULATOR'S OWN status for that condition, carrying no body: a proxy with nothing
/// behind the name answers the service-unavailable class, and the body of such an answer is
/// not a document this emulator holds.
pub const NOT_SERVING_STATUS: u16 = 503;

/// What a `forward` whose target group holds NO target to send the request to is answered
/// with. Same class, and the same reason, as [`NOT_SERVING_STATUS`]: nothing is behind the
/// forward, and no body is invented for it.
pub const NO_TARGET_STATUS: u16 = 503;

/// What a `forward` whose target this emulator cannot reach is answered with: the target
/// group holds one, and the connection to it cannot be made — nothing of this emulator is
/// listening at the registered address, or the registration names a plane another service
/// owns. The bad-gateway class, with no body for the same reason.
pub const TARGET_UNREACHABLE_STATUS: u16 = 502;

/// Whether a path or a header match compares WITHOUT regard to case when the rule configures
/// nothing for it.
///
/// `caseSensitive` is "Indicates whether the match is case sensitive" (`PathMatch`,
/// `HeaderMatch`) and is modelled optional with no documented default, so a rule that
/// configures none is applied with the LESS selective comparison: a rule is never narrowed by
/// a condition its author did not ask for.
const DEFAULT_CASE_SENSITIVE: bool = false;

/// The terminal status a resource that is serving traffic reports: the `ServiceStatus` and
/// `TargetGroupStatus` value every other value of those enums is on the way to or from
/// (`smithy_query(query="shape",shape="ServiceStatus")`).
const ACTIVE_STATUS: &str = "ACTIVE";

/// What a data-plane request ADDRESSES, as the wire carries it: the authority it was sent to
/// — absent on a request that carries neither an absolute-form target nor a `Host` header —
/// and its path.
///
/// Built from BOTH shapes the router hands this module, the buffered parts the claim reads and
/// the runtime-API request the serving seam reads, so the claim cannot resolve a different
/// service than the answer does.
#[derive(Debug)]
struct Addressed<'a> {
    authority: Option<&'a str>,
    path: &'a str,
}

impl<'a> Addressed<'a> {
    /// The addressing of the buffered request these parts came from.
    fn of_parts(parts: &'a http::request::Parts) -> Self {
        Self {
            authority: addressed_authority_of(
                parts.uri.authority().map(http::uri::Authority::as_str),
                parts
                    .headers
                    .get(::http::header::HOST)
                    .and_then(|host| host.to_str().ok()),
            ),
            path: parts.uri.path(),
        }
    }

    /// The addressing of a runtime-API request, whose URI arrives as a string and is parsed by
    /// the `http` crate rather than split by hand.
    fn of_request(req: &'a RtRequest, uri: &'a http::Uri) -> Self {
        Self {
            authority: addressed_authority(req.uri(), req.headers().get("host")),
            path: uri.path(),
        }
    }
}

/// What a request resolves to: the service it addresses, the listener it reached, and the path
/// WITHIN that service's own endpoint — which is what the rules are matched against.
#[derive(Debug)]
struct Resolved {
    service_id: String,
    listener_id: String,
    request_path: String,
}

/// Whether this data plane serves the request these `parts` came from — read by
/// `Service::can_handle`, so a request addressed to a service this crate holds reaches this
/// service instead of the front door's `UnknownService`.
pub fn claims_parts<Eng: Send + Sync + 'static>(engine: &Eng, parts: &http::request::Parts) -> bool {
    resolve(engine, &Addressed::of_parts(parts)).is_some()
}

/// [`claims_parts`] for the runtime-API request the serving seam is handed, resolved by the
/// same rule.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    let uri = parsed_uri(req.uri());
    resolve(engine, &Addressed::of_request(req, &uri)).is_some()
}

/// The request URI of a runtime-API request, parsed by the `http` crate: the authority — absent
/// on the origin-form URI a client sends on the wire — the path and the query.
///
/// A URI this parse rejects is one the server would not have accepted, and it resolves to
/// `http::Uri`'s own default (`/`), which names no service and claims nothing.
fn parsed_uri(uri: &str) -> http::Uri {
    uri.parse::<http::Uri>().unwrap_or_default()
}

/// Which service and listener `addressed` addresses, and the path within that service's
/// endpoint, or `None` when this data plane serves the request not at all.
///
/// The two addressings are the module documentation's, in that order. Each resolves only to a
/// service that has a listener carrying requests of this kind ([`listener_addressed`]), so the
/// claim and the answer cannot disagree about which requests this plane can answer.
fn resolve<Eng: Send + Sync + 'static>(engine: &Eng, addressed: &Addressed<'_>) -> Option<Resolved> {
    let state = service_state(engine)?;
    let store = state.lock();
    let port = addressed_port(addressed.authority);

    // PREFIXED PATH-STYLE, the one addressing that is explicit about naming this plane, and so
    // the one read whatever the request's host is.
    if let Some((named, path)) = prefixed_addressing(addressed.path) {
        let service = named_service(&store, named)?;
        if !reachable(&store, &service.id) {
            return None;
        }
        let listener = listener_addressed(&store, &service.id, port)?;
        return Some(Resolved {
            service_id: service.id.clone(),
            listener_id: listener.id.clone(),
            request_path: path.to_string(),
        });
    }
    // A path under the reserved namespace that did NOT name this plane names another one,
    // which serves its own requests: reading it as a lattice service's would make two services
    // claim one request, which the router answers `500 AmbiguousService`.
    if first_segment(addressed.path).is_some_and(|(first, _)| first == EMULATOR_PATH_NAMESPACE) {
        return None;
    }

    // HOST-STYLE: the host the published `dnsEntry` gives a client, or the service's own
    // custom domain name.
    let authority = addressed.authority?;
    let service = store
        .services
        .values()
        .find(|service| addresses_published_name(service, authority))?;
    if !reachable(&store, &service.id) {
        return None;
    }
    let listener = listener_addressed(&store, &service.id, port)?;
    Some(Resolved {
        service_id: service.id.clone(),
        listener_id: listener.id.clone(),
        request_path: addressed.path.to_string(),
    })
}

/// The service and the path a PREFIXED PATH-STYLE request names, or `None` for a path that is
/// not one: `/_aws/vpc-lattice/{service}/{path}`.
fn prefixed_addressing(path: &str) -> Option<(&str, &str)> {
    let (namespace, under) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, under) = first_segment(under)?;
    if plane != DATA_PLANE_SEGMENT {
        return None;
    }
    let (named, path) = first_segment(under)?;
    if named.is_empty() {
        return None;
    }
    Some((named, path))
}

/// Whether `addressed` — the `host[:port]` a request was sent to — names `service`'s own
/// published address.
///
/// The announced name is the service's own LABEL in whatever zone this emulator answers in
/// ([`crate::state::announced_dns_name`]), and that label carries the service's id, so the
/// first label of the host is what identifies the service whichever zone the client read the
/// name in. A service's `customDomainName` — "the custom domain name of the service" — is the
/// other name a client reaches it by, and is matched whole.
fn addresses_published_name(service: &LatticeService, addressed: &str) -> bool {
    let host = host_only(addressed);
    if host.is_empty() {
        return false;
    }
    let label = crate::state::service_label(&service.name, &service.id);
    let first = host.split('.').next().unwrap_or_default();
    if first.eq_ignore_ascii_case(&label) {
        return true;
    }
    service
        .custom_domain_name
        .as_deref()
        .is_some_and(|custom| custom.eq_ignore_ascii_case(host))
}

/// Whether a client is in a position to REACH the service `service_id` names at all.
///
/// A service is reached through a service network it is associated with, and a VPC association
/// of that network is what puts a client in a position to send it traffic: "it enables all the
/// resources within that VPC to be clients and communicate with other services in the service
/// network" (`CreateServiceNetworkVpcAssociation`'s own documentation). So a service that is
/// associated with no network, and one whose networks no VPC is associated with, has no client
/// — and this plane answers nothing for its name rather than serving a request no client of
/// the emulated network could have made.
fn reachable(store: &LatticeState, service_id: &str) -> bool {
    store
        .service_associations
        .values()
        .filter(|association| association.service_id == service_id)
        .any(|association| {
            store.vpc_associations.values().any(|vpc| {
                vpc.service_network_id == association.service_network_id
            })
        })
}

/// The service `named` names, under every form a client writes one in: the bare id, the
/// service's name, or the first label of its announced host.
fn named_service<'s>(store: &'s LatticeState, named: &str) -> Option<&'s LatticeService> {
    store.services.values().find(|service| {
        named.eq_ignore_ascii_case(&service.id)
            || named.eq_ignore_ascii_case(&service.name)
            || named.eq_ignore_ascii_case(&crate::state::service_label(&service.name, &service.id))
    })
}

/// The listener the request reached: the one on the port the client addressed and, where that
/// port names none of them — a client that reached this emulator's own endpoint addressed the
/// EMULATOR's port, not the service's — the lowest-numbered one.
///
/// Only a listener on a protocol that carries this request is considered
/// ([`SERVED_PROTOCOLS`]), so a service whose listeners carry none has none here — which is
/// what makes the claim and the answer agree.
fn listener_addressed<'s>(
    store: &'s LatticeState,
    service_id: &str,
    port: Option<i32>,
) -> Option<&'s Listener> {
    let mut serving: Vec<&Listener> = store
        .listeners
        .values()
        .filter(|listener| {
            listener.service_id == service_id
                && SERVED_PROTOCOLS
                    .iter()
                    .any(|protocol| listener.protocol.eq_ignore_ascii_case(protocol))
        })
        .collect();
    serving.sort_by_key(|listener| (listener.port, listener.sequence));
    match port {
        Some(port) => serving.into_iter().find(|listener| listener.port == port),
        None => serving.into_iter().next(),
    }
}

/// One request this plane serves, as the records read it.
#[derive(Debug)]
pub(super) struct DataPlaneRequest {
    /// The method the client sent.
    pub(super) method: String,
    /// The path within the service's own endpoint, which the rules match against.
    pub(super) path: String,
    /// The query string, without its `?`.
    pub(super) query: String,
    /// Every header it carried, by lower-cased name, in the order it carried them.
    pub(super) headers: Vec<(String, String)>,
    /// The body it sent.
    pub(super) body: Vec<u8>,
}

impl DataPlaneRequest {
    /// What the runtime-API request carries, for the path [`resolve`] read out of it and the
    /// `uri` the `http` crate parsed out of it.
    fn of(req: &RtRequest, uri: &http::Uri, path: &str) -> Self {
        Self {
            method: req.method().to_string(),
            path: path.to_string(),
            query: uri.query().unwrap_or_default().to_string(),
            headers: req
                .headers()
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                .collect(),
            body: req
                .body()
                .bytes()
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
        }
    }

    /// The first value of the header `name` names, read case-insensitively.
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One answer this plane renders: the status, the headers it carries and the body.
#[derive(Debug, Default)]
pub(super) struct Served {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Served {
    /// A status with no body: what this plane answers for a request it could not pass on,
    /// where no body of its own is this emulator's to invent.
    pub(super) fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }
}

/// What the records decided one request is answered BY, read under the store's lock and
/// performed with no lock held.
#[derive(Debug)]
enum Chosen {
    /// A `fixedResponse` action: the status it carries.
    Fixed(u16),
    /// A `forward` to a `LAMBDA` target group: the function to invoke and what the event says
    /// about where the request came from ([`super::lambda_target`]).
    Lambda(super::lambda_target::LambdaTarget),
    /// A `forward` to an `IP` or `INSTANCE` target group: the address the registration names
    /// and the port it receives traffic on.
    Address { address: String, port: i32 },
    /// A `forward` with no target to send the request to.
    NoTarget,
    /// A `forward` whose target this emulator cannot reach.
    Unreachable,
}

/// Answer a request this plane claimed, out of the records the control plane holds.
///
/// Reached from the plane `Service::override_operation` hands back for a request
/// [`claims_request`] resolved, so the resolution here repeats that one; a resolution that has
/// gone away between the two — the service or its listener deleted by another request in
/// between — is nothing serving the name, which is [`NOT_SERVING_STATUS`] rather than a panic.
///
/// `ctx` is what a `forward` to a `LAMBDA` target group needs: the in-process transport the
/// function is invoked over ([`super::lambda_target`]). The store's guard is RELEASED before
/// that invocation, so one client's handler cannot stall another client's request.
pub async fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    let uri = parsed_uri(req.uri());
    let Some(resolved) = resolve(engine, &Addressed::of_request(req, &uri)) else {
        return rendered(&Served::status(NOT_SERVING_STATUS));
    };
    let request = DataPlaneRequest::of(req, &uri, &resolved.request_path);
    let Some(state) = service_state(engine) else {
        return rendered(&Served::status(NOT_SERVING_STATUS));
    };
    let now = SystemTime::now();
    // What the records decide, under a lock that is released before anything else is asked.
    // Read, never written: this plane moves no status.
    let chosen = {
        let mut store = state.lock();
        // A target group whose asynchronous deletion has fallen due is gone for every caller,
        // which was owed at the same instant whether or not anybody called.
        store.expire_deleted_target_groups(now);
        let store = &*store;
        let Some(service) = store.services.get(&resolved.service_id) else {
            return rendered(&Served::status(NOT_SERVING_STATUS));
        };
        // A service the clock still owes its `ACTIVE` status is not serving the name it
        // published yet.
        if service.status(now) != ACTIVE_STATUS {
            return rendered(&Served::status(NOT_SERVING_STATUS));
        }
        let Some(listener) = store.listeners.get(&resolved.listener_id) else {
            return rendered(&Served::status(NOT_SERVING_STATUS));
        };
        let Some(action) = matched_action(store, listener, &request) else {
            return rendered(&Served::status(NOT_SERVING_STATUS));
        };
        chosen(store, service, listener, &action, now)
    };
    let served = match chosen {
        Chosen::Fixed(status) => Served::status(status),
        Chosen::Lambda(target) => super::lambda_target::served(ctx, &target, &request).await,
        Chosen::Address { address, port } => address_target(engine, &address, port, &request),
        Chosen::NoTarget => Served::status(NO_TARGET_STATUS),
        Chosen::Unreachable => Served::status(TARGET_UNREACHABLE_STATUS),
    };
    rendered(&served)
}

/// The action the listener performs for this request: the one carried by the first rule of the
/// listener, in priority order, whose match the request satisfies.
///
/// "Rules are evaluated in priority order, from the lowest value to the highest value"
/// (`CreateRule`'s own documentation), and the listener's DEFAULT rule holds
/// [`crate::state::DEFAULT_RULE_PRIORITY`] and matches on nothing of its own, so it is reached
/// last and answers a request no other rule matched. `None` only for a listener with no rule
/// at all, which no create of this service leaves behind.
fn matched_action(
    store: &LatticeState,
    listener: &Listener,
    request: &DataPlaneRequest,
) -> Option<RuleAction> {
    let mut rules: Vec<&crate::state::Rule> = store
        .rules
        .values()
        .filter(|rule| rule.listener_id == listener.id)
        .collect();
    rules.sort_by_key(|rule| (rule.priority, rule.sequence));
    rules
        .into_iter()
        .find(|rule| rule_matches(&rule.r#match, request))
        .map(|rule| rule.action.clone())
}

/// Whether `request` satisfies `matched` — a rule's whole match.
fn rule_matches(matched: &RuleMatch, request: &DataPlaneRequest) -> bool {
    match matched {
        RuleMatch::httpMatch(http) => http_matches(http, request),
    }
}

/// Whether `request` satisfies every criterion `http` configures: "Describes criteria that can
/// be applied to incoming requests" (`HttpMatch`), so a criterion the match leaves out
/// constrains nothing and a match that configures none matches every request.
fn http_matches(http: &HttpMatch, request: &DataPlaneRequest) -> bool {
    if let Some(method) = http.method.as_deref() {
        // "The HTTP method type": a method names the one verb it is, compared without regard
        // to case because a method is written in upper case on the wire.
        if !request.method.eq_ignore_ascii_case(method.trim()) {
            return false;
        }
    }
    if let Some(path) = http.path_match.as_ref() {
        let sensitive = path.case_sensitive.unwrap_or(DEFAULT_CASE_SENSITIVE);
        let matched = match &path.r#match {
            PathMatchType::Exact(exact) => compared(&request.path, exact, sensitive),
            PathMatchType::Prefix(prefix) => prefixed(&request.path, prefix, sensitive),
        };
        if !matched {
            return false;
        }
    }
    http.header_matches
        .as_deref()
        .unwrap_or_default()
        .iter()
        .all(|header| header_matches(header, request))
}

/// Whether `request` carries a header `matched` is satisfied by: "matches incoming requests
/// with rule based on request header value" (`HeaderMatch`), so the named header has to be
/// there and its value has to satisfy the configured match.
fn header_matches(matched: &HeaderMatch, request: &DataPlaneRequest) -> bool {
    let Some(value) = request.header(&matched.name.to_ascii_lowercase()) else {
        return false;
    };
    let sensitive = matched.case_sensitive.unwrap_or(DEFAULT_CASE_SENSITIVE);
    match &matched.r#match {
        HeaderMatchType::Exact(exact) => compared(value, exact, sensitive),
        HeaderMatchType::Prefix(prefix) => prefixed(value, prefix, sensitive),
        HeaderMatchType::Contains(contains) => {
            if sensitive {
                value.contains(contains.as_str())
            } else {
                value
                    .to_ascii_lowercase()
                    .contains(&contains.to_ascii_lowercase())
            }
        }
    }
}

/// Whether `value` IS `expected`, compared with or without regard to case.
fn compared(value: &str, expected: &str, sensitive: bool) -> bool {
    if sensitive {
        value == expected
    } else {
        value.eq_ignore_ascii_case(expected)
    }
}

/// Whether `value` STARTS WITH `expected`, compared with or without regard to case.
fn prefixed(value: &str, expected: &str, sensitive: bool) -> bool {
    if sensitive {
        value.starts_with(expected)
    } else {
        value
            .to_ascii_lowercase()
            .starts_with(&expected.to_ascii_lowercase())
    }
}

/// What the records decide `action` is answered by.
fn chosen(
    store: &LatticeState,
    service: &LatticeService,
    listener: &Listener,
    action: &RuleAction,
    now: SystemTime,
) -> Chosen {
    let forward = match action {
        // "The rule returns a custom HTTP response" (`RuleAction$fixedResponse`): the status
        // the action carries IS the answer. `statusCode` is modelled `@range(100, 599)`, so a
        // value no response status can take is refused before a create stores one.
        RuleAction::fixedResponse(fixed) => {
            return Chosen::Fixed(u16::try_from(fixed.status_code).unwrap_or(NO_TARGET_STATUS));
        }
        RuleAction::Forward(forward) => forward,
    };
    let Some(group) = forwarded_group(store, forward) else {
        return Chosen::NoTarget;
    };
    // A target group whose own creation the clock has not finished has no target serving
    // traffic yet, and one being deleted no longer takes any.
    if group.status(now) != ACTIVE_STATUS {
        return Chosen::NoTarget;
    }
    // The target that receives the request: the first registration the group holds that a
    // deregistration has not started draining, in registration order.
    let Some(target) = group
        .targets
        .iter()
        .find(|target| target.draining_at.is_none())
    else {
        return Chosen::NoTarget;
    };
    match group.group_type.as_str() {
        crate::state::TYPE_LAMBDA => {
            Chosen::Lambda(super::lambda_target::LambdaTarget {
                function_arn: target.id.clone(),
                target_group_arn: group.arn.clone(),
                // "The version of the event structure that your Lambda function receives …
                // The default is `V1`" (`TargetGroupConfig$lambdaEventStructureVersion`).
                event_structure_version: group
                    .config
                    .lambda_event_structure_version
                    .clone()
                    .unwrap_or_else(|| {
                        crate::state::DEFAULT_LAMBDA_EVENT_STRUCTURE_VERSION.to_string()
                    }),
                service_arn: service.arn.clone(),
                service_network_arn: sole_service_network_arn(store, &service.id),
            })
        }
        crate::state::TYPE_IP | crate::state::TYPE_INSTANCE => Chosen::Address {
            address: target.id.clone(),
            // "The port on which the target is listening", which the registration settled on
            // from the target group's own configuration where the caller sent none.
            port: target
                .port
                .or(group.config.port)
                .unwrap_or_else(|| default_target_port(listener)),
        },
        // An `ALB` target group registers "the ARN of an Application Load Balancer", whose
        // listeners are another service's data plane: this plane opens no connection to it, so
        // the request is answered as one whose target could not be reached.
        _ => Chosen::Unreachable,
    }
}

/// The service network the request reached this service THROUGH, where the records leave no
/// doubt which one that was: the ARN of the service network the service is associated with,
/// when it is associated with exactly one.
///
/// A client reaches a service through a service network it is associated with, so a service
/// associated with one network was reached through that network; with several, which of them
/// carried this request is not something these records establish, and nothing is reported for
/// it rather than one of them being guessed at.
fn sole_service_network_arn(store: &LatticeState, service_id: &str) -> Option<String> {
    let mut associated = store
        .service_associations
        .values()
        .filter(|association| association.service_id == service_id);
    let only = associated.next()?;
    associated
        .next()
        .is_none()
        .then(|| only.service_network_arn.clone())
}

/// The target group a `forward` sends this request to: the one the weights distribute the
/// requests to.
///
/// "The weight determines how requests are distributed to the target group"
/// (`WeightedTargetGroup$weight`), so an entry weighted 0 — and one weighted nothing beside
/// others, which the create leaves absent — receives none, and the entry with the greatest
/// weight is the one requests go to. Deterministic on purpose: a read of these records must
/// answer the same request the same way, so nothing here keeps a counter or draws at random.
fn forwarded_group<'s>(store: &'s LatticeState, forward: &ForwardAction) -> Option<&'s TargetGroup> {
    forward
        .target_groups
        .iter()
        .filter(|weighted| weighted.weight.unwrap_or_default() > 0)
        .max_by_key(|weighted| weighted.weight.unwrap_or_default())
        .and_then(|weighted| {
            crate::state::id_from_identifier(
                &weighted.target_group_identifier,
                crate::state::PREFIX_TARGET_GROUP,
            )
        })
        .and_then(|id| store.target_groups.get(&id))
}

/// The port a registration that named none, under a target group that configures none,
/// receives traffic on: the LISTENER's own port, which is the port the request arrived on.
fn default_target_port(listener: &Listener) -> i32 {
    listener.port
}

/// What the workload registered at `address` answers for this request, read through the one
/// register a server this emulator runs is published in
/// ([`::service_common::served_http_addresses`]).
///
/// An address nothing of this emulator is listening at is a target the connection cannot be
/// made to, which is [`TARGET_UNREACHABLE_STATUS`] — never a status or a body this plane
/// invented for a backend that never ran.
fn address_target<Eng: Send + Sync + 'static>(
    engine: &Eng,
    address: &str,
    port: i32,
    request: &DataPlaneRequest,
) -> Served {
    let Some(answered) = ::service_common::served_http_addresses::ServedAddresses::of(engine)
        .and_then(|addresses| addresses.serve(address, port, &request.method, &request.path))
    else {
        return Served::status(TARGET_UNREACHABLE_STATUS);
    };
    let mut headers = Vec::new();
    if let Some(content_type) = answered.content_type {
        headers.push(("content-type".to_string(), content_type));
    }
    Served {
        status: answered.status,
        headers,
        body: answered.body,
    }
}

/// One rendered answer: the status, its headers and its body, as the wire carries them.
fn rendered(served: &Served) -> RtResponse {
    let mut response = RtResponse::new(
        StatusCode::try_from(served.status).unwrap_or_else(|_| {
            StatusCode::try_from(NOT_SERVING_STATUS).expect("503 is a status")
        }),
        SdkBody::from(served.body.clone()),
    );
    for (name, value) in &served.headers {
        if let Err(refused) = response.headers_mut().try_append(name.clone(), value.clone()) {
            // A header a target's response named is a returned value, so one the wire cannot
            // carry is left off the response rather than failing it. APPENDED, not inserted: a
            // name the answer carries more than once is sent more than once, which is how
            // several cookies travel.
            ::tracing::warn!(
                header = %name,
                error = %refused,
                "the header the target's answer carries cannot be sent on the wire and is \
                 left off the response"
            );
        }
    }
    let _ = response
        .headers_mut()
        .try_insert("content-length", served.body.len().to_string());
    response
}

/// The plane as the router mounts it: a service's own traffic is no modelled operation — no
/// routing key names it — so it is claimed by `Service::can_handle` ([`claims_parts`]) and
/// served here.
///
/// It declares NO auth scheme, because a client of the name a service publishes carries no AWS
/// credential and has none to sign with; it still claims only a request addressed to a service
/// this store holds. Being an operation is what hands the plane the in-process transport a
/// `forward` to a `LAMBDA` target group invokes the function over
/// ([`super::lambda_target`]).
pub struct DataPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a request served
    /// here sees exactly the configuration the control plane persisted.
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> DataPlane<Eng> {
    /// Mount the plane over the shared engine.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Whether this plane serves `req`, by the one rule the claim uses
    /// ([`claims_request`]).
    pub fn claims(&self, req: &RtRequest) -> bool {
        claims_request(&*self.engine, req)
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for DataPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { serve(&*self.engine, req, ctx).await })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // A client that sends traffic to the name a service published presents no AWS
        // credential: this plane serves the request however it arrived.
        Some(&[])
    }
}

#[cfg(test)]
mod tests {
    //! The pure rules of this module: which request a rule's match is satisfied by, how a
    //! prefixed address is read, and which target group a weighted forward sends a request to.
    //! What the plane SERVES is pinned end to end in
    //! `core/router/tests/all/vpc_lattice_lambda_target_data_plane.rs`.

    use super::*;

    /// A request for the rows below.
    fn request(method: &str, path: &str, headers: &[(&str, &str)]) -> DataPlaneRequest {
        DataPlaneRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    /// One `httpMatch` with the criteria a row configures.
    fn http_match(
        method: Option<&str>,
        path: Option<PathMatchType>,
        case_sensitive: Option<bool>,
        headers: Vec<HeaderMatch>,
    ) -> RuleMatch {
        RuleMatch::httpMatch(HttpMatch {
            method: method.map(ToString::to_string),
            path_match: path.map(|matched| crate::ops::types::PathMatch {
                r#match: matched,
                case_sensitive,
            }),
            header_matches: Some(headers),
        })
    }

    /// RULE: a request satisfies a rule's match when it satisfies every criterion the match
    /// configures, and a criterion the match leaves out constrains nothing.
    ///
    /// The rows span what the rule generalises over: the criteria a match may carry (none, a
    /// method, an exact path, a path prefix, a header match of each of the three kinds, and
    /// several criteria together), the case the request and the rule write a value in, and
    /// the requests each criterion admits and refuses.
    #[test]
    fn a_request_matches_a_rule_when_it_satisfies_every_criterion_it_configures() {
        let header = |name: &str, matched: HeaderMatchType, case_sensitive: Option<bool>| {
            HeaderMatch {
                name: name.to_string(),
                r#match: matched,
                case_sensitive,
            }
        };
        let rows: Vec<(&str, RuleMatch, DataPlaneRequest, bool)> = vec![
            (
                "a match with no criterion admits every request",
                http_match(None, None, None, Vec::new()),
                request("GET", "/anything", &[]),
                true,
            ),
            (
                "the method the match names",
                http_match(Some("POST"), None, None, Vec::new()),
                request("POST", "/", &[]),
                true,
            ),
            (
                "another method",
                http_match(Some("POST"), None, None, Vec::new()),
                request("GET", "/", &[]),
                false,
            ),
            (
                "an exact path",
                http_match(None, Some(PathMatchType::Exact("/orders".into())), None, Vec::new()),
                request("GET", "/orders", &[]),
                true,
            ),
            (
                "an exact path the request extends",
                http_match(None, Some(PathMatchType::Exact("/orders".into())), None, Vec::new()),
                request("GET", "/orders/7", &[]),
                false,
            ),
            (
                "a path prefix",
                http_match(None, Some(PathMatchType::Prefix("/orders".into())), None, Vec::new()),
                request("GET", "/orders/7", &[]),
                true,
            ),
            (
                "the root prefix every path carries",
                http_match(None, Some(PathMatchType::Prefix("/".into())), None, Vec::new()),
                request("GET", "/deeply/nested", &[]),
                true,
            ),
            (
                "a path in another case, matched without regard to case by default",
                http_match(None, Some(PathMatchType::Exact("/Orders".into())), None, Vec::new()),
                request("GET", "/orders", &[]),
                true,
            ),
            (
                "a path in another case under a case-sensitive match",
                http_match(
                    None,
                    Some(PathMatchType::Exact("/Orders".into())),
                    Some(true),
                    Vec::new(),
                ),
                request("GET", "/orders", &[]),
                false,
            ),
            (
                "an exact header match",
                http_match(
                    None,
                    None,
                    None,
                    vec![header("x-shelf", HeaderMatchType::Exact("front".into()), None)],
                ),
                request("GET", "/", &[("X-Shelf", "front")]),
                true,
            ),
            (
                "an exact header match the request does not carry",
                http_match(
                    None,
                    None,
                    None,
                    vec![header("x-shelf", HeaderMatchType::Exact("front".into()), None)],
                ),
                request("GET", "/", &[]),
                false,
            ),
            (
                "a header prefix match",
                http_match(
                    None,
                    None,
                    None,
                    vec![header("x-shelf", HeaderMatchType::Prefix("fro".into()), None)],
                ),
                request("GET", "/", &[("x-shelf", "front")]),
                true,
            ),
            (
                "a header contains match",
                http_match(
                    None,
                    None,
                    None,
                    vec![header("x-shelf", HeaderMatchType::Contains("ron".into()), None)],
                ),
                request("GET", "/", &[("x-shelf", "front")]),
                true,
            ),
            (
                "a case-sensitive header match in another case",
                http_match(
                    None,
                    None,
                    None,
                    vec![header("x-shelf", HeaderMatchType::Exact("FRONT".into()), Some(true))],
                ),
                request("GET", "/", &[("x-shelf", "front")]),
                false,
            ),
            (
                "every criterion together",
                http_match(
                    Some("PUT"),
                    Some(PathMatchType::Prefix("/orders".into())),
                    None,
                    vec![header("x-shelf", HeaderMatchType::Exact("front".into()), None)],
                ),
                request("PUT", "/orders/7", &[("x-shelf", "front")]),
                true,
            ),
            (
                "every criterion together, one of them unmet",
                http_match(
                    Some("PUT"),
                    Some(PathMatchType::Prefix("/orders".into())),
                    None,
                    vec![header("x-shelf", HeaderMatchType::Exact("front".into()), None)],
                ),
                request("PUT", "/orders/7", &[("x-shelf", "back")]),
                false,
            ),
        ];
        for (case, matched, request, expected) in rows {
            let satisfied = rule_matches(&matched, &request);
            assert_eq!(
                satisfied, expected,
                "case {case:?}: expected the match to be satisfied {expected:?}, got \
                 {satisfied:?}"
            );
        }
    }

    /// RULE: a prefixed address names this plane, the service and the request path; a path
    /// that is not one names no service here.
    ///
    /// The rows span what the reading generalises over: the service identifier forms a client
    /// writes behind the prefix, a request path of several segments, an address with no path
    /// of its own, another plane's prefix, and a path outside the reserved namespace.
    #[test]
    fn a_prefixed_address_names_the_plane_the_service_and_the_path() {
        /// One row: the path a client wrote, and the service and path it names here.
        struct Row {
            case: &'static str,
            path: &'static str,
            named: Option<(&'static str, &'static str)>,
        }
        let rows = [
            Row {
                case: "the service's id and the root path",
                path: "/_aws/vpc-lattice/svc-0123456789abcdef0/",
                named: Some(("svc-0123456789abcdef0", "/")),
            },
            Row {
                case: "an announced label and a nested path",
                path: "/_aws/vpc-lattice/shop-0123456789abcdef0/orders/7",
                named: Some(("shop-0123456789abcdef0", "/orders/7")),
            },
            Row {
                case: "no path of its own",
                path: "/_aws/vpc-lattice/svc-0123456789abcdef0",
                named: Some(("svc-0123456789abcdef0", "/")),
            },
            Row {
                case: "another plane behind the namespace",
                path: "/_aws/elasticloadbalancing/alb/",
                named: None,
            },
            Row {
                case: "the namespace with no plane",
                path: "/_aws/",
                named: None,
            },
            Row {
                case: "a path outside the namespace",
                path: "/orders/7",
                named: None,
            },
        ];
        for row in rows {
            let read = prefixed_addressing(row.path);
            assert_eq!(
                read, row.named,
                "case {:?}: expected {:?} for {:?}, got {read:?}",
                row.case, row.named, row.path
            );
        }
    }
}
