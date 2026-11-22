// SPDX-License-Identifier: CC0-1.0

//! How this service NAMES, in the `x-amzn-errortype` response header, the error shape a
//! refused call is refused with — and why that name carries nothing beside it.
//!
//! A `restJson1` client resolves the error shape from that header, and what this service's
//! wire carries is the shape's NAME and nothing else:
//!
//! ```text
//! HTTP/1.1 404 Not Found
//! x-amzn-errortype: NotFoundException
//!
//! {"__type":"NotFoundException","message":"Resource was not found"}
//! ```
//!
//! Nothing is appended to that name and nothing precedes it: not the namespace that declares
//! the shape, and not a locator for it inside whatever stack serves the request. A locator is
//! an internal detail of where a shape is declared — it is not part of the name a client
//! resolves, no trait of this model states it, and a response a client reads does not carry
//! it. The name the framework's `restJson1` renderers write is therefore exactly the value
//! this service answers with, and the body names that same shape in `__type`.
//!
//! WHICH shape is named is the failure's own: the modelled shape a handler returned — a
//! `ResourceNotFoundException` for a resource this service's own name-addressed operations
//! could not resolve — or, for the ARN-addressed tag operations, which the shared tagging
//! frontend serves, that frontend's own [`crate::state::TAG_PLANE_NOT_FOUND_CODE`], carried
//! through [`crate::scaffolded::Scaffolded::unmodeled`]. Both are derived from the failure, so
//! nothing about the value is written here.
//!
//! [`header_shape_named`] is what reads that shape back out of a header value, whichever form
//! a value arrives in; it is this module's one export, and the suite in
//! `tests/all/error_type_header_form.rs` asserts the form on the wire with it.

/// The bare error SHAPE a header value names: the text before any locator, and after any
/// namespace — exactly what an SDK resolves the exception class from.
pub use service_common::error_type_locator::header_shape_named;
