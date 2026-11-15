// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a body it cannot READ into the modelled
//! input shape is refused.
//!
//! Two refusals happen before a handler runs, and they are not the same error class:
//!
//! * a value whose JSON class the modelled type of the member it is bound to cannot be read
//!   from — a string where the model declares a list, a list where it declares a string, a
//!   number where it declares a structure — never becomes a value at all. That is a
//!   DESERIALIZATION failure, which AWS answers with the protocol-level
//!   [`SERIALIZATION_EXCEPTION`] (HTTP 400) and a message stating the JSON form the member is
//!   read from ([`expected_message`]);
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers as `InvalidInputException` through
//!   [`crate::service::EmulatedService::override_refusal`].
//!
//! The framework renders both as `ValidationException` (see
//! `vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code), so a client branching on the error code to decide whether to fix its
//! serializer or fix a value takes the wrong branch — and the codec's own prose is not the
//! sentence AWS states, nor is it carried in the member a serialization refusal states its
//! reason in. The vendored tree is read-only from a service crate, so the separation is made
//! here, at the one seam a service has before dispatch
//! (`Service::override_operation`), exactly as
//! `services/service-com-amazonaws-directoryservicedata-directoryservicedata` does for its own
//! request layer.
//!
//! Nothing about the model is written down here: which members a body may carry, and what each
//! one is read from, come off the operation's own input schema
//! (`crate::service::input_schema`), and WHETHER a value can be read into the member it is
//! bound to is the workspace's one statement of that question
//! (`service_common::json_wire_values::reads_as`), so this layer cannot disagree with the
//! request layers of the other services that answer it. Only the WORDS are this service's.
//! Only the body's own TOP level is read, as the codec reads it: a member `Schema` in this
//! framework carries its target's kind but not its target's schema, so a value nested inside a
//! list, a map or a structure is left to the codec exactly as it is today.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use serde_json::Value;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

/// The error code a request body that cannot be deserialized is refused with.
///
/// It is the PROTOCOL's error, not one of this service's modelled shapes: the request never
/// became an input, so no operation ran to answer with its own error. The framework already
/// answers this code for an awsJson body it cannot process at all (`pre_dispatch.rs`'s
/// catch-all arm), which is the same refusal one unreadable member earns.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The status such a refusal is answered at: the caller sent something that cannot be read,
/// so it is a client error — the same 4xx every refusal of this service's own model carries.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// Whether this layer refuses `req`: its body carries a value the modelled member it is bound
/// to cannot be read from.
pub(super) fn refuses(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> bool {
    sentence(definition, req).is_some()
}

/// The sentence refusing `req`, when a value its body carries cannot be read into the modelled
/// type of the member it is bound to.
///
/// `None` is "nothing for THIS layer to refuse", which covers several outcomes deliberately:
/// the body reads cleanly; the body is not a JSON object, so no member of it can be named (the
/// framework's own rendering of an unreadable body stands); or the request names no operation
/// this service serves, which is the router's routing failure to answer.
fn sentence(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<String> {
    let key = definition.protocol().routing_key(req).ok()?;
    unreadable_member(super::input_schema(key)?, body_of(req))
}

/// The request's body bytes, or empty when the body is not buffered.
fn body_of(req: &RtRequest) -> &[u8] {
    req.body().bytes().unwrap_or_default()
}

/// The first member of `body` whose JSON class the modelled type of the member of `input` it is
/// bound to cannot be read from, as the sentence refusing it.
///
/// A key naming no member is ignored, exactly as the codec ignores it.
fn unreadable_member(input: &Schema, body: &[u8]) -> Option<String> {
    let Ok(Value::Object(members)) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    members
        .iter()
        .filter_map(|(key, held)| {
            Some((
                ::service_common::json_wire_values::member_by_wire_name(input, key)?,
                held,
            ))
        })
        .find_map(|(member, held)| {
            let form = read_from(member.shape_type())?;
            (!::service_common::json_wire_values::reads_as(member.shape_type(), held))
                .then(|| expected_message(form))
        })
}

/// What a deserialization refusal states: the JSON form the member it refused is read from,
/// and that the member may be null instead.
///
/// `Expected list or null` is the recorded sentence for a list member handed a value of
/// another class; the form is the only part of it that varies, and it is the form the MEMBER's
/// own modelled type is read from ([`read_from`]) rather than anything about the value sent —
/// so a caller is never told back what it submitted.
fn expected_message(form: &str) -> String {
    format!("Expected {form} or null")
}

/// The JSON form a member whose modelled type is `target` is read from, as a deserialization
/// refusal names it.
///
/// `None` for a `document`, the one modelled type every JSON value reads into — there is no
/// form it expects, so no value bound to it is unreadable.
fn read_from(target: ShapeType) -> Option<&'static str> {
    Some(match target {
        ShapeType::Document => return None,
        ShapeType::List => "list",
        ShapeType::Map => "map",
        ShapeType::Structure | ShapeType::Union => "structure",
        ShapeType::Boolean => "boolean",
        ShapeType::Blob => "blob",
        ShapeType::Timestamp => "timestamp",
        ShapeType::Byte
        | ShapeType::Short
        | ShapeType::Integer
        | ShapeType::Long
        | ShapeType::Float
        | ShapeType::Double
        | ShapeType::BigInteger
        | ShapeType::BigDecimal => "number",
        // Every other modelled type a member can carry is a string on the wire — an
        // enumerated type among them, which codegen types as a string member and the
        // constraint walk, not the codec, holds to its value set.
        _ => "string",
    })
}

/// The `Message` member of the serialization refusal's shape.
///
/// Spelled `Message`: this service speaks `awsJson1_1`, whose codec takes the wire key from the
/// member NAME, and the protocol-level serialization refusal carries its sentence under that
/// capitalized key — unlike the error shapes this service's own model declares, whose member is
/// the lowercase `message`. A client reading a serialization refusal finds nothing under any
/// other name.
static SERIALIZATION_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    "Message",
    0,
)
.with_xml_name("Message")
.with_json_name("Message");

/// Schema for the serialization refusal's shape.
///
/// In the `smithy.framework` namespace for the same reason as the scaffold's synthetic shapes
/// (`crate::scaffolded`): this shape is in no service's model, so it must not collide with a
/// modelled one. The wire `__type` is the code passed to the protocol's own error serializer,
/// not this id.
static SERIALIZATION_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#SerializationException",
        "smithy.framework",
        "SerializationException",
    ),
    ShapeType::Structure,
    &[&SERIALIZATION_MESSAGE],
);

/// The body of a serialization refusal: the one sentence it states.
#[derive(Debug, Clone)]
struct Unreadable {
    /// The sentence — [`expected_message`].
    message: String,
}

impl SerializableStruct for Unreadable {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&SERIALIZATION_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

/// The operation this service serves for a request whose body it cannot read into the modelled
/// input shape.
///
/// An [`ErasedOperation`] rather than a response built in `Service::override_operation`,
/// because that seam hands the router a BORROWED operation. It holds no state: the refusal is
/// re-derived from the same request the router passes back into [`ErasedOperation::run`], so
/// one value serves every such request.
pub(super) struct UnreadableBody;

/// The one value of [`UnreadableBody`] this service hands back.
pub(super) static UNREADABLE_BODY: UnreadableBody = UnreadableBody;

impl ErasedOperation for UnreadableBody {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        let definition = svc.definition();
        // The same decision `Service::override_operation` made, re-derived from the same
        // request: this operation is reached only when it answered `Some`, so the refusal is
        // found again. Were it somehow not, the request is served exactly as the generated
        // routing table would serve it rather than refused in words nothing decided.
        let Some(message) = sentence(definition, req) else {
            return match definition
                .protocol()
                .routing_key(req)
                .ok()
                .and_then(|key| definition.routing_table().get(key))
            {
                Some(op) => op.run(svc, req, ctx),
                None => Box::pin(std::future::ready(
                    definition.routing_error_response(req),
                )),
            };
        };
        let refused = definition
            .protocol()
            .serialize_error(
                &Unreadable {
                    message: message.clone(),
                },
                &SERIALIZATION_SCHEMA,
                SERIALIZATION_EXCEPTION,
                SERIALIZATION_EXCEPTION_STATUS,
            )
            // The protocol writes one string member into its own envelope, so this cannot
            // fail; a codec that somehow could not is answered in the envelope the framework
            // renders a malformed request in, at the same status.
            .unwrap_or_else(|_| {
                definition.framework_error_response(
                    ::aws_smithy_schema_server::ProtocolError::Malformed(message),
                    req,
                )
            });
        Box::pin(std::future::ready(refused))
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so a request this
        // layer refuses is still authenticated exactly as the operation it addressed is: an
        // unreadable body is no reason to answer an unsigned caller.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a member is read from the JSON form its modelled type takes on the wire, and the
    /// refusal names that form.
    ///
    /// The rows span every modelled kind a member of this service's inputs can be — the
    /// collections, the structure, both scalar families and the catch-all string — and the
    /// `document`, which has no expected form because every value reads into it.
    #[test]
    fn every_modelled_kind_is_read_from_one_json_form() {
        // (the modelled type of the member, the form a refusal names it read from)
        let rows: [(ShapeType, Option<&str>); 10] = [
            (ShapeType::List, Some("list")),
            (ShapeType::Map, Some("map")),
            (ShapeType::Structure, Some("structure")),
            (ShapeType::Union, Some("structure")),
            (ShapeType::Boolean, Some("boolean")),
            (ShapeType::Integer, Some("number")),
            (ShapeType::Double, Some("number")),
            (ShapeType::String, Some("string")),
            (ShapeType::Timestamp, Some("timestamp")),
            (ShapeType::Document, None),
        ];
        for (target, form) in rows {
            assert_eq!(
                read_from(target),
                form,
                "case {target:?}: expected the form {form:?}, got {:?}",
                read_from(target)
            );
        }
        assert_eq!(
            expected_message("list"),
            "Expected list or null",
            "the recorded sentence states the form and that null is admitted"
        );
    }

    /// RULE: the member a refusal is about is the FIRST one of the body the modelled input
    /// cannot read, and a key the input declares no member for is ignored exactly as the codec
    /// ignores it.
    #[test]
    fn the_refusal_is_about_a_member_the_input_declares() {
        let input = crate::ops::types::BATCH_GET_PROJECTS_INPUT_SCHEMA;
        assert_eq!(
            unreadable_member(input, br#"{"names":"not-a-list"}"#).as_deref(),
            Some("Expected list or null"),
            "a string bound to the list member is unreadable"
        );
        assert_eq!(
            unreadable_member(input, br#"{"names":["a"]}"#),
            None,
            "a list bound to the list member reads"
        );
        assert_eq!(
            unreadable_member(input, br#"{"unmodelled":"a"}"#),
            None,
            "a key the input declares no member for is ignored"
        );
        assert_eq!(
            unreadable_member(input, b"[]"),
            None,
            "a body that is not a JSON object names no member"
        );
        assert_eq!(
            unreadable_member(input, b""),
            None,
            "an absent body names no member"
        );
    }
}
