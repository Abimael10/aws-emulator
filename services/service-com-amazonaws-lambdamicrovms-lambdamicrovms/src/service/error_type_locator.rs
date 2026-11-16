// SPDX-License-Identifier: CC0-1.0

//! How this service NAMES, in the `x-amzn-errortype` response header, the error shape a
//! refused call is refused with.
//!
//! A `restJson1` client resolves the error shape from that header, and the framework's
//! renderers write the shape's bare name into it. What this service's wire carries is the
//! QUALIFIED form: the shape name, the separator, and then the locator of the namespace that
//! declares the shape, under the service stack's locator root:
//!
//! ```text
//! HTTP/1.1 404 Not Found
//! x-amzn-errortype: ResourceNotFoundException:http://internal.amazon.com/coral/com.amazonaws.lambda.microvms/
//!
//! {"__type":"ResourceNotFoundException","message":"MicroVMImage not found for MicroVMImage: …"}
//! ```
//!
//! The locator is not part of the shape's NAME — every SDK resolves the shape from the text
//! before the separator (and after a `#`, for a namespaced name), so nothing a client resolves
//! changes — but it is what this service's wire carries, and a client that reads or validates
//! the whole header value sees it. The body keeps naming the bare shape in `__type`, exactly as
//! the renderer wrote it, and every other part of a response — the status, the message, the
//! other headers — stays as the renderer wrote it. A success keeps every byte and names no
//! shape at all.
//!
//! The form itself, and the two wrappers that complete a response with it, are
//! [`service_common::error_type_locator`]'s; this module supplies the one fact that is this
//! service's own, [`SHAPE_NAMESPACE`].
//!
//! The header belongs to the RESPONSE, not to any modelled shape — no error shape of this
//! model declares it, so no handler can set it — so it is completed at the two layers every
//! response of this service passes through:
//!
//! * [`QualifyingErrorType`], the wrapper [`crate::service::EmulatedService`] hands the router
//!   from `Service::override_operation`, covers everything an operation answers: a success
//!   (untouched), a modelled error, and a constraint refusal raised while that operation was
//!   being served;
//! * [`ErrorTypeLocatorDefinition`], the definition wrapper `Service::definition` hands back,
//!   covers a response NO operation produced: a routing failure, a transport failure detected
//!   up front, and a pre-dispatch refusal.

pub use service_common::error_type_locator::with_error_type_locator;

/// The namespace that DECLARES this service's shapes on the wire — what its qualified
/// `x-amzn-errortype` values name, observed on its own recorded refusals.
///
/// Stated rather than derived: this is the service stack's own namespace, and it is NOT the
/// namespace of the Smithy model this crate is generated from
/// (`com.amazonaws.lambdamicrovms`, which `smithy_query(query="service_info")` reports as the
/// shape id `com.amazonaws.lambdamicrovms#LambdaMicrovms`), so there is nothing in the model
/// to read it off. Every failure of this service is qualified with it, whichever operation
/// answered and whichever layer rendered the failure.
pub const SHAPE_NAMESPACE: &str = "com.amazonaws.lambda.microvms";

/// The locator every failure of this service qualifies its error shape with: the separator,
/// the service stack's locator root, then [`SHAPE_NAMESPACE`].
#[must_use]
pub fn locator() -> String {
    service_common::error_type_locator::namespace_locator(SHAPE_NAMESPACE)
}

/// This service's generated definition under the shared wrapper that qualifies the error shape
/// of every framework-level response it renders.
pub type ErrorTypeLocatorDefinition<D> =
    service_common::error_type_locator::QualifiedErrorTypeDefinition<D>;

/// The one wrapper every operation of this service is served through, so each of their
/// responses passes through the same completion.
pub type QualifyingErrorType = service_common::error_type_locator::QualifiedErrorTypeOperation;
