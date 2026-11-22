// SPDX-License-Identifier: CC0-1.0

//! The error CHANNEL the three `/tags/{resourceArn}` operations answer through, and the
//! class the resource-tagging layer in front of this service refuses a resource reference
//! it cannot act on in.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` share ONE route
//! (`/tags/{resourceArn}`, by method) and are answered by that layer rather than by this
//! service's own resource operations. The refusal it answers a reference it cannot act on
//! with — one outside the modelled `RsiArn` constraint, and one inside it that resolves to
//! no resource this endpoint holds — carries an error code this model declares NOWHERE
//! (`smithy_query(query="operation",operation="TagResource,UntagResource,\
//! ListTagsForResource")` lists exactly `InternalServerException`,
//! `ResourceNotFoundException` and `ValidationException`), so it cannot be a variant of an
//! operation's generated error enum — and a client branches on the code, so answering it
//! under one of the three modelled shapes would send a caller into the wrong `catch`.
//!
//! The refusal ITSELF — its code, its status and the `smithy.framework` shape it is written
//! through — is the shared [`::service_common::tagging_refusal`]'s, because the same layer
//! fronts several services and two of them must not drift; what this service states is the
//! one fixed sentence that layer words the refusal with for the EMR surface
//! ([`super::INVALID_RESOURCE_ARN_MESSAGE`]), and the channel [`TaggingError`], which names
//! this crate's own scaffold wrapper and so cannot live in a shared crate.

use ::aws_smithy_schema::serde::SerializableStruct;
use ::aws_smithy_schema::Schema;
use ::service_common::tagging_refusal::{TaggingRefusal, TAG_BAD_REQUEST};
use ::smithy_server_runtime::OperationError;

use crate::scaffolded::Scaffolded;

/// The code that refusal carries, for the ONE place this service's pre-handler seam needs
/// it as a plain value ([`super::resource_arn_refusal`]): a `resourceArn` the modelled
/// `RsiArn` constraint itself rejects is refused by the same layer, in the same class,
/// before any handler runs.
pub use ::service_common::tagging_refusal::TAG_BAD_REQUEST_ERROR_CODE as UNUSABLE_RESOURCE_ARN_CODE;

/// The error channel of a tagging operation: two channels, not one.
///
/// [`Self::Scaffolded`] is the usual one — the operation's own modelled errors, plus the
/// refusal the scaffold channel words — and [`Self::FrontDoor`] is the tagging layer's own
/// refusal, which this model declares on no shape. Generic over the operation's modelled
/// error enum, so all three tagging operations share one spelling of the second channel.
#[derive(Debug)]
pub enum TaggingError<E> {
    /// A modelled error, or a refusal the scaffold channel words.
    Scaffolded(Scaffolded<E>),
    /// The tagging layer's own refusal.
    FrontDoor(TaggingRefusal),
}

impl<E> TaggingError<E> {
    /// The layer's refusal of a `resourceArn` it cannot act on: one it resolved to no
    /// resource of this service, in the sentence it words that refusal with here.
    ///
    /// The class is the layer's bad-request one, which is the class whose sentence this is:
    /// the layer states an unusable resource reference as a bad request about the value it
    /// was handed rather than as a missing resource of the service behind it, and that
    /// distinction is a client's to branch on.
    #[must_use]
    pub fn unusable_resource_arn() -> Self {
        Self::FrontDoor(TaggingRefusal::worded(
            &TAG_BAD_REQUEST,
            super::INVALID_RESOURCE_ARN_MESSAGE,
        ))
    }
}

impl<E: ::std::fmt::Display> ::std::fmt::Display for TaggingError<E> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::Scaffolded(error) => write!(f, "{error}"),
            Self::FrontDoor(error) => write!(f, "{error}"),
        }
    }
}

impl<E: ::std::error::Error + 'static> ::std::error::Error for TaggingError<E> {
    fn source(&self) -> Option<&(dyn ::std::error::Error + 'static)> {
        match self {
            Self::Scaffolded(error) => error.source(),
            Self::FrontDoor(error) => error.source(),
        }
    }
}

/// Every method delegates to the channel the active variant belongs to, so a modelled error
/// renders exactly as it does for every other operation of this service and only the
/// layer's own variant answers the layer's code and status.
impl<E: OperationError> OperationError for TaggingError<E> {
    fn as_serializable(&self) -> &dyn SerializableStruct {
        match self {
            Self::Scaffolded(error) => error.as_serializable(),
            Self::FrontDoor(error) => error,
        }
    }

    fn variant_schema(&self) -> &'static Schema {
        match self {
            Self::Scaffolded(error) => error.variant_schema(),
            Self::FrontDoor(error) => error.schema(),
        }
    }

    fn shape_name(&self) -> &'static str {
        match self {
            Self::Scaffolded(error) => error.shape_name(),
            Self::FrontDoor(error) => error.error_code(),
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::Scaffolded(error) => error.http_status(),
            Self::FrontDoor(error) => error.http_status(),
        }
    }

    fn query_error_code(&self) -> Option<&'static str> {
        match self {
            Self::Scaffolded(error) => error.query_error_code(),
            // No `awsQueryError` trait to honour: the layer's shape is in no model, and
            // this service speaks restJson1 in any case.
            Self::FrontDoor(_) => None,
        }
    }
}

/// Lets a modelled failure, or a refusal the scaffold channel words, lift into this channel
/// so a handler keeps writing `?` and `.into()`.
impl<E> From<Scaffolded<E>> for TaggingError<E> {
    fn from(scaffolded: Scaffolded<E>) -> Self {
        Self::Scaffolded(scaffolded)
    }
}
