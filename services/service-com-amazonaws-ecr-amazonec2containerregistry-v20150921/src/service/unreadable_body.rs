// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a body whose TOP LEVEL is not a JSON
//! object is refused.
//!
//! Two refusals happen before a handler runs, and they are not the same error class:
//!
//! * a body the protocol cannot FRAME at all — one whose top level is a JSON array, string,
//!   number or flag where every operation of an `awsJson1_1` service takes a structure —
//!   never becomes an input: no member of it can even be named. That is a serialization
//!   failure, which AWS answers with the protocol-level [`SERIALIZATION_EXCEPTION`] (HTTP
//!   400) and a message naming the structural token the reader found where a structure had
//!   to open;
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers as `InvalidParameterException` through
//!   [`crate::service::EmulatedService::override_refusal`].
//!
//! The framework renders both as `ValidationException` (see
//! `vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code), so a client branching on the error code to decide whether to fix its
//! serializer or fix a value takes the wrong branch. The vendored tree is read-only from a
//! service crate, so the separation is made here, at the one seam a service has before
//! dispatch (`Service::override_operation`), exactly as
//! `services/service-com-amazonaws-codebuild-codebuild-20161006` does for its own request
//! layer.
//!
//! Only the body's own TOP LEVEL is read, and only when it is VALID JSON of another class:
//! a body that is not JSON at all, and a value nested inside the structure, are left to the
//! codec exactly as they are today. A body that reads as an object, and an absent body, are
//! this layer's business not at all.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use serde_json::Value;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

/// The error code a request body the protocol could not frame is refused with.
///
/// It is the PROTOCOL's error, not one of this service's modelled shapes: the request never
/// became an input, so no operation ran to answer with an error of its own. What it must NOT
/// be is this service's constraint-refusal code, which is the answer to a value that WAS
/// read.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The status such a refusal is answered at: the caller sent something that cannot be read,
/// so it is a client error — the same 400 every refusal of this service's own model carries
/// on this protocol.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// Whether this layer refuses `req`: it addresses an operation this service serves, and its
/// body is a JSON value of a class no request body can be.
pub(super) fn refuses(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> bool {
    sentence(definition, req).is_some()
}

/// The sentence refusing `req`, when its body's top level is a JSON value no request of this
/// protocol can be framed from.
///
/// `None` is "nothing for THIS layer to refuse", which covers several outcomes deliberately:
/// the body is a JSON object, so the codec reads it against the input schema; the body is
/// absent, or is not JSON at all, which the framework already renders its own way; or the
/// request names no operation this service serves, which is the router's routing failure to
/// answer.
fn sentence(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<String> {
    let key = definition.protocol().routing_key(req).ok()?;
    definition.routing_table().get(key)?;
    unframable_body(req.body().bytes().unwrap_or_default())
}

/// The sentence refusing `body`, when its top level is valid JSON of a class a request body
/// cannot be.
fn unframable_body(body: &[u8]) -> Option<String> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(_)) | Err(_) => None,
        Ok(value) => Some(unexpected_token_message(&value)),
    }
}

/// What a framing refusal states: the structural token the reader found where a request
/// body's structure had to open.
///
/// `Start of list found where not expected` is the recorded sentence for a body whose top
/// level is a JSON array; the token is the only part of it that varies, and it names the
/// CLASS of value the body carried ([`::service_common::json_wire_values::json_value_class`],
/// this tree's one vocabulary for that class) rather than anything the value held — so a
/// caller is never told its own payload back.
fn unexpected_token_message(value: &Value) -> String {
    match value {
        Value::Array(_) => "Start of list found where not expected".to_string(),
        other => format!(
            "{} found where not expected",
            ::service_common::json_wire_values::json_value_class(other)
        ),
    }
}

/// The `Message` member of the serialization refusal's shape.
///
/// Spelled `Message`: this service speaks `awsJson1_1`, whose codec takes the wire key from
/// the member NAME, and the protocol-level serialization refusal carries its sentence under
/// that capitalized key — unlike the error shapes this service's own model declares, whose
/// member is the lowercase `message`.
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
/// In the `smithy.framework` namespace for the same reason as the scaffold's synthetic
/// shapes (`crate::scaffolded`): this shape is in no service's model, so it must not collide
/// with a modelled one. The wire `__type` is the code passed to the protocol's own error
/// serializer, not this id.
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
struct Unframable {
    /// The sentence — [`unexpected_token_message`].
    message: String,
}

impl SerializableStruct for Unframable {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&SERIALIZATION_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

/// The operation this service serves for a request whose body the protocol cannot frame.
///
/// An [`ErasedOperation`] rather than a response built in `Service::override_operation`,
/// because that seam hands the router a BORROWED operation. It holds no state: the refusal
/// is re-derived from the same request the router passes back into [`ErasedOperation::run`],
/// so one value serves every such request.
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
                None => Box::pin(std::future::ready(definition.routing_error_response(req))),
            };
        };
        let refused = definition
            .protocol()
            .serialize_error(
                &Unframable {
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
        // `None` is "defer to the modelled operation the routing key names", so a request
        // this layer refuses is still authenticated exactly as the operation it addressed is:
        // an unreadable body is no reason to answer an unsigned caller.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a request body is a JSON structure; a top level of any other class is a framing
    /// failure naming the token found, and nothing else is this layer's to refuse.
    #[test]
    fn only_a_top_level_that_is_not_a_structure_is_refused() {
        // (the body, the sentence refusing it)
        let rows: [(&str, Option<&str>); 9] = [
            (r#"{"repositoryName":"a"}"#, None),
            ("{}", None),
            ("", None),
            ("{\"unclosed\":", None),
            ("[]", Some("Start of list found where not expected")),
            (
                r#"[{"repositoryName":"a"}]"#,
                Some("Start of list found where not expected"),
            ),
            ("\"text\"", Some("STRING_VALUE found where not expected")),
            ("7", Some("NUMBER_VALUE found where not expected")),
            ("true", Some("BOOLEAN_VALUE found where not expected")),
        ];
        for (body, expected) in rows {
            let refused = unframable_body(body.as_bytes());
            assert_eq!(
                refused.as_deref(),
                expected,
                "case {body:?}: expected {expected:?}, got {refused:?}"
            );
        }
    }
}
