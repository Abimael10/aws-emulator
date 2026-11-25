// SPDX-License-Identifier: CC0-1.0

//! How this service NAMES, in the `x-amzn-errortype` response header, the error shape a
//! refused call is refused with — and the one place that naming is composed with the other
//! response header this service's wire completes.
//!
//! A restJson1 client resolves the error shape from that header, and the framework's
//! renderers write the shape's bare name into it. What the wire of this service family
//! carries is the QUALIFIED form: the shape's name, the separator, and then the locator of
//! the namespace that declares the shape, under the service stack's locator root —
//!
//! ```text
//! HTTP/1.1 404 Not Found
//! x-amzn-errortype: ResourceNotFoundException:http://internal.amazon.com/coral/com.amazonaws.docdbelastic/
//!
//! {"__type":"ResourceNotFoundException","message":"Resource was not found", …}
//! ```
//!
//! The locator is not part of the shape's NAME — every SDK resolves the shape from the
//! LEADING token, the text before the separator (and after a `#`, for a namespaced name), so
//! nothing a client resolves changes, and the body keeps naming the bare shape in `__type` —
//! but the value this service's wire carries is the qualified one, and a client that reads or
//! validates the whole value sees it.
//!
//! The form, the writer and both wrappers are [`service_common::error_type_locator`]'s, which
//! is where this wire family's one rule for the header lives: the locator is DERIVED from this
//! service's own generated identity
//! (`com.amazonaws.docdbelastic#ChimeraDbLionfishServiceLambda` carries the namespace that
//! declares this service's shapes), so nothing about the value is written out here, a model
//! moved to another namespace follows with no edit, and no part of it comes from the request
//! or from state.
//!
//! What this module owns is the COMPOSITION, which the shared wrappers cannot do for it:
//! this service completes a second response header of its own — the media type a JSON payload
//! is announced with ([`super::json_media_type`]) — and both have to be completed at the two
//! layers every response of this service passes through, or one operation's success and
//! another's refusal disagree about one request:
//!
//! * [`NamingErrorShape`], the wrapper [`crate::service::EmulatedService`] hands the router
//!   from `Service::override_operation`, covers everything an operation answers: a success
//!   (whose media type is announced, and which names no error shape), a modelled error, and a
//!   constraint refusal raised while that operation was being served;
//! * [`ErrorTypeHeaderDefinition`], the definition wrapper `Service::definition` hands back,
//!   covers a response NO operation produced: a routing failure, a transport failure detected
//!   up front, and a pre-dispatch refusal.
//!
//! Each layer is the shared qualifying wrapper with this service's media type announced over
//! it. Nothing else about a response changes: the status, the body and every other header stay
//! as the renderer wrote them, and a success keeps every byte.

use super::json_media_type::{announced_media_type, with_json_media_type};
use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, OperationRoutingTable,
    Refused, Service, ServerHttpProtocol, ServiceIdentity,
};

/// The shared per-operation wrapper that qualifies the error shape of whatever an operation
/// answered, for every operation of a service.
type Qualifying = ::service_common::error_type_locator::QualifiedErrorTypeOperation;

/// The shared definition wrapper that qualifies the error shape of every framework-level
/// response a generated definition renders.
type QualifyingDefinition<D> = ::service_common::error_type_locator::QualifiedErrorTypeDefinition<D>;

/// `resp` with the media type this service announces a JSON payload to `req` with.
///
/// The error shape has already been named by the shared wrapper this sits over, so this is the
/// second half of the completion and the two are applied together at each layer below.
fn announced(resp: RtResponse, req: &RtRequest) -> RtResponse {
    with_json_media_type(resp, &announced_media_type(req))
}

/// Serves the modelled operation the service's routing table names for a request exactly as
/// that table would, naming the error shape of whatever it answered (the shared wrapper) and
/// announcing the media type of the payload it answered with.
///
/// It stores no operation of its own and makes no routing decision: the operation served is
/// the one the service's own table names for the request, so the framework keeps owning the
/// whole deserialize -> validate -> handle -> serialize sequence and every status, shape and
/// message it answers with. One wrapper serves every operation of this service, and it is the
/// one place each of their responses passes through.
pub struct NamingErrorShape(Qualifying);

impl NamingErrorShape {
    /// Serve every operation of this service, qualifying its failures with `locator` — the one
    /// [`ErrorTypeHeaderDefinition`] names, so the two layers cannot disagree.
    pub fn new(locator: String) -> Self {
        Self(Qualifying::new(locator))
    }
}

impl ErasedOperation for NamingErrorShape {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { announced(self.0.run(svc, req, ctx).await, req) })
    }

    /// Whatever the shared wrapper defers to, which is "the modelled operation the routing key
    /// names": its model-declared schemes decide, and wrapping cannot weaken them.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.0.auth_schemes()
    }
}

/// This service's model-derived half with its framework-level error renderers wrapped, so a
/// response NO operation produced carries the same two completed headers as one an operation
/// produced.
///
/// Identity, the routing table and the protocol come from the wrapped definition untouched:
/// nothing model-derived is restated here.
pub struct ErrorTypeHeaderDefinition<D>(QualifyingDefinition<D>);

impl<D: ErasedServiceDefinition> ErrorTypeHeaderDefinition<D> {
    /// Wrap `inner`, completing both headers of every framework-level response it renders.
    pub fn new(inner: D) -> Self {
        Self(QualifyingDefinition::new(inner))
    }

    /// Whether the generated routing table can serve this request — the condition under
    /// which this service claims it for [`NamingErrorShape`], so a request naming no
    /// modelled operation stays the framework's routing failure.
    pub fn routes(&self, req: &RtRequest) -> bool {
        self.0.routes(req)
    }

    /// The locator every layer of this service qualifies an error shape with, derived once
    /// from this service's own identity.
    pub fn locator(&self) -> &str {
        self.0.locator()
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for ErrorTypeHeaderDefinition<D> {
    fn identity(&self) -> &ServiceIdentity {
        self.0.identity()
    }

    fn protocol(&self) -> &dyn ServerHttpProtocol {
        self.0.protocol()
    }

    fn routing_table(&self) -> &OperationRoutingTable {
        self.0.routing_table()
    }

    fn routing_error_response(&self, req: &RtRequest) -> RtResponse {
        announced(self.0.routing_error_response(req), req)
    }

    fn framework_error_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        req: &RtRequest,
    ) -> RtResponse {
        announced(self.0.framework_error_response(error, req), req)
    }

    fn refusal_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        chosen: Option<Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        announced(self.0.refusal_response(error, chosen, req), req)
    }

    fn wire_protocol(&self, req: &RtRequest) -> smithy_server_runtime::WireProtocol<'_> {
        self.0.wire_protocol(req)
    }
}
