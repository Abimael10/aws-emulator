// SPDX-License-Identifier: CC0-1.0

//! The routes of this API the on-disk model does not describe, served out of the records
//! this service holds: a domain's cross-cluster REPLICATION family and the CONFIGURATION
//! COMPARISON of two domains.
//!
//! # Why they are here and not in `src/ops/`
//!
//! `src/ops/` holds one file per MODELLED operation, and the generated routing table is
//! built from the model: `smithy_query(query="operation",
//! operation="AddReplicationPeer,RemoveReplicationPeer,FailoverReplication,SwitchoverReplication,DescribeDomainReplication,CompareDomainConfiguration")`
//! answers "not found" for all six, so none of them can be named by that table and none of
//! them has a modelled input, output or error shape. They are nevertheless operations of
//! this API that AWS serves — a client calls them on the same endpoint, under the same
//! signing name, with their paths bound under this model's own `/2021-01-01` plane — so
//! without them the front door answered them from its catch-all: `501 NotImplemented`,
//! which is a code no AWS model declares, on a class of request that is a permanent CLIENT
//! error.
//!
//! They are therefore served the way this crate serves every other request the model's
//! routing key cannot name ([`crate::service::domain_rest`], the analogous plane in
//! `service-com-amazonaws-rolesanywhere-rolesanywhere`'s `create_session`): the route is
//! recognised here and answered as ONE [`ErasedOperation`] the service holds, which the
//! `override_operation` seam names. Being an operation rather than a routing-error answer
//! is what makes the router verify the SigV4 credential these calls carry
//! ([`UnmodelledRoutes::auth_schemes`]) before anything is read or written.
//!
//! # What their contract is
//!
//! RECORDED BEHAVIOUR ONLY. The status, the error code and the message of every refusal
//! below are the ones the recorded wire answers for that route, and nothing is taken from a
//! single recording's own values: a message's variable parts are the domain name the
//! request carries, the ARN composed from the request's own Region and this account, and
//! the value the request sent. Where a refusal class is one the rest of this service
//! already words — an absent required member, a value outside a modelled constraint — the
//! words are the ones [`crate::service::validation`] states for every other operation, so a
//! caller cannot tell these routes' refusals from a modelled operation's.
//!
//! Each route's own rule lives beside it: [`replication`] for the five calls on a domain's
//! replication arrangement, [`configuration_comparison`] for the comparison of two domains'
//! configuration.

use std::sync::Arc;
use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::{json, Value};
use ::smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

/// The comparison of two domains' configuration, per category.
pub mod configuration_comparison;
/// The `DomainName` label these routes carry, held to the constraints the model declares
/// for it.
pub mod domain_name;
/// The five calls on one domain's cross-cluster replication arrangement.
pub mod replication;

/// The API version segment every path of this model is bound under, which these routes are
/// bound under too: it is what tells this API's surface from the legacy Elasticsearch
/// Service API sharing the `es` signing name (`crate::service`'s claim).
const API_VERSION: &str = "2021-01-01";

/// The plane segment the domain and comparison paths sit under.
const PLANE: &str = "opensearch";

/// The collection segment a domain's own paths sit under.
const DOMAIN: &str = "domain";

/// The segment a domain's replication arrangement is addressed at.
const REPLICATION: &str = "replication";

/// The largest request body these routes read.
///
/// The emulator's OWN bound, like [`crate::service::domain_rest`]'s: one of these bodies
/// carries two ARNs, a category list and two flags, so this ceiling sits far above any such
/// document and bounds the work one request can ask for before it is parsed.
const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;

/// Which of these routes a request addresses.
///
/// The domain name is the one the PATH carries, percent-decoded as the framework decodes a
/// `@httpLabel`: it is held to the constraints the model declares for a `DomainName`
/// ([`domain_name`]) rather than trusted, exactly as a modelled route's label is.
#[derive(Debug)]
pub enum Route {
    /// `GET /2021-01-01/opensearch/domain/{DomainName}/replication`
    DescribeDomainReplication(String),
    /// `POST /2021-01-01/opensearch/domain/{DomainName}/replication/addPeer`
    AddReplicationPeer(String),
    /// `POST /2021-01-01/opensearch/domain/{DomainName}/replication/removePeer`
    RemoveReplicationPeer(String),
    /// `POST /2021-01-01/opensearch/domain/{DomainName}/replication/failover`
    FailoverReplication(String),
    /// `POST /2021-01-01/opensearch/domain/{DomainName}/replication/switchover`
    SwitchoverReplication(String),
    /// `POST /2021-01-01/opensearch/compareDomainConfiguration`
    CompareDomainConfiguration,
}

/// The route the request target `uri` addresses under `method`, or `None` when it addresses
/// none of them — which leaves the request to the answer it would otherwise get.
///
/// Matched on the WHOLE path by exact segments, so no path that merely shares a prefix with
/// one of these is claimed.
#[must_use]
pub fn route(method: &str, uri: &str) -> Option<Route> {
    let parsed = uri.parse::<http::Uri>().unwrap_or_default();
    let segments: Vec<&str> = parsed
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect::<Vec<_>>();
    let method = method.to_ascii_uppercase();
    match segments.as_slice() {
        [API_VERSION, PLANE, "compareDomainConfiguration"] if method == "POST" => {
            Some(Route::CompareDomainConfiguration)
        }
        [API_VERSION, PLANE, DOMAIN, named, REPLICATION] if method == "GET" => {
            Some(Route::DescribeDomainReplication(label(named)?))
        }
        [API_VERSION, PLANE, DOMAIN, named, REPLICATION, action] if method == "POST" => {
            let named = label(named)?;
            match *action {
                "addPeer" => Some(Route::AddReplicationPeer(named)),
                "removePeer" => Some(Route::RemoveReplicationPeer(named)),
                "failover" => Some(Route::FailoverReplication(named)),
                "switchover" => Some(Route::SwitchoverReplication(named)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// One path segment as the value the label it binds carries: percent-decoded, which is how
/// the framework reads a `@httpLabel` of a modelled route (the decoding is
/// `percent-encoding`'s, the crate this service already reads an escaped request value
/// with). `None` for an empty segment, which names no resource, and for one whose escapes
/// are not text.
fn label(segment: &str) -> Option<String> {
    let decoded = ::percent_encoding::percent_decode_str(segment)
        .decode_utf8()
        .ok()?
        .into_owned();
    (!decoded.is_empty()).then_some(decoded)
}

/// How one of these routes refuses a request: the status, the error code a client branches
/// on, and the sentence.
///
/// One type for all of them, because what differs between the routes is WHICH refusal a
/// condition earns, not how a refusal reaches the wire.
#[derive(Debug)]
pub struct Refusal {
    status: u16,
    code: &'static str,
    message: String,
}

impl Refusal {
    /// The request itself is wrong: a member it must carry is absent, a value is outside a
    /// constraint, or a value names something the request cannot name. `400`, the status
    /// and code every other refused input of this service answers on.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: VALIDATION_EXCEPTION,
            message: message.into(),
        }
    }

    /// The request names no resource this account holds. The status is the one this service
    /// answers EVERY missing-resource refusal on
    /// ([`crate::scaffolded::RESOURCE_NOT_FOUND_STATUS`]), so a client branching on it
    /// takes the same branch here as on a modelled operation.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: crate::scaffolded::RESOURCE_NOT_FOUND_STATUS,
            code: RESOURCE_NOT_FOUND_EXCEPTION,
            message: message.into(),
        }
    }

    /// The emulator cannot reach this service's store at all — the one answer that is not
    /// about the request. The scaffold's 501 and its wording, for the reason
    /// [`crate::state::state_unavailable`] gives: no engine of this process will serve the
    /// call, so there is nothing to retry.
    #[must_use]
    fn unavailable() -> Self {
        Self {
            status: 501,
            code: "NotImplemented",
            message: crate::state::STATE_UNAVAILABLE_MESSAGE.to_string(),
        }
    }
}

/// The error code every refused input of this service travels under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The error code its missing-resource refusals travel under.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// These routes, as the ONE [`ErasedOperation`] the service holds for them.
///
/// Owns the shared engine exactly as a generated operation does, so every answer is
/// computed from the SAME per-service store the modelled operations write: the domain a
/// `CreateDomain` minted is the domain these routes resolve, and an arrangement one of them
/// establishes is released when `DeleteDomain` releases the domain.
pub struct UnmodelledRoutes<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> UnmodelledRoutes<Eng> {
    /// Build the plane from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for UnmodelledRoutes<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // Unreachable for anything else: `override_operation` names this only for a
            // request [`route`] resolves, and the same reading resolves it here.
            let Some(route) = route(req.method(), req.uri()) else {
                return svc.definition().routing_error_response(req);
            };
            answered(served(&*self.engine, &route, req, ctx))
        })
    }

    /// SigV4, the only scheme this service's model declares, and the scheme a client signs
    /// one of these calls with: naming it is what makes the router verify the credential
    /// the request carries, as it does for a modelled operation. There is no modelled
    /// operation here for the router to read a scheme off, so without this a signed request
    /// to one of these routes would be served unverified.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(AUTH_SCHEMES)
    }
}

/// See [`UnmodelledRoutes::auth_schemes`].
const AUTH_SCHEMES: &[&str] = &[::emulator_server::aws_credentials::SIGV4_SCHEME];

/// What one of these routes answers `req` with: the document the route serves, or the
/// refusal it earns.
fn served<Eng: Send + Sync + 'static>(
    engine: &Eng,
    route: &Route,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> Result<Value, Refusal> {
    // The Region the request was signed for, which is what the ARNs these routes name and
    // refuse on belong to — the one reading of it every operation of this service uses.
    let region = crate::state::request_region(ctx.parts());
    let asked = asked_members(req.body().bytes().unwrap_or_default())?;
    let now = SystemTime::now();
    match route {
        Route::CompareDomainConfiguration => configuration_comparison::served(engine, &asked),
        Route::DescribeDomainReplication(named) => {
            replication::described(engine, named, &region, now)
        }
        Route::AddReplicationPeer(named) => {
            replication::peer_added(engine, named, &region, &asked, now)
        }
        Route::RemoveReplicationPeer(named) => {
            replication::peer_removed(engine, named, &region, &asked, now)
        }
        // The unplanned promotion and the planned one take the same gates and move the same
        // roles; see [`replication::promoted`].
        Route::FailoverReplication(named) | Route::SwitchoverReplication(named) => {
            replication::promoted(engine, named, &region, now)
        }
    }
}

/// The members `body` carries, as the JSON object a `restJson1` request body is.
///
/// A body that is EMPTY carries no member, which is what a request to a route taking no
/// member sends; a member a route requires and the body does not carry is then refused as
/// absent, by that route's own rule. A body that is not a JSON object carries no member
/// this surface can read either — including one whose nesting is past what the parser
/// admits, which the maintained parser refuses rather than recursing into.
fn asked_members(body: &[u8]) -> Result<Value, Refusal> {
    if body.len() > MAX_REQUEST_BODY_BYTES {
        return Err(Refusal::validation(format!(
            "Request body is larger than the maximum of {MAX_REQUEST_BODY_BYTES} bytes this \
             service reads"
        )));
    }
    Ok(::serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({})))
}

/// One answer on this wire: the served document on `200`, or the refusal's own status, error
/// code and sentence.
fn answered(served: Result<Value, Refusal>) -> RtResponse {
    let (status, code, body) = match served {
        Ok(document) => (200u16, None, document),
        // `restJson1` carries a failure's error SHAPE in `x-amzn-errortype` and the shape's
        // own members in the body, so the body carries the message and nothing else — no
        // discriminator member, which belongs to the AWS JSON protocol family
        // (`service_common::rest_json_error_body` states the rule for the fleet).
        Err(refusal) => (
            refusal.status,
            Some(refusal.code),
            json!({ "message": refusal.message }),
        ),
    };
    let rendered = ::serde_json::to_vec(&body).unwrap_or_default();
    let length = rendered.len();
    let mut response = RtResponse::new(
        StatusCode::try_from(status).unwrap_or_else(|_| {
            // Unreachable: every status above is a constant of this module. A status this
            // emulator cannot build answers as a server fault rather than panicking.
            StatusCode::try_from(500).expect("500 is a status")
        }),
        SdkBody::from(rendered),
    );
    let headers = response.headers_mut();
    headers.insert("content-type", "application/json");
    headers.insert("content-length", length.to_string());
    if let Some(code) = code {
        headers.insert("x-amzn-errortype", code);
    }
    response
}

/// The text of one required string member of the request body, or the refusal its absence
/// earns.
///
/// `member` is the member as a request SPELLS it; the clause names it the way every other
/// refusal of this service names a member — lowerCamel, the casing the framework's own
/// validation path applies ([`::aws_smithy_schema_server::constraints::path::member`]).
fn required_text(asked: &Value, member: &str) -> Result<String, Refusal> {
    match asked.get(member).and_then(Value::as_str) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(Refusal::validation(absent_member_report(member))),
    }
}

/// The `ValidationException` message an absent required member earns, worded by the one
/// wording every refused input of this service goes through.
fn absent_member_report(member: &str) -> String {
    let path = ::aws_smithy_schema_server::constraints::path::member(member);
    let violation = ::aws_smithy_schema_server::Violation {
        path: path.clone(),
        kind: ::aws_smithy_schema_server::ViolationKind::MissingRequiredMember,
        value: None,
        sensitive: false,
    };
    crate::service::validation::stated_report(std::slice::from_ref(&violation))
        .map_or_else(|| violation.message(), |refused| refused.message)
}
