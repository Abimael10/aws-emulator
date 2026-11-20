// SPDX-License-Identifier: CC0-1.0

//! The refusal this service answers when a well-formed request REFERENCES a resource that
//! does not exist.
//!
//! A request that addresses a resource by id in its URI is refused with the modelled
//! `NotFoundException` and a message naming the resource type and the id
//! (`crate::state::voice_connector_not_found` and its siblings). A request that merely
//! REFERENCES another resource
//! in its BODY — a Voice Connector group whose routing names the Voice Connectors inbound
//! calls go to — is refused differently on the wire: the same HTTP 404 and the same
//! `NotFound` code, but with the FIXED sentence below (it carries no value of the request)
//! and with the identifier it could not resolve carried in a `Resource` member beside the
//! modelled pair.
//!
//! That member is why this payload is not the modelled shape: `NotFoundException` declares
//! `Code` and `Message` and nothing else (`smithy_query(query="shape",
//! shape="NotFoundException")`), so a handler returning it cannot state the resource at all.
//! The error channel's wrapper ([`crate::scaffolded::Scaffolded`]) is the one place an
//! operation can answer a shape its model does not declare, and this is such a shape: the
//! wire's `NotFoundException` with one member more than the model records. Everything else
//! about it is the modelled error's — the 404 status and the `NotFoundException` the
//! `x-amzn-errortype` header names — so a client resolving the error shape sees the error the
//! model declares.

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};

/// The sentence this refusal states. FIXED text: it names the class of failure and carries
/// no value of the request, which the `Resource` member below carries instead.
///
/// It is the sentence the wire answers a `CreateVoiceConnectorGroup` whose routing names a
/// Voice Connector the account does not hold with, recorded in the
/// `chime-sdk-voice-voice-connector-group-create-validation-errors` trace of the
/// `simparity-wire-aws-v1` corpus.
pub const UNRESOLVED_REFERENCE_MESSAGE: &str = "Unable to find the requested identifier";

/// The error SHAPE this refusal is answered under — the one the MODEL declares for a
/// resource that could not be found, so a client resolving the shape from
/// `x-amzn-errortype` resolves a modelled error and not a class of its own.
pub const UNRESOLVED_REFERENCE_SHAPE: &str = "NotFoundException";

/// The status it is answered with: `NotFoundException`'s own `@httpError`
/// (`smithy_query(query="shape", shape="NotFoundException")`).
pub const UNRESOLVED_REFERENCE_STATUS: u16 = 404;

/// The `Code` member: the `NotFound` value of the model's `ErrorCode` enum, exactly as the
/// modelled payload carries it.
static CODE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.chimesdkvoice#NotFoundException$Code",
        "com.amazonaws.chimesdkvoice",
        "Code",
    ),
    ShapeType::String,
    "Code",
    0,
);

/// The `Message` member, carrying [`UNRESOLVED_REFERENCE_MESSAGE`].
static MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.chimesdkvoice#NotFoundException$Message",
        "com.amazonaws.chimesdkvoice",
        "Message",
    ),
    ShapeType::String,
    "Message",
    1,
);

/// The `Resource` member: the identifier the request referenced that resolved to nothing.
/// Not in the model's `NotFoundException` — see the module docs.
static RESOURCE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.chimesdkvoice#NotFoundException$Resource",
        "com.amazonaws.chimesdkvoice",
        "Resource",
    ),
    ShapeType::String,
    "Resource",
    2,
);

/// Schema for the payload: the modelled `Code`/`Message` pair plus the `Resource` member the
/// wire carries.
pub static UNRESOLVED_REFERENCE_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.chimesdkvoice#NotFoundException",
        "com.amazonaws.chimesdkvoice",
        "NotFoundException",
    ),
    ShapeType::Structure,
    &[&CODE, &MESSAGE, &RESOURCE],
);

/// The payload of an unresolved reference: the not-found code, the fixed sentence, and the
/// identifier that resolved to nothing.
#[derive(Debug, Clone)]
pub struct UnresolvedReference {
    pub code: String,
    pub message: String,
    pub resource: String,
}

impl UnresolvedReference {
    /// The refusal for a request that referenced `identifier` and nothing holds it.
    pub fn new(identifier: impl Into<String>) -> Self {
        Self {
            code: crate::state::NOT_FOUND_CODE.to_string(),
            message: UNRESOLVED_REFERENCE_MESSAGE.to_string(),
            resource: identifier.into(),
        }
    }
}

impl ::std::fmt::Display for UnresolvedReference {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        write!(f, "NotFoundException: {} ({})", self.message, self.resource)
    }
}

impl ::std::error::Error for UnresolvedReference {}

impl SerializableStruct for UnresolvedReference {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&CODE, self.code.as_str())?;
        s.write_string(&MESSAGE, self.message.as_str())?;
        s.write_string(&RESOURCE, self.resource.as_str())?;
        Ok(())
    }
}
