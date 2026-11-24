// SPDX-License-Identifier: CC0-1.0

//! The GRAPH DATA PLANE's front door: how a request addressed to a graph's own endpoint
//! is answered when that address resolves to no graph at all.
//!
//! # The rule
//!
//! Five operations of this service are reached not at the service endpoint but at the
//! GRAPH's own one — each carries `@endpoint hostPrefix "{graphIdentifier}."`
//! (`smithy_query(query="operation",
//! operation="ExecuteQuery,GetGraphSummary,ListQueries,GetQuery,CancelQuery")` ->
//! endpoint) — and a host prefix naming a graph the account does not hold in the Region
//! the request was signed for addresses no endpoint: there is nothing there for the
//! request's signature to be accepted against. Such a call is therefore refused as an
//! AUTHENTICATION failure, [`ENDPOINT_AUTHENTICATION_STATUS`] with the error code
//! [`ENDPOINT_AUTHENTICATION_CODE`] and the one fixed sentence
//! [`ENDPOINT_AUTHENTICATION_MESSAGE`], carried in the data plane's own error envelope
//! ([`unresolved_endpoint_response`]) — never as the control-plane "no such resource",
//! which is the answer of an operation addressed by a `graphIdentifier` the request
//! carries in its BODY or its URI and which reaches the service's own endpoint.
//!
//! The decision is taken on the ADDRESS ALONE: it does not depend on which operation was
//! attempted, on the query text or language, on the query id, or on any other member — so
//! every operation on that endpoint answers it identically, and it is reached ahead of
//! every decision the operation behind the door takes (its own authorization decision,
//! the arguments it accepts, and the store lookups it makes).
//!
//! # Why a gate rather than the handlers
//!
//! Two reasons. The refusal is the ADDRESS's, not the operation's, so placing it in each
//! handler would make it the last decision of each rather than the first, and a request
//! that broke some other rule of the operation would be told about that rule by an
//! endpoint that does not exist. And the envelope carries more than a modelled error can:
//! the model's `AccessDeniedException` declares only `message`
//! (`smithy_query(query="shape",shape="AccessDeniedException")`), while the data plane's
//! refusal also names the error class in `code` and the response in `requestId`, which no
//! handler's error channel can carry.
//!
//! Nothing else is re-implemented. The constraint refusal every operation of this service
//! is served behind ([`super::constraints`]) is answered here first, for the same reason
//! it exists — the modelled `ValidationException` carries a `reason` the framework's
//! refusal channel cannot — and because a value the model's own `@pattern` refuses is not
//! an address to resolve at all. A request this door does not refuse is handed to the
//! operation it wraps, whose [`ErasedOperation::run`] owns deserialize -> validate ->
//! handle -> serialize, telemetry and every modelled error.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{BoxFuture, ErasedOperation, Operation, OperationContext, Service};
use std::sync::Arc;

/// The header the model binds `graphIdentifier` to for every operation on this endpoint
/// (`httpHeader: graphIdentifier`, `hostLabel: true`).
///
/// The host prefix a client dials is built from that same member, so this header is the
/// address the request STATES, and it is all the front door reads: it never looks at the
/// body.
const GRAPH_IDENTIFIER_HEADER: &str = "graphIdentifier";

/// The HTTP status of the refusal: the CALLER's credentials were not accepted at the
/// address the request was sent to, which is neither a bad request (400) nor a missing
/// resource (404).
pub const ENDPOINT_AUTHENTICATION_STATUS: u16 = 403;

/// The error class that refusal names, in `x-amzn-errortype` — the header `restJson1`
/// resolves an error shape from — and in the `code` member of the body. The service's
/// authentication and authorization failures are one class
/// (`smithy_query(query="shape",shape="AccessDeniedException")`: "Raised in case of an
/// authentication or authorization failure", `@httpError 403`).
pub const ENDPOINT_AUTHENTICATION_CODE: &str = "AccessDeniedException";

/// The whole sentence that refusal states. Wholly FIXED text with no variable part: it
/// names neither the identifier the request carried nor the account it was resolved in, so
/// a caller refused at the door learns nothing about what the account holds.
pub const ENDPOINT_AUTHENTICATION_MESSAGE: &str = "Authentication Failed";

/// The member the error class travels under in the body, beside the header: what a client
/// of this data plane branches on.
const CODE_MEMBER: &str = "code";

/// The member naming the response, for correlating one answer with a service-side record
/// of it.
const REQUEST_ID_MEMBER: &str = "requestId";

/// The member the sentence travels under.
const MESSAGE_MEMBER: &str = "message";

/// The media type a `restJson1` body is rendered in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The data plane's refusal of a request addressed to a graph endpoint that does not
/// resolve: the status, the error class in the header a client resolves an error shape
/// from, and the envelope this data plane states a failure in — the class in `code`, the
/// id of THIS response in `requestId`, and the one fixed sentence in `message`.
///
/// `requestId` is minted per response (`service_common`'s one minting of the id most AWS
/// services report, a fresh v4 UUID), so two refusals never name the same request and the
/// value is derived from nothing the caller sent. It needs no state, which is what lets a
/// request refused before any store is read still carry one.
///
/// Built here rather than through the framework's error renderer because that renderer
/// serializes a MODELLED shape, and the two members beside `message` are not modelled on
/// any shape of this service.
#[must_use]
pub fn unresolved_endpoint_response() -> RtResponse {
    let body = ::serde_json::json!({
        CODE_MEMBER: ENDPOINT_AUTHENTICATION_CODE,
        REQUEST_ID_MEMBER: ::service_common::query_error_envelope::new_request_id(),
        MESSAGE_MEMBER: ENDPOINT_AUTHENTICATION_MESSAGE,
    });
    let status = StatusCode::try_from(ENDPOINT_AUTHENTICATION_STATUS)
        .expect("the refusal's status is a constant");
    let mut response = RtResponse::new(status, SdkBody::from(body.to_string()));
    let headers = response.headers_mut();
    headers.insert("content-type", JSON_CONTENT_TYPE);
    headers.insert("x-amzn-errortype", ENDPOINT_AUTHENTICATION_CODE);
    response
}

/// One operation reached at a graph's OWN endpoint, served so the address is resolved
/// before anything else about the request is decided.
///
/// Generic over what it wraps, which is the operation as the service otherwise serves it,
/// and over the engine the address is resolved against: `crate::service` wires both from
/// the one shared engine, so the store this door reads is the store the handler behind it
/// reads.
pub struct GraphEndpointGate<Eng, Inner> {
    engine: Arc<Eng>,
    operation: Inner,
}

impl<Eng, Inner> GraphEndpointGate<Eng, Inner> {
    /// Put the door in front of `operation` — already constructed from the shared engine
    /// — resolving addresses against that same `engine`.
    pub fn over(engine: Arc<Eng>, operation: Inner) -> Self {
        Self { engine, operation }
    }
}

impl<Eng: Send + Sync + 'static, Inner: Operation + ErasedOperation> ErasedOperation
    for GraphEndpointGate<Eng, Inner>
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The service definition is read, used and dropped inside each of the two
            // statements below: `&dyn ErasedServiceDefinition` is not `Sync`, so holding
            // it across the await would make this future non-`Send`.
            let refused =
                super::constraints::constraint_violations::<Inner>(svc.definition().protocol(), req);
            if let Some(response) = super::constraints::constraint_refusal::<Inner>(svc, req, &refused)
            {
                return response;
            }
            if self.addresses_no_graph_endpoint(req, ctx) {
                ::tracing::info!(
                    operation = Inner::NAME,
                    outcome = ENDPOINT_AUTHENTICATION_CODE,
                    status = ENDPOINT_AUTHENTICATION_STATUS,
                    "refused a request addressed to a graph endpoint that resolves to no graph"
                );
                return unresolved_endpoint_response();
            }
            self.operation.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

impl<Eng: Send + Sync + 'static, Inner> GraphEndpointGate<Eng, Inner> {
    /// Whether the host prefix this request states resolves to NO graph of the Region it
    /// was signed for — the one fact the door decides on.
    ///
    /// `false` for a request that states no address at all, which is the operation's own
    /// refusal to word, and `false` when this engine carries no store of this service,
    /// which the operation reports as the server fault it is: the door refuses a request
    /// only when the store it read answers that there is nothing at that address.
    ///
    /// The store is taken and released HERE, before the operation behind the door takes it
    /// again: the guard is a plain mutex guard and the handler takes its own. The one write
    /// the lookup makes is materialising the transitions the clock had already made due,
    /// which were owed whether or not anybody called
    /// (`crate::state::ComAmazonawsNeptunegraphAmazonneptunegraphState::settled_graph`).
    fn addresses_no_graph_endpoint(
        &self,
        req: &RtRequest,
        ctx: &OperationContext<'_>,
    ) -> bool {
        let Some(identifier) = req.headers().get(GRAPH_IDENTIFIER_HEADER) else {
            return false;
        };
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return false;
        };
        // A graph lives in exactly one Region, so the Region the request was signed for is
        // part of the address: a graph of another Region is not behind this host prefix.
        let region = crate::state::request_region(ctx);
        let (mut store, now) = crate::state::locked_store_now(&state);
        store.settled_graph(identifier, &region, now).is_none()
    }
}
