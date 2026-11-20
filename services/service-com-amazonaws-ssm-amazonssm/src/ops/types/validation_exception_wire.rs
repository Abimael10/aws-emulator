// SPDX-License-Identifier: CC0-1.0

//! `ValidationException`, and the wire key this service carries its explanatory text
//! under.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives an error
//! structure's `Schema` from the on-disk model's member list, and the JSON codecs key a
//! member by that member's NAME (the awsJson family ignores `@jsonName` entirely). The
//! on-disk model spells this shape's one text member `Message`
//! (`smithy_query(query="shape", shape="ValidationException")` reports
//! `{"Message": {"target": "String"}, "ReasonCode": {"target": "String"}}`), so the derive
//! puts `Message` on the wire — while Systems Manager answers a `ValidationException` with
//! its text under the lower-cased `message` key (`WIRE_MESSAGE_KEY` below).
//!
//! That is a property of this ONE shape rather than of the service: `ValidationException`
//! is the refusal this service's request-validation front end words, and that envelope
//! keys its text `message`, which is also the key the framework's own boundary refusal of
//! a broken modelled constraint already writes
//! (`smithy_server_runtime`'s pre-dispatch envelope) — so one logical refusal reaches a
//! client under one key whether a modelled constraint or a handler refused it. Every other
//! error shape of this service is keyed exactly as the model spells its member, which for
//! most of them is `Message` and for some (`InvalidInstancePropertyFilterValue$message`)
//! is already lower case; none of them is touched here.
//!
//! This module therefore emits for `ValidationException` the same items the derive emits —
//! one member schema per member (index = its position in the model's member list), the
//! `SCHEMA_VALIDATION_EXCEPTION` static each operation's generated error enum references by
//! name, `shape_name` (the wire `__type`, the model's shape name unchanged), `Display`,
//! `Error` and `SerializableStruct` — and differs from that output in nothing but the wire
//! key of the text member. It is the same arrangement this crate's `command_wire` makes for
//! a shape the derive cannot express.
//!
//! The member's absolute shape id keeps the model's own spelling, so the member stays
//! identifiable as the modelled one; only the name the codecs read is the wire key. The
//! status (400) and the `__type` still come from the model, through the generated error
//! enum.
//!
//! `crate::scaffolded`'s own `lowercase_message_member!` is NOT reused: it builds the one
//! member of a SYNTHETIC shape, fixed in the `smithy.framework` namespace so it cannot
//! collide with a modelled one — the opposite of what this modelled shape needs, which is
//! its own namespace, its own shape ids and a second member beside the text.

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};

/// The key this service's `ValidationException` body carries its explanatory text under.
///
/// Lower case, unlike the `Message` the model spells the member — see this module's
/// header. Both the member NAME (what the awsJson codecs write) and `@jsonName` (what
/// restJson1 prefers) are set from this one constant, so the key cannot differ per codec.
/// Module-private: the shape is what the rest of the crate reaches, and this name is too
/// general to put in `types`' glob.
const WIRE_MESSAGE_KEY: &str = "message";

/// This service's namespace, as every `ShapeId` of a modelled shape carries it.
const NAMESPACE: &str = "com.amazonaws.ssm";

/// The model's `Message` member, keyed on the wire as `WIRE_MESSAGE_KEY`.
///
/// The `ShapeId` is the model's own, spelling included, so the member stays identifiable as
/// the modelled one; the wire key is the member NAME and the `@jsonName`, which is what the
/// codecs read. `with_xml_name("Message")` is what the derive and the scaffold's synthetic
/// sibling both put on a member whose name is lower case, so the XML envelopes keep their
/// own casing — inert for this awsJson1_1 service, and kept so this static is what the
/// derive would emit but for the wire key.
static VALIDATION_EXCEPTION_M_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ssm#ValidationException$Message",
        NAMESPACE,
        "Message",
    ),
    ShapeType::String,
    WIRE_MESSAGE_KEY,
    0,
)
.with_xml_name("Message")
.with_json_name(WIRE_MESSAGE_KEY);

/// The model's `ReasonCode` member — "the reason code for the invalid request" — keyed as
/// the model spells it, like every member of this service's error shapes but the one
/// above.
static VALIDATION_EXCEPTION_M_REASON_CODE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ssm#ValidationException$ReasonCode",
        NAMESPACE,
        "ReasonCode",
    ),
    ShapeType::String,
    "ReasonCode",
    1,
);

/// The schema of the modelled `ValidationException`, named as the derive names it so the
/// generated per-operation error enums resolve it unchanged.
pub static SCHEMA_VALIDATION_EXCEPTION: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.ssm#ValidationException",
        NAMESPACE,
        "ValidationException",
    ),
    ShapeType::Structure,
    &[
        &VALIDATION_EXCEPTION_M_MESSAGE,
        &VALIDATION_EXCEPTION_M_REASON_CODE,
    ],
);

/// "The request isn't valid. Verify that you entered valid contents for the command and
/// try again."
#[derive(Debug, Clone, Default)]
pub struct ValidationException {
    pub message: Option<String>,
    pub reason_code: Option<String>,
}

impl ValidationException {
    /// The wire error code (`__type`): the model's shape name, unchanged.
    pub const fn shape_name() -> &'static str {
        "ValidationException"
    }
}

impl ::std::fmt::Display for ValidationException {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        write!(f, "ValidationException")?;
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

impl ::std::error::Error for ValidationException {}

impl SerializableStruct for ValidationException {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(message) = self.message.as_ref() {
            s.write_string(&VALIDATION_EXCEPTION_M_MESSAGE, message.as_str())?;
        }
        if let Some(reason_code) = self.reason_code.as_ref() {
            s.write_string(&VALIDATION_EXCEPTION_M_REASON_CODE, reason_code.as_str())?;
        }
        Ok(())
    }
}
