// SPDX-License-Identifier: CC0-1.0

//! How Control Tower's TAG PLANE refuses a `resourceArn` it resolves to no resource, and
//! the error CHANNEL its three operations carry that refusal in.
//!
//! `ListTagsForResource`, `TagResource` and `UntagResource` share one route
//! (`/tags/{resourceArn}`, by method) and are answered by the resource-tagging plane in
//! front of this service rather than by its own resource operations. That plane names its
//! refusals with its OWN error vocabulary: an ARN it cannot read as an ARN is
//! `BadRequestException` with the plane's fixed sentence (worded at the door those
//! operations are served behind, see [`crate::service`]), and an ARN it parsed and resolved
//! to no resource is [`TAGGING_NOT_FOUND_ERROR_CODE`] with the fixed sentence
//! [`TAGGING_NOT_FOUND_MESSAGE`] — neither of which this service's model declares on any
//! shape, so neither can be a variant of an operation's generated error enum. Both come
//! from the shared [`::service_common::tagging_refusal`], because the same plane fronts
//! several services and two of them must not drift in the code a client branches on or in
//! the sentence it reads.
//!
//! What a refusal CARRIES is still this service's own: the body is the modelled
//! `ResourceNotFoundException` payload, written through that shape's schema and answered
//! with its `@httpError` status, so only the wire code and the sentence are the plane's.
//!
//! The resource operations keep the modelled spelling: `GetEnabledControl` and its
//! siblings answer `ResourceNotFoundException` for a resource they do not hold, which an
//! SDK unmarshals into a different exception class from this one, so a client can tell a
//! tag-plane miss from a resource read's miss.

use ::aws_smithy_schema::serde::SerializableStruct;
use ::aws_smithy_schema::Schema;
use ::smithy_server_runtime::OperationError;

use crate::ops::types::{ResourceNotFoundException, SCHEMA_RESOURCE_NOT_FOUND_EXCEPTION};
use crate::scaffolded::Scaffolded;

/// The wire error code (`__type`, and the `x-amzn-errortype` header restJson1 resolves an
/// error shape from) the tag plane names a resource it cannot find by.
///
/// The shorter `NotFoundException` rather than the model-declared
/// `ResourceNotFoundException`: an SDK builds its exception class from this string, so a
/// client branching on the plane's class never matches the modelled one.
pub const TAGGING_NOT_FOUND_ERROR_CODE: &str =
    ::service_common::tagging_refusal::TAG_NOT_FOUND_ERROR_CODE;

/// The sentence that refusal states: FIXED text, naming nothing of the request — not the
/// ARN the caller sent, not the kind of resource it addressed.
pub const TAGGING_NOT_FOUND_MESSAGE: &str =
    ::service_common::tagging_refusal::TAG_NOT_FOUND_MESSAGE;

/// A refusal the tag plane words itself, carrying the ONE mapping from the refusal to the
/// wire code, the HTTP status and the body it answers with.
///
/// The three operations are one plane, so they cannot word one condition three ways: an
/// ARN naming no resource is [`TAGGING_NOT_FOUND_ERROR_CODE`] carrying the modelled
/// `ResourceNotFoundException` payload and its 404. Each operation's error channel
/// ([`TaggingError`]) delegates every [`OperationError`] method here, so the mapping is
/// declared once instead of once per operation.
#[derive(Debug)]
pub enum TaggingRefusal {
    /// "That ARN names no resource of this service", worded with
    /// [`TAGGING_NOT_FOUND_MESSAGE`].
    NotFound(ResourceNotFoundException),
}

impl TaggingRefusal {
    /// The not-found refusal for a `resourceArn` naming no resource this service holds.
    pub fn not_found() -> Self {
        Self::NotFound(ResourceNotFoundException {
            message: TAGGING_NOT_FOUND_MESSAGE.to_string(),
        })
    }

    /// The body this refusal serializes: the modelled shape itself, so its own schema
    /// decides which members the answer carries.
    fn as_serializable(&self) -> &dyn SerializableStruct {
        match self {
            Self::NotFound(error) => error,
        }
    }

    /// The schema the answer is written against: the modelled shape's own.
    fn variant_schema(&self) -> &'static Schema {
        match self {
            Self::NotFound(_) => &SCHEMA_RESOURCE_NOT_FOUND_EXCEPTION,
        }
    }

    /// The code on the wire, which the framework reads from an operation's `shape_name`
    /// rather than from the schema — the one respect in which this refusal differs from
    /// the modelled shape it carries.
    fn error_code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => TAGGING_NOT_FOUND_ERROR_CODE,
        }
    }

    /// The HTTP status: the carried shape's own `@httpError`
    /// (`smithy_query(query="shape", shape="ResourceNotFoundException")` -> 404), so the
    /// code on the wire changes and the status does not.
    fn http_status(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
        }
    }

    /// The sentence the body states, for the `Display` rendering below.
    fn message(&self) -> &str {
        match self {
            Self::NotFound(error) => &error.message,
        }
    }
}

impl ::std::fmt::Display for TaggingRefusal {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        write!(f, "{}: {}", self.error_code(), self.message())
    }
}

impl ::std::error::Error for TaggingRefusal {}

/// The error channel of a tag-plane operation: the scaffold wrapper every other operation
/// of this crate carries, or a refusal the tag plane words itself ([`TaggingRefusal`]).
///
/// Generic over the operation's modelled error enum, so all three tag-plane operations
/// share one spelling of the second channel. The scaffold wrapper is kept so a tag-plane
/// handler still reaches the refusals it offers (`Scaffolded::validation`) and a modelled
/// error is rendered exactly as `#[operation_error]` renders it.
#[derive(Debug)]
pub enum TaggingError<E> {
    /// A modelled error, or a refusal the scaffold channel words.
    Scaffolded(Scaffolded<E>),
    /// A refusal the tag plane words itself: the code, the status and the carried body are
    /// [`TaggingRefusal`]'s.
    Refused(TaggingRefusal),
}

impl<E> TaggingError<E> {
    /// The tag plane's refusal of a `resourceArn` naming no resource this service holds.
    pub fn not_found() -> Self {
        Self::Refused(TaggingRefusal::not_found())
    }
}

impl<E: ::std::fmt::Display> ::std::fmt::Display for TaggingError<E> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::Scaffolded(error) => write!(f, "{error}"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
        }
    }
}

/// `source()` forwards to the wrapped error rather than taking the default `None`, so a
/// modelled error's own `source()` is preserved through the channel.
impl<E: ::std::error::Error + 'static> ::std::error::Error for TaggingError<E> {
    fn source(&self) -> Option<&(dyn ::std::error::Error + 'static)> {
        match self {
            Self::Scaffolded(error) => error.source(),
            Self::Refused(refusal) => refusal.source(),
        }
    }
}

/// Every method delegates to the channel the active variant belongs to, so the scaffold
/// wrapper keeps rendering exactly as it does for every other operation and only the tag
/// plane's own variant answers that plane's code.
impl<E: OperationError> OperationError for TaggingError<E> {
    fn as_serializable(&self) -> &dyn SerializableStruct {
        match self {
            Self::Scaffolded(error) => error.as_serializable(),
            Self::Refused(refusal) => refusal.as_serializable(),
        }
    }

    fn variant_schema(&self) -> &'static Schema {
        match self {
            Self::Scaffolded(error) => error.variant_schema(),
            Self::Refused(refusal) => refusal.variant_schema(),
        }
    }

    fn shape_name(&self) -> &'static str {
        match self {
            Self::Scaffolded(error) => error.shape_name(),
            Self::Refused(refusal) => refusal.error_code(),
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::Scaffolded(error) => error.http_status(),
            Self::Refused(refusal) => refusal.http_status(),
        }
    }

    fn query_error_code(&self) -> Option<&'static str> {
        match self {
            Self::Scaffolded(error) => error.query_error_code(),
            // No `awsQueryError` trait to honour: the plane's class is in no model, and
            // this service speaks restJson1 in any case.
            Self::Refused(_) => None,
        }
    }
}

/// Lets a refusal the scaffold channel words lift into this channel, so a handler keeps
/// writing `?` and `.into()`.
impl<E> From<Scaffolded<E>> for TaggingError<E> {
    fn from(scaffolded: Scaffolded<E>) -> Self {
        Self::Scaffolded(scaffolded)
    }
}
