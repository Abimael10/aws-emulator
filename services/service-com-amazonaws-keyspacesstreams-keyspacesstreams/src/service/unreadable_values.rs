// SPDX-License-Identifier: CC0-1.0

//! This service's request-DESERIALIZATION layer: how a value the modelled member it is
//! bound to cannot be READ from is refused.
//!
//! Two refusals happen before a handler runs, and they are not the same error class:
//!
//! * a value whose JSON class the modelled type of its member cannot be read from — a
//!   string where the model declares an integer, a number where it declares a string, a
//!   list where it declares a structure — never becomes a value at all. That is a
//!   protocol-level SERIALIZATION failure, which this family of services answers with
//!   [`SERIALIZATION_EXCEPTION`] at [`SERIALIZATION_EXCEPTION_STATUS`], stating the two
//!   facts the caller has to reconcile: the CLASS of value it sent and the modelled type
//!   that value could not become ([`unconvertible_message`]);
//! * a value that WAS read and then broke a rule the model declares (an absent `@required`
//!   member, a `@length`/`@range`/`@pattern`/enum violation) is a CONSTRAINT refusal, which
//!   this service answers as its modelled `ValidationException`.
//!
//! The framework renders BOTH as `ValidationException` (see
//! `vendor/smithy-server-runtime/src/pre_dispatch.rs`, where every `ProtocolError::Serde`
//! takes that code), so a client branching on the error code to decide whether to fix its
//! serializer or fix a value takes the wrong branch — and the codec's own prose is neither
//! the sentence this family states nor carried in the member such a refusal states its
//! reason in (the capitalized `Message`). The vendored tree is read-only from a service
//! crate, so the separation is made here, at the one seam a service has before dispatch
//! ([`crate::service::EmulatedService::override_operation`]), as
//! `services/service-com-amazonaws-codebuild-codebuild-20161006` does for its own request
//! layer.
//!
//! Nothing about the model is written down here: which members a body may carry comes off
//! the operation's own input schema ([`input_schema`]), and WHETHER a value can be read into
//! the member it is bound to is the workspace's one statement of that question
//! (`service_common::json_wire_values::reads_as`), so this layer cannot disagree with the
//! request layers of the other services that answer it. Only the WORDS are this service's.
//! Only the body's own TOP level is read, as the codec reads it: a member `Schema` in this
//! framework carries its target's kind but not its target's schema, so a value nested inside
//! a structure, a list or a map is left to the codec exactly as it is today.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use serde_json::Value;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

/// The error code a request carrying a value that cannot be deserialized is refused with.
///
/// It is the PROTOCOL's error, not one of this service's modelled shapes: the value never
/// became a value, so no operation ran to answer with its own error. The framework already
/// answers this code for an `awsJson` body it cannot process at all (`pre_dispatch.rs`'s
/// catch-all arm), which is the same refusal one unreadable member earns.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The status such a refusal is answered at: the caller sent something that cannot be read,
/// so it is a client fault — the same 400 every client fault of this service takes
/// ([`crate::service::wire_status`]).
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// The sentence a failed conversion states: the CLASS of value the request sent and the
/// modelled type it could not be read as.
///
/// The CLASS, never the value: a caller is not told its own payload back, so a long value
/// cannot be amplified into a long response. The type is named by its Smithy shape type
/// (`Integer`, `String`, `Boolean`, …), with no article — the wording this family of
/// services uses, and one sentence shape for every member, so no member needs a case of its
/// own here.
fn unconvertible_message(class: &str, target: ShapeType) -> String {
    format!("{class} cannot be converted to {target:?}")
}

/// Whether this layer refuses `req`: its body carries a value the modelled member it is
/// bound to cannot be read from.
pub(super) fn refuses(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> bool {
    sentence(definition, req).is_some()
}

/// The sentence refusing `req`, when a value its body carries cannot be read into the
/// modelled type of the member it is bound to.
///
/// `None` is "nothing for THIS layer to refuse", which covers several outcomes
/// deliberately: the body reads cleanly; the body is not a JSON object, so no member of it
/// can be named (the framework's own rendering of an unreadable body stands); or the request
/// names no operation this service serves, which is the router's routing failure to answer.
fn sentence(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<String> {
    let key = definition.protocol().routing_key(req).ok()?;
    unreadable_member(input_schema(key)?, body_of(req))
}

/// The request's body bytes, or empty when the body is not buffered.
fn body_of(req: &RtRequest) -> &[u8] {
    req.body().bytes().unwrap_or_default()
}

/// The first member of `body` whose JSON class the modelled type of the member of `input` it
/// is bound to cannot be read from, as the sentence refusing it.
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
            (!::service_common::json_wire_values::reads_as(member.shape_type(), held)).then(|| {
                unconvertible_message(
                    ::service_common::json_wire_values::json_value_class(held),
                    member.shape_type(),
                )
            })
        })
}

/// Every operation's modelled input shape, as the generated types declare them.
///
/// Listed so [`input_schema`] can resolve one by the name a routing key carries; the
/// resolution reads the shape names off the schemas themselves, so nothing here restates
/// what the model says.
const INPUT_SCHEMAS: &[&Schema] = &[
    crate::ops::types::GET_RECORDS_INPUT_SCHEMA,
    crate::ops::types::GET_SHARD_ITERATOR_INPUT_SCHEMA,
    crate::ops::types::GET_STREAM_INPUT_SCHEMA,
    crate::ops::types::LIST_STREAMS_INPUT_SCHEMA,
];

/// How the model names the INPUT shape of an operation: the operation's own name with this
/// suffix, which is what [`input_schema`] resolves a routing key through.
const INPUT_SHAPE_SUFFIX: &str = "Input";

/// The input schema of the operation `operation` names, or `None` when this service serves
/// no operation by that name.
fn input_schema(operation: &str) -> Option<&'static Schema> {
    let shape = format!("{operation}{INPUT_SHAPE_SUFFIX}");
    INPUT_SCHEMAS
        .iter()
        .copied()
        .find(|input| input.shape_id().shape_name() == shape)
}

/// The `Message` member of the serialization refusal's shape.
///
/// Spelled `Message`: the `awsJson` codec takes the wire key from the member NAME, and this
/// protocol-level refusal carries its sentence under that capitalized key — unlike the error
/// shapes this service's own model declares, whose member is the lowercase `message`. A
/// client reading a serialization refusal finds nothing under any other name.
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
/// shapes ([`crate::scaffolded`]): this shape is in no service's model, so it must not
/// collide with a modelled one. The wire `__type` is the code passed to the protocol's own
/// error serializer, not this id.
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
    /// The sentence — [`unconvertible_message`].
    message: String,
}

impl SerializableStruct for Unreadable {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&SERIALIZATION_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

/// The operation this service serves for a request carrying a value it cannot read into the
/// modelled member that value is bound to.
///
/// An [`ErasedOperation`] rather than a response built in `Service::override_operation`,
/// because that seam hands the router a BORROWED operation. It holds no state: the refusal
/// is re-derived from the same request the router passes back into [`ErasedOperation::run`],
/// so one value serves every such request.
pub(super) struct UnreadableValues;

/// The one value of [`UnreadableValues`] this service hands back.
pub(super) static UNREADABLE_VALUES: UnreadableValues = UnreadableValues;

impl ErasedOperation for UnreadableValues {
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
        // `None` is "defer to the modelled operation the routing key names", so a request
        // this layer refuses is still authenticated exactly as the operation it addressed
        // is: an unreadable value is no reason to answer an unsigned caller.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a routing key resolves to the input shape the model gives that operation, and a
    /// name no operation of this service carries resolves to nothing.
    #[test]
    fn every_routing_key_resolves_to_its_modelled_input() {
        for operation in ["GetRecords", "GetShardIterator", "GetStream", "ListStreams"] {
            let resolved = input_schema(operation).map(|input| input.shape_id().shape_name());
            assert_eq!(
                resolved,
                Some(format!("{operation}{INPUT_SHAPE_SUFFIX}")).as_deref(),
                "case {operation:?}: expected the modelled input shape, got {resolved:?}"
            );
        }
        for unknown in ["GetRecordsInput", "NoSuchOperation", ""] {
            assert!(
                input_schema(unknown).is_none(),
                "case {unknown:?}: expected no input shape, got one"
            );
        }
    }

    /// Every input this layer reads a request against is reachable by the operation name its
    /// routing key carries: a model whose input shape is named otherwise would silently take
    /// this layer out of the path for that operation.
    #[test]
    fn every_input_is_reachable_by_its_operation_name() {
        for input in INPUT_SCHEMAS {
            let shape = input.shape_id().shape_name();
            let operation = shape
                .strip_suffix(INPUT_SHAPE_SUFFIX)
                .unwrap_or_else(|| panic!("case {shape}: expected an input shape name"));
            assert_eq!(
                input_schema(operation).map(|resolved| resolved.shape_id().shape_name()),
                Some(shape),
                "case {shape}: expected to resolve from {operation:?}"
            );
        }
    }

    /// RULE: the sentence names the class of value sent and the modelled type it could not
    /// be read as, and nothing of the value itself.
    #[test]
    fn the_sentence_states_the_class_and_the_modelled_type() {
        // (the class sent, the modelled type of the member, the sentence)
        let rows = [
            (
                "STRING_VALUE",
                ShapeType::Integer,
                "STRING_VALUE cannot be converted to Integer",
            ),
            (
                "NUMBER_VALUE",
                ShapeType::String,
                "NUMBER_VALUE cannot be converted to String",
            ),
            (
                "ARRAY_VALUE",
                ShapeType::Structure,
                "ARRAY_VALUE cannot be converted to Structure",
            ),
            (
                "BOOLEAN_VALUE",
                ShapeType::String,
                "BOOLEAN_VALUE cannot be converted to String",
            ),
        ];
        for (class, target, expected) in rows {
            let stated = unconvertible_message(class, target);
            assert_eq!(
                stated, expected,
                "case {class}/{target:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: the member a refusal is about is the FIRST one of the body the modelled input
    /// cannot read, and a key the input declares no member for is ignored exactly as the
    /// codec ignores it.
    #[test]
    fn the_refusal_is_about_a_member_the_input_declares() {
        let input = crate::ops::types::GET_RECORDS_INPUT_SCHEMA;
        assert_eq!(
            unreadable_member(input, br#"{"shardIterator":"a","maxResults":"7"}"#).as_deref(),
            Some("STRING_VALUE cannot be converted to Integer"),
            "a string bound to the integer member is unreadable"
        );
        assert_eq!(
            unreadable_member(input, br#"{"shardIterator":7}"#).as_deref(),
            Some("NUMBER_VALUE cannot be converted to String"),
            "a number bound to the string member is unreadable"
        );
        assert_eq!(
            unreadable_member(input, br#"{"shardIterator":"a","maxResults":7}"#),
            None,
            "values of the modelled classes read"
        );
        assert_eq!(
            unreadable_member(input, br#"{"shardIterator":"a","maxResults":null}"#),
            None,
            "a null is absence, which the constraint layer answers for"
        );
        assert_eq!(
            unreadable_member(input, br#"{"unmodelled":[]}"#),
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
