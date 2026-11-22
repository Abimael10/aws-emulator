// SPDX-License-Identifier: CC0-1.0

//! The refusals the RESOURCE-TAGGING layer in front of a service answers with, which are
//! that layer's own error classes rather than any error the serving service's model
//! declares.
//!
//! Several AWS services share one tagging surface (`/tags/{ResourceArn}`, by method) that
//! is answered AHEAD of the service's own resource operations, and that layer refuses the
//! two things it can decide for itself with error codes no such model declares:
//!
//! * a `ResourceArn` it cannot PARSE as an ARN — [`TAG_BAD_REQUEST`], the code
//!   [`TAG_BAD_REQUEST_ERROR_CODE`] with the fixed sentence [`TAG_BAD_REQUEST_MESSAGE`],
//!   answered before any resource is looked up;
//! * an ARN it parsed and resolved to no resource — [`TAG_NOT_FOUND`], the code
//!   [`TAG_NOT_FOUND_ERROR_CODE`] with the fixed sentence [`TAG_NOT_FOUND_MESSAGE`].
//!
//! Each carries its sentence under the single lower-case `message` member, and each
//! sentence is FIXED text — the same whichever value was presented — so neither refusal
//! echoes a value of the request and neither carries the members a service's own
//! not-found error declares.
//!
//! Neither shape is in any model, so both are spelled here in the `smithy.framework`
//! namespace the scaffold reserves for a shape no model declares, and they live in this
//! shared crate for the reason the rest of this module tree does: two services answering
//! the same layer must not drift in code, status or wording. What a service still chooses
//! is its own error CHANNEL — the enum that carries either its modelled errors or one of
//! these refusals — because that enum names the service's own scaffold wrapper.

use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};

/// The sentence the tagging layer's not-found refusal carries: fixed text, naming nothing
/// of the request.
pub const TAG_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The wire error code that refusal is identified by (`__type`, and the `x-amzn-errortype`
/// header restJson1 resolves an error shape from). A client branches on this string, so it
/// is the tagging layer's own class name and not the serving service's
/// `ResourceNotFoundException`.
pub const TAG_NOT_FOUND_ERROR_CODE: &str = "NotFoundException";

/// The HTTP status that refusal carries: the layer states an ARN it cannot resolve as a bad
/// request about the value it was handed, not as a missing resource of the service behind
/// it.
pub const TAG_NOT_FOUND_STATUS: u16 = 400;

/// The sentence the tagging layer's refusal of an unparseable `ResourceArn` carries: fixed
/// text, naming neither the value, the member nor any expression.
pub const TAG_BAD_REQUEST_MESSAGE: &str = "Invalid input resource arn: bad syntax";

/// The wire error code that refusal is identified by. The tagging layer's own class again,
/// distinct from the not-found one above, so a client can tell "this string is not an ARN"
/// from "that ARN names nothing".
pub const TAG_BAD_REQUEST_ERROR_CODE: &str = "BadRequestException";

/// The HTTP status that refusal carries: a value the layer could not parse is a client
/// error.
pub const TAG_BAD_REQUEST_STATUS: u16 = 400;

/// The `message` member of the tagging layer's not-found shape — lower-case on the wire,
/// which is the name a client reads the sentence out of.
static NOT_FOUND_MESSAGE_MEMBER: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#NotFoundException$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    "message",
    0,
);

/// Schema for the tagging layer's not-found shape.
///
/// Deliberately in the `smithy.framework` namespace: the shape is in no service's model, so
/// it must not collide with a modelled one. The wire error code comes from the serving
/// operation's error channel, not from this schema.
pub static TAG_NOT_FOUND_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#NotFoundException",
        "smithy.framework",
        "NotFoundException",
    ),
    ShapeType::Structure,
    &[&NOT_FOUND_MESSAGE_MEMBER],
);

/// The `message` member of the tagging layer's bad-request shape — lower-case on the wire
/// for the same reason as the not-found one.
static BAD_REQUEST_MESSAGE_MEMBER: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#BadRequestException$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    "message",
    0,
);

/// Schema for the tagging layer's bad-request shape, in the `smithy.framework` namespace
/// for the same reason.
pub static TAG_BAD_REQUEST_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#BadRequestException",
        "smithy.framework",
        "BadRequestException",
    ),
    ShapeType::Structure,
    &[&BAD_REQUEST_MESSAGE_MEMBER],
);

/// One refusal class of the resource-tagging layer: everything that distinguishes it on the
/// wire, in one place, so the classes differ only in these values and share every line of
/// rendering below.
pub struct TaggingRefusalClass {
    /// The wire error code a client branches on.
    pub error_code: &'static str,
    /// The HTTP status the refusal carries.
    pub http_status: u16,
    /// The fixed sentence, which is the whole of what the body says.
    pub message: &'static str,
    /// The shape the body is written through.
    pub schema: &'static Schema,
    /// That shape's single lower-case `message` member.
    pub message_member: &'static Schema,
}

/// The tagging layer's refusal of an ARN it resolved to no resource.
pub static TAG_NOT_FOUND: TaggingRefusalClass = TaggingRefusalClass {
    error_code: TAG_NOT_FOUND_ERROR_CODE,
    http_status: TAG_NOT_FOUND_STATUS,
    message: TAG_NOT_FOUND_MESSAGE,
    schema: &TAG_NOT_FOUND_SCHEMA,
    message_member: &NOT_FOUND_MESSAGE_MEMBER,
};

/// The tagging layer's refusal of a `ResourceArn` it could not parse as an ARN.
pub static TAG_BAD_REQUEST: TaggingRefusalClass = TaggingRefusalClass {
    error_code: TAG_BAD_REQUEST_ERROR_CODE,
    http_status: TAG_BAD_REQUEST_STATUS,
    message: TAG_BAD_REQUEST_MESSAGE,
    schema: &TAG_BAD_REQUEST_SCHEMA,
    message_member: &BAD_REQUEST_MESSAGE_MEMBER,
};

/// The payload of one of those refusals: the class it belongs to, and the one fixed
/// sentence that class states, as its single member.
#[derive(Clone)]
pub struct TaggingRefusal {
    /// Which of the layer's classes this is.
    class: &'static TaggingRefusalClass,
    /// The sentence, always the class's own [`TaggingRefusalClass::message`].
    message: String,
}

impl TaggingRefusal {
    /// The refusal of `class`, as the tagging layer words it.
    #[must_use]
    pub fn new(class: &'static TaggingRefusalClass) -> Self {
        Self {
            class,
            message: class.message.to_string(),
        }
    }

    /// The refusal of `class` in the SENTENCE the front door that answered it states.
    ///
    /// The code, the status and the shape are the layer's — they are what a client branches
    /// on, and two services answering the layer must not differ in them — while a family of
    /// services fronted by one layer may word a class in its own fixed text. The sentence is
    /// still FIXED: it is a `&'static str` of the caller's, so nothing of the request can be
    /// interpolated into it here either.
    #[must_use]
    pub fn worded(class: &'static TaggingRefusalClass, message: &'static str) -> Self {
        Self {
            class,
            message: message.to_string(),
        }
    }

    /// The refusal of an ARN naming no resource of the serving service.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(&TAG_NOT_FOUND)
    }

    /// The refusal of a `ResourceArn` the layer could not parse as an ARN, which it answers
    /// before any resource is looked up.
    #[must_use]
    pub fn malformed_resource_arn() -> Self {
        Self::new(&TAG_BAD_REQUEST)
    }

    /// The wire error code this refusal is identified by.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        self.class.error_code
    }

    /// The HTTP status it carries.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        self.class.http_status
    }

    /// The shape its body is written through.
    #[must_use]
    pub fn schema(&self) -> &'static Schema {
        self.class.schema
    }
}

impl std::fmt::Debug for TaggingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaggingRefusal")
            .field("error_code", &self.class.error_code)
            .field("http_status", &self.class.http_status)
            .field("message", &self.message)
            .finish()
    }
}

impl std::fmt::Display for TaggingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.class.error_code, self.message)
    }
}

impl std::error::Error for TaggingRefusal {}

impl SerializableStruct for TaggingRefusal {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        serializer.write_string(self.class.message_member, self.message.as_str())?;
        Ok(())
    }
}
