// SPDX-License-Identifier: CC0-1.0

//! The error CHANNEL a tagging operation of this service answers through: its own modelled
//! errors, or one of the resource-tagging layer's two refusals.
//!
//! The three tagging operations share ONE route (`/tags/{ResourceArn}`, by method) and are
//! answered by the resource-tagging layer in front of this service rather than by its own
//! resource operations. That layer refuses a `ResourceArn` it cannot parse, and an ARN it
//! resolves to no resource, with error codes this model declares nowhere
//! (`smithy_query(query="operation",operation="ListTagsForResource,TagResource,\
//! UntagResource")` lists `AccessDenied`, `InternalServer`, `ResourceNotFound`,
//! `Throttling` and `Validation` and nothing else), so neither refusal can be a variant of
//! an operation's generated error enum.
//!
//! The refusals THEMSELVES — their codes, statuses, fixed sentences and the
//! `smithy.framework` shapes they are written through — are the shared
//! [`::service_common::tagging_refusal`]: the same layer fronts several services, so two of
//! them must not drift in code, status or wording. What is left here is the one thing a
//! service chooses, because it names the service's own scaffold wrapper: the channel
//! [`TaggingError`], which every tagging operation of this crate uses as its `Error` type.

use ::aws_smithy_schema::serde::SerializableStruct;
use ::aws_smithy_schema::Schema;
use ::service_common::tagging_refusal::TaggingRefusal;
use ::smithy_server_runtime::OperationError;

use crate::scaffolded::Scaffolded;

/// The error channel of a tagging operation: two channels, not one.
///
/// [`Self::Scaffolded`] is the usual one — the operation's own modelled errors, plus the
/// `ValidationException` a handler needs for a refusal its model cannot express — and
/// [`Self::Tagging`] is the resource-tagging layer's own refusal, which the model declares
/// on no shape. Generic over the operation's modelled error enum, so all three tagging
/// operations share one spelling of the second channel.
#[derive(Debug)]
pub enum TaggingError<E> {
    /// A modelled error, or a refusal the scaffold channel words.
    Scaffolded(Scaffolded<E>),
    /// One of the resource-tagging layer's own refusals.
    Tagging(TaggingRefusal),
}

impl<E> TaggingError<E> {
    /// The tagging layer's refusal of an ARN naming no resource of this service.
    pub fn not_found() -> Self {
        Self::Tagging(TaggingRefusal::not_found())
    }

    /// The tagging layer's refusal of a `ResourceArn` it could not parse as an ARN, which
    /// it answers before any resource is looked up.
    pub fn malformed_resource_arn() -> Self {
        Self::Tagging(TaggingRefusal::malformed_resource_arn())
    }
}

impl<E: ::std::fmt::Display> ::std::fmt::Display for TaggingError<E> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::Scaffolded(error) => write!(f, "{error}"),
            Self::Tagging(error) => write!(f, "{error}"),
        }
    }
}

impl<E: ::std::error::Error + 'static> ::std::error::Error for TaggingError<E> {
    fn source(&self) -> Option<&(dyn ::std::error::Error + 'static)> {
        match self {
            Self::Scaffolded(error) => error.source(),
            Self::Tagging(error) => error.source(),
        }
    }
}

/// Every method delegates to the channel the active variant belongs to, so the scaffold
/// wrapper keeps rendering exactly as it does for every other operation and only the
/// tagging layer's own variant answers that layer's code and status.
impl<E: OperationError> OperationError for TaggingError<E> {
    fn as_serializable(&self) -> &dyn SerializableStruct {
        match self {
            Self::Scaffolded(error) => error.as_serializable(),
            Self::Tagging(error) => error,
        }
    }

    fn variant_schema(&self) -> &'static Schema {
        match self {
            Self::Scaffolded(error) => error.variant_schema(),
            Self::Tagging(error) => error.schema(),
        }
    }

    fn shape_name(&self) -> &'static str {
        match self {
            Self::Scaffolded(error) => error.shape_name(),
            Self::Tagging(error) => error.error_code(),
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::Scaffolded(error) => error.http_status(),
            Self::Tagging(error) => error.http_status(),
        }
    }

    fn query_error_code(&self) -> Option<&'static str> {
        match self {
            Self::Scaffolded(error) => error.query_error_code(),
            // No `awsQueryError` trait to honour: neither shape is in a model, and this
            // service speaks restJson1 in any case.
            Self::Tagging(_) => None,
        }
    }
}

/// Lets a refusal the scaffold channel words (`Scaffolded::validation`) lift into this
/// channel, so a handler keeps writing `?` and `.into()`.
impl<E> From<Scaffolded<E>> for TaggingError<E> {
    fn from(scaffolded: Scaffolded<E>) -> Self {
        Self::Scaffolded(scaffolded)
    }
}
