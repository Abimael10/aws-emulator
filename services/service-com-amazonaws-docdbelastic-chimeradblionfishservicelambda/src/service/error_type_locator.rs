// SPDX-License-Identifier: CC0-1.0

//! How this service NAMES, in the `x-amzn-errortype` response header, the error shape a
//! refused call is refused with — and why that name carries no locator beside it.
//!
//! A restJson1 client resolves the error shape from that header, and what this service's
//! wire carries is the shape's NAME, as the model declares it, and nothing else:
//!
//! ```text
//! HTTP/1.1 404 Not Found
//! x-amzn-errortype: ResourceNotFoundException
//!
//! {"__type":"ResourceNotFoundException","message":"Resource was not found", …}
//! ```
//!
//! Nothing is appended to that name: not the namespace that declares the shape, and not a
//! locator for it inside whatever stack serves the request. A locator is an internal
//! detail of where a shape is declared — it is not part of the name a client resolves, no
//! trait of this model states it, and a response a client reads does not carry it, so a
//! value carrying one is a value no client can compare to the error it names. The name the
//! framework's restJson1 renderers write is therefore exactly the value this service
//! answers with, and the body keeps naming that same shape in `__type`.
//!
//! The value is model-derived end to end: it is the error shape's own name, so an error
//! shape renamed in the model renames it here with no edit, and no part of it comes from
//! the request or from state.
//!
//! The header belongs to the RESPONSE, not to any modelled shape — no error shape of this
//! model declares it, so no handler can set it — so this module holds the header to that
//! one form at the two layers every response of this service passes through:
//!
//! * [`NamingErrorShape`], the wrapper [`crate::service::EmulatedService`] hands the
//!   router from `Service::override_operation`, covers everything an operation answers: a
//!   success (untouched), a modelled error, and a constraint refusal raised while that
//!   operation was being served;
//! * [`ErrorTypeHeaderDefinition`], the definition wrapper `Service::definition` hands
//!   back, covers a response NO operation produced: a routing failure, a transport failure
//!   detected up front, and a pre-dispatch refusal.
//!
//! Nothing else about a response changes: the status, the body and every other header stay
//! as the renderer wrote them, and a success keeps every byte.
//!
//! Those same two layers are where the other response header this service's wire carries is
//! completed — the media type a JSON payload is announced with
//! ([`super::json_media_type`]) — so [`answered`] composes the two and each layer applies
//! that one function.

use super::json_media_type::{announced_media_type, with_json_media_type};
use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
// The header a restJson1 client resolves the error shape from, and the SHARED reader of the
// shape a value names in it: the shape's own name, with neither the namespace that may
// precede it (`com.amazonaws.docdbelastic#ResourceNotFoundException`) nor a locator appended
// after it. Reading a shape out of every form a value can arrive in is what every service of
// this wire family needs, so this service takes that reader rather than keeping a copy.
use ::service_common::error_type_locator::{header_shape_named, ERROR_TYPE_HEADER};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, OperationRoutingTable,
    Refused, Service, ServerHttpProtocol, ServiceIdentity,
};

/// The lowest HTTP status that is a failure, and so the lowest one that names an error
/// shape at all.
const FIRST_FAILURE_STATUS: u16 = 400;

/// `resp` as this service answers it: a failure names its error shape in
/// [`ERROR_TYPE_HEADER`] by that shape's name alone.
///
/// A success is returned untouched, as is a failure whose renderer named no shape (there is
/// nothing to name, and this invents no header the renderer did not write) and one already
/// naming the bare shape — which is what every renderer of this service writes, so the
/// common path rewrites nothing. IDEMPOTENT: the shape is read back out of whatever the
/// header carries, so the two layers above cannot disagree about one call. A header value
/// the HTTP layer will not carry is left as it was rather than turned into a transport
/// failure, which on an error path would replace a modelled 4xx with a 500 an SDK retries.
pub fn with_error_type_shape_name(mut resp: RtResponse) -> RtResponse {
    if resp.status().as_u16() < FIRST_FAILURE_STATUS {
        return resp;
    }
    let Some(named) = resp.headers().get(ERROR_TYPE_HEADER) else {
        return resp;
    };
    let shape = header_shape_named(named);
    if shape.is_empty() || shape == named {
        return resp;
    }
    let shape = shape.to_string();
    let _ = resp.headers_mut().try_insert(ERROR_TYPE_HEADER, shape);
    resp
}

/// `resp` as this service ANSWERS `req`: the two completions of a response this service
/// owns, applied together at every layer a response passes through.
///
/// Each is its own concern and lives in its own module — the error shape's name here, the
/// media type a JSON payload is announced with in [`super::json_media_type`] — and this is
/// the one place they are composed, so no layer can apply one and forget the other.
fn answered(resp: RtResponse, req: &RtRequest) -> RtResponse {
    with_json_media_type(with_error_type_shape_name(resp), &announced_media_type(req))
}

/// The modelled operation the service's own routing table names for this request, or `None`
/// when the request names none.
fn modeled_operation<'s>(svc: &'s dyn Service, req: &RtRequest) -> Option<&'s dyn ErasedOperation> {
    let definition = svc.definition();
    let key = definition.protocol().routing_key(req).ok()?;
    definition.routing_table().get(key)
}

/// Serves the modelled operation the service's routing table names for a request exactly as
/// that table would, then completes the response headers this service's wire carries (see
/// [`answered`]).
///
/// It stores no operation of its own and makes no routing decision: the operation it
/// delegates to is the one the service's own table names for the request, so the framework
/// keeps owning the whole deserialize -> validate -> handle -> serialize sequence and every
/// status, shape and message it answers with. One wrapper serves every operation of this
/// service, and it is the one place each of their responses passes through.
pub struct NamingErrorShape;

impl ErasedOperation for NamingErrorShape {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            match modeled_operation(svc, req) {
                Some(operation) => answered(operation.run(svc, req, ctx).await, req),
                // Unreachable through a `Service::override_operation` that claims a request
                // only when the routing table holds its key; answered as the framework
                // answers an unroutable request rather than guessed at (and the definition
                // wrapper below names the shape on it there).
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// `None` is "defer to the modelled operation the routing key names" — which is the
    /// operation this wrapper delegates to, so its model-declared schemes decide and
    /// wrapping cannot weaken them.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

/// This service's model-derived half with its framework-level error renderers wrapped, so a
/// response NO operation produced carries the same completed headers (see [`answered`]) as
/// one an operation produced.
///
/// Identity, the routing table and the protocol come from the wrapped definition untouched:
/// nothing model-derived is restated here.
pub struct ErrorTypeHeaderDefinition<D>(D);

impl<D: ErasedServiceDefinition> ErrorTypeHeaderDefinition<D> {
    /// Wrap `inner`, completing the headers of every framework-level response it renders.
    pub fn new(inner: D) -> Self {
        Self(inner)
    }

    /// Whether the generated routing table can serve this request — the condition under
    /// which this service claims it for [`NamingErrorShape`], so a request naming no
    /// modelled operation stays the framework's routing failure.
    pub fn routes(&self, req: &RtRequest) -> bool {
        self.0
            .protocol()
            .routing_key(req)
            .ok()
            .and_then(|key| self.0.routing_table().get(key))
            .is_some()
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
        answered(self.0.routing_error_response(req), req)
    }

    fn framework_error_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        req: &RtRequest,
    ) -> RtResponse {
        answered(self.0.framework_error_response(error, req), req)
    }

    fn refusal_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        chosen: Option<Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        answered(self.0.refusal_response(error, chosen, req), req)
    }

    fn wire_protocol(&self, req: &RtRequest) -> smithy_server_runtime::WireProtocol<'_> {
        self.0.wire_protocol(req)
    }
}

#[cfg(test)]
mod tests {
    //! The header over the forms a response can name its error shape in. The wire
    //! behaviour — that every refusal of every operation names the bare shape — is
    //! asserted through the router in `tests/all/error_type_header_form.rs`.

    use super::*;
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::body::SdkBody;

    /// The namespace that declares this service's shapes, as the generated identity states
    /// the service shape id (`smithy_query(query="service_info")`:
    /// `com.amazonaws.docdbelastic#ChimeraDbLionfishServiceLambda`). Used only to build
    /// the decorated forms a value can arrive in.
    const NAMESPACE: &str = "com.amazonaws.docdbelastic";

    /// One response, built from a status and the error type its renderer named.
    fn response(status: u16, error_type: Option<&str>) -> RtResponse {
        let mut resp = RtResponse::new(
            StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(r#"{"__type":"ResourceNotFoundException"}"#),
        );
        if let Some(error_type) = error_type {
            resp.headers_mut().insert(ERROR_TYPE_HEADER, error_type.to_string());
        }
        resp
    }

    /// Every shape a renderer can name is answered by its own name, so the value follows
    /// the model's error shapes rather than one recorded refusal.
    #[test]
    fn the_header_names_whatever_shape_the_renderer_named() {
        for shape in [
            "ResourceNotFoundException",
            "ValidationException",
            "ConflictException",
            "ThrottlingException",
            "InternalServerException",
        ] {
            let answered = with_error_type_shape_name(response(400, Some(shape)));
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                Some(shape),
                "case {shape:?}: expected {shape:?}, got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
        }
    }

    /// The shape is read out of every form a renderer can state it in, and whatever was
    /// written beside the name — a namespace, a locator, both — is not answered with.
    #[test]
    fn nothing_beside_the_shape_name_is_answered_with() {
        for (case, named) in [
            ("bare", "ResourceNotFoundException".to_string()),
            ("namespaced", format!("{NAMESPACE}#ResourceNotFoundException")),
            (
                "a locator appended",
                format!("ResourceNotFoundException:http://internal.example.test/{NAMESPACE}/"),
            ),
            (
                "namespaced with a locator appended",
                format!(
                    "{NAMESPACE}#ResourceNotFoundException:http://internal.example.test/\
                     {NAMESPACE}/"
                ),
            ),
            ("padded", "  ResourceNotFoundException  ".to_string()),
        ] {
            let answered = with_error_type_shape_name(response(404, Some(&named)));
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                Some("ResourceNotFoundException"),
                "case {case:?}: expected the bare shape name, got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
        }
    }

    /// Nothing to name: a success, and a failure whose renderer named no shape (or an
    /// empty one), are left exactly as they were.
    #[test]
    fn a_response_that_names_no_shape_is_left_as_it_was() {
        for (case, status, named) in [
            ("a success", 200, Some("ResourceNotFoundException")),
            ("no header", 404, None),
            ("an empty header", 404, Some("")),
        ] {
            let answered = with_error_type_shape_name(response(status, named));
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                named,
                "case {case:?}: expected the header left as it was ({named:?}), got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                answered.status().as_u16(),
                status,
                "case {case:?}: the status must not move, got {}",
                answered.status().as_u16()
            );
        }
    }
}
