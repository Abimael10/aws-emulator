// SPDX-License-Identifier: CC0-1.0

//! The endpoint's own JSON-RPC surface: what it answers a request it NAMES NO OPERATION
//! for, and what it answers a request body it could not READ.
//!
//! This endpoint is a JSON-RPC endpoint before it is an API. Two decisions are made in
//! front of every operation of the model, in this order, and neither is any operation's
//! business:
//!
//! * the MEDIA TYPE. The endpoint serves its API under the one media type its protocol
//!   publishes ([`PROTOCOL_MEDIA_TYPE`]). A request announcing any other — plain JSON,
//!   another version of the `awsJson` family, anything outside JSON — announces a protocol
//!   this endpoint does not read its documents in, so no operation of it is identified and
//!   the answer is the same unrouted envelope.
//! * the TARGET. An `awsJson1_1` request names its operation in the `x-amz-target` header,
//!   as `<the endpoint's target prefix>.<operation>` ([`TARGET_PREFIX`]). A target that
//!   does not name THIS endpoint's API in that form — no target at all, a target without
//!   the separator, or one whose prefix belongs to some other endpoint — is not routed at
//!   all, and is answered the request framework's own JSON-RPC envelope
//!   ([`unrouted_response`]): the refusal is reported INSIDE a `200` envelope rather than
//!   as a status, because no operation was identified to answer for. A target that DOES
//!   name this API and names an operation it does not publish was read and resolved
//!   nothing, which is a fault in the request: `400` [`UNKNOWN_OPERATION`] with no prose
//!   at all ([`unknown_operation_response`]), since no operation, member or value was
//!   read for a sentence to state.
//! * the BODY. A body the JSON wire could not read is a fault in the caller's SERIALIZER,
//!   never in the values it was asked to send: nothing became a value, so no modelled
//!   constraint on one was ever reached and no member can be reported. It is answered
//!   `400` [`SERIALIZATION_EXCEPTION`] — the class of the request framework in front of
//!   this service, not a shape of its model — with the mismatch stated, where this
//!   framework states one, under the capitalized [`MESSAGE_MEMBER`]
//!   ([`unreadable_body_response`]). That is a different class from the constraint
//!   refusal a body that WAS read earns, and a client branches on the two differently:
//!   one says fix the serializer, the other says fix a value.
//!
//! Both decisions are read from the request alone — its headers and its body's first
//! token — and neither reads or writes any stored state.
//!
//! # Where they are applied
//!
//! The framework renders a codec failure and an unresolvable routing key from its own
//! generic vocabulary (`vendor/smithy-server-runtime/src/pre_dispatch.rs`: every
//! `ProtocolError::Serde` takes the constraint-refusal class and the lowercase `message`
//! member, and an unresolved key takes a `501`), and the vendored tree is read-only from a
//! service crate — so this endpoint's own answers are stated here and applied at the two
//! seams the service has in front of dispatch:
//! [`super::EmulatedService::override_operation`] for a request this surface owns, and
//! `override_routing_error` for a target that resolved nothing.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_types::body::SdkBody;
use serde_json::Value;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, OperationContext, ServerHttpProtocol, Service,
};

/// The header an `awsJson1_1` request names its operation in.
const TARGET_HEADER: &str = "x-amz-target";

/// The separator between the endpoint's target prefix and the operation name in that
/// header.
const TARGET_SEPARATOR: char = '.';

/// The target prefix this endpoint publishes its API under: the local name of the service
/// shape the model declares (`com.amazonaws.firehose#Firehose_20150804`), which is what the
/// `awsJson` family builds `x-amz-target` from. A target carrying any other prefix names
/// some other endpoint's API, so no operation of THIS one is identified by it.
const TARGET_PREFIX: &str = "Firehose_20150804";

/// The transfer encoding this surface does not decode: a body declaring one is not the
/// document the codec will read, so its bytes are not read here at all.
const CONTENT_ENCODING_HEADER: &str = "content-encoding";

/// The media type this endpoint serves its API under: the one the `awsJson1_1` protocol
/// trait implies, stated once for the whole tree in the shared module. A request announcing
/// another media type announces a protocol this endpoint does not read a document in.
const PROTOCOL_MEDIA_TYPE: &str = ::service_common::response_media_type::AWS_JSON_1_1;

/// The header a response announces its media type in, and a request the media type of its
/// body.
const CONTENT_TYPE: &str = "content-type";

/// The media type the request framework's own JSON-RPC envelope is announced in: it is that
/// framework's document rather than a document of this API, so it carries the plain JSON
/// media type and not the `awsJson1_1` one this service's own answers carry.
const FRAMEWORK_MEDIA_TYPE: &str = "application/json";

/// The status the framework's JSON-RPC envelope is carried on: the envelope itself reports
/// that no operation was identified, so the status says only that the endpoint answered.
const UNROUTED_STATUS: u16 = 200;

/// The shape a request that identified no operation at all is reported as, inside that
/// envelope. It belongs to the request framework in front of this service — no Firehose
/// model declares it — which is why it is named in full here rather than serialized through
/// an error schema of this service's.
const UNROUTED_TYPE: &str = "com.amazon.coral.service#UnknownOperationException";

/// The envelope's members: the answer it carries, and the version of the JSON-RPC envelope
/// it is carried in.
const OUTPUT_MEMBER: &str = "Output";
/// See [`OUTPUT_MEMBER`].
const VERSION_MEMBER: &str = "Version";
/// The `__type` discriminator, as that envelope spells it.
const TYPE_MEMBER: &str = "__type";
/// The envelope version that framework answers in.
const ENVELOPE_VERSION: &str = "1.0";

/// The class this endpoint refuses a target it READ and could resolve no operation from.
/// Not a shape of this service's model either: the target named no operation, so there is
/// no operation whose modelled errors could classify it.
const UNKNOWN_OPERATION: &str = "UnknownOperationException";

/// The status that refusal carries: the request was read and names nothing this endpoint
/// serves, which is a fault in the request and a terminal one — a client must not retry a
/// target that can never resolve.
const UNKNOWN_OPERATION_STATUS: u16 = 400;

/// The class this endpoint refuses a body its JSON wire could not read with.
///
/// It is the PROTOCOL's error rather than one of this service's modelled shapes: the
/// request never became an input, so no operation ran to answer with an error of its own.
/// What it must NOT be is the constraint-refusal class, which is the answer to values that
/// WERE read.
const SERIALIZATION_EXCEPTION: &str = "SerializationException";

/// The member a protocol-level error of this endpoint carries its prose in: the capitalized
/// `Message`, unlike the lowercase `message` the modelled exceptions of this service
/// declare.
const MESSAGE_MEMBER: &str = "Message";

/// The status an unreadable body is refused with: the caller sent bytes no operation could
/// be given.
const SERIALIZATION_EXCEPTION_STATUS: u16 = 400;

/// What this request framework states when a structural JSON token stands where the
/// operation's input structure must begin.
///
/// The vocabulary is that framework's own, read off the refusals it is recorded answering:
/// a list opening where none is expected is stated here, and the same framework states the
/// structure opening of the same family ("Start of structure or map found where not
/// expected", `services/service-com-amazonaws-sqs-amazonsqs/src/service/
/// serialization_faults.rs`) — which cannot arise at the TOP level of a request, where a
/// structure is exactly what the input shape is. The sentence names the token CLASS and
/// never a value, so nothing of the request appears in it.
const START_OF_LIST: &str = "Start of list found where not expected";

/// What this endpoint's own surface owns for a request, when it owns one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unserved {
    /// The request identified no operation of this API at all: its target names none.
    Unrouted,
    /// The request identified an operation this endpoint serves, and its body is not one
    /// the JSON wire could read into that operation's input — carrying the sentence this
    /// framework states for the mismatch, where it states one.
    UnreadableBody(Option<&'static str>),
}

/// What this surface owns for `req`, or `None` when the request is one the API serves and
/// an operation of the model is to answer it.
///
/// `serves` is whether this endpoint publishes an operation of that NAME — the routing
/// table's answer, so this reader states no operation list of its own. A target that names
/// this API and an operation it does not publish is owned by neither this surface nor an
/// operation: it is the routing failure the service answers
/// [`unknown_operation_response`] with.
#[must_use]
pub fn unserved(req: &RtRequest, serves: impl Fn(&str) -> bool) -> Option<Unserved> {
    if announces_another_media_type(req) {
        return Some(Unserved::Unrouted);
    }
    let Some(operation) = targeted_operation(req) else {
        return Some(Unserved::Unrouted);
    };
    if !serves(operation) {
        return None;
    }
    unreadable_body(req)
}

/// Whether `req` announces a media type this endpoint does NOT serve its API under.
///
/// A request announcing NONE is read in the protocol this endpoint publishes, exactly as
/// the framework reads it: the announcement is what this rule is about, and a request that
/// makes none has announced no other protocol.
#[must_use]
pub fn announces_another_media_type(req: &RtRequest) -> bool {
    announced_media_type(req).is_some_and(|media_type| media_type != PROTOCOL_MEDIA_TYPE)
}

/// The media type `req` announces its body under: the `content-type` header with its
/// parameters (`; charset=…`) dropped and its case folded, which is how a media type is
/// compared. `None` when the request announces none at all.
fn announced_media_type(req: &RtRequest) -> Option<String> {
    let announced = req.headers().get(CONTENT_TYPE)?;
    let media_type = announced.split(';').next().unwrap_or(announced).trim();
    (!media_type.is_empty()).then(|| media_type.to_ascii_lowercase())
}

/// The operation name `req`'s target names for THIS endpoint's API, or `None` when the
/// target names no operation of it: a request carrying no target, a target without the
/// `<prefix>.<operation>` separator, or one whose prefix is another endpoint's.
///
/// Only the prefix and the separator are read here — whether the NAME is one this endpoint
/// publishes is the routing table's answer, not this function's.
#[must_use]
pub fn targeted_operation(req: &RtRequest) -> Option<&str> {
    let (prefix, operation) = req
        .headers()
        .get(TARGET_HEADER)?
        .split_once(TARGET_SEPARATOR)?;
    (prefix == TARGET_PREFIX).then_some(operation)
}

/// What this surface owns for a body that does not open the structure every input shape of
/// this API is, with the sentence this framework states for the token it opened with;
/// `None` when the body opens that structure, so the codec reads it as it stands.
///
/// A body that is not a JSON document AT ALL, and one opening with a token this framework
/// states no sentence for, are both unreadable with nothing to state: they are owned with
/// no sentence. An EMPTY body is not unreadable — the `awsJson` protocols read one as the
/// empty structure — and a body in a transfer encoding this surface does not decode is not
/// read here at all.
#[must_use]
pub fn unreadable_body(req: &RtRequest) -> Option<Unserved> {
    if req.headers().get(CONTENT_ENCODING_HEADER).is_some() {
        return None;
    }
    let body = req.body().bytes()?;
    if body.is_empty() {
        return None;
    }
    match ::serde_json::from_slice::<Value>(body) {
        // The document opens the structure every input shape of this API is.
        Ok(Value::Object(_)) => None,
        Ok(opening) => Some(Unserved::UnreadableBody(stated_opening(&opening))),
        // Bytes that are no JSON document: no token was read, so none is named.
        Err(_) => Some(Unserved::UnreadableBody(None)),
    }
}

/// The sentence this framework states for the token class a body OPENS with where the input
/// structure must open instead; `None` for a class it states no sentence for, which is
/// answered under the same class with no prose rather than with a sentence invented here.
fn stated_opening(opening: &Value) -> Option<&'static str> {
    match opening {
        Value::Array(_) => Some(START_OF_LIST),
        _ => None,
    }
}

/// The framework's JSON-RPC envelope for a request that identified no operation: the
/// envelope reports the unknown operation as its own answer, so every byte of it is fixed
/// text and nothing of the request appears in it.
#[must_use]
pub fn unrouted_response() -> RtResponse {
    let body = ::serde_json::json!({
        OUTPUT_MEMBER: { TYPE_MEMBER: UNROUTED_TYPE },
        VERSION_MEMBER: ENVELOPE_VERSION,
    })
    .to_string();
    let mut resp = RtResponse::new(
        StatusCode::try_from(UNROUTED_STATUS).expect("the status is a constant of this module"),
        SdkBody::from(body),
    );
    resp.headers_mut().insert(CONTENT_TYPE, FRAMEWORK_MEDIA_TYPE);
    resp
}

/// What this endpoint refuses a target that named this API and resolved no operation of it:
/// the class alone, with no prose member at all, in this service's own JSON envelope.
///
/// `None` when the envelope could not be encoded, which leaves the framework's own
/// rendering to answer.
#[must_use]
pub fn unknown_operation_response(protocol: &dyn ServerHttpProtocol) -> Option<RtResponse> {
    stated_response(
        protocol,
        UNKNOWN_OPERATION,
        UNKNOWN_OPERATION_STATUS,
        None,
    )
}

/// What this endpoint answers a body its JSON wire could not read: the serialization class,
/// carrying the sentence under the capitalized member when this framework states one for
/// the mismatch, and no member at all when it does not.
#[must_use]
pub fn unreadable_body_response(
    protocol: &dyn ServerHttpProtocol,
    message: Option<&'static str>,
) -> Option<RtResponse> {
    stated_response(
        protocol,
        SERIALIZATION_EXCEPTION,
        SERIALIZATION_EXCEPTION_STATUS,
        message,
    )
}

/// One protocol-level refusal on the wire: `status` naming `class`, with `message` under
/// the capitalized member where there is one, encoded by `protocol` — which also writes the
/// `__type` discriminator and this service's own media type.
fn stated_response(
    protocol: &dyn ServerHttpProtocol,
    class: &str,
    status: u16,
    message: Option<&'static str>,
) -> Option<RtResponse> {
    protocol
        .serialize_error(
            &Stated {
                message: message.map(str::to_string),
            },
            &STATED_SCHEMA,
            class,
            status,
        )
        .ok()
}

/// The `Message` member of the synthetic shape the protocol-level classes above are stated
/// through.
static STATED_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#SerializationException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    MESSAGE_MEMBER,
    0,
);

/// The synthetic shape of a protocol-level refusal: no model declares it and no operation
/// lists it, because it is the request framework's own error rather than one of this
/// service's.
static STATED_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#SerializationException",
        "smithy.framework",
        "SerializationException",
    ),
    ShapeType::Structure,
    &[&STATED_MESSAGE],
);

/// The payload of a protocol-level refusal: the sentence it states, where it states one. A
/// class that interpolates nothing carries no member at all, so the whole body is the class
/// discriminator the protocol writes.
#[derive(Debug, Clone)]
struct Stated {
    message: Option<String>,
}

impl SerializableStruct for Stated {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        match self.message.as_deref() {
            Some(message) => serializer.write_string(&STATED_MESSAGE, message),
            None => Ok(()),
        }
    }
}

/// The endpoint's own surface, as the one [`ErasedOperation`] this service hands back for a
/// request no operation of it serves ([`super::EmulatedService::override_operation`]).
///
/// Stateless: every answer it gives is computed from the request's headers and its body's
/// first token, and nothing it is handed is read or written — no stream, no record, no
/// stored state at all.
#[derive(Debug, Clone, Copy)]
pub struct EndpointSurface;

impl ErasedOperation for EndpointSurface {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        let _ = ctx;
        Box::pin(async move {
            // The definition is read, used and dropped inside this one statement: it is not
            // `Sync`, so holding it across an await would make this future non-`Send`.
            let answer = {
                let definition = svc.definition();
                let owned = unserved(req, |operation| {
                    definition.routing_table().get(operation).is_some()
                });
                match owned {
                    Some(Unserved::UnreadableBody(message)) => unreadable_body_response(
                        definition.wire_protocol(req).protocol(),
                        message,
                    ),
                    // A request that identified no operation — and one this surface does
                    // not own at all, which dispatch never hands it — is the unrouted
                    // envelope.
                    _ => Some(unrouted_response()),
                }
            };
            answer.unwrap_or_else(unrouted_response)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so a request
        // that names one is signed exactly as that operation's model declares and this
        // surface cannot weaken any scheme.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request for the readers under test: the target it carries, if any, and its body.
    fn request(target: Option<&str>, body: &'static [u8]) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::from(body));
        req.set_method("POST").expect("a valid method");
        req.set_uri("/").expect("a valid uri");
        if let Some(target) = target {
            req.headers_mut().insert(TARGET_HEADER, target.to_string());
        }
        req
    }

    /// The same request announcing `media_type`.
    fn announcing(media_type: Option<&str>) -> RtRequest {
        let mut req = request(Some("Firehose_20150804.ListDeliveryStreams"), b"{}");
        if let Some(media_type) = media_type {
            req.headers_mut().insert(CONTENT_TYPE, media_type.to_string());
        }
        req
    }

    /// RULE: the endpoint reads a document only in the media type its own protocol
    /// publishes; a request announcing another one announces a protocol it does not read a
    /// document in, and a request announcing none has announced no other protocol.
    #[test]
    fn only_this_protocols_media_type_is_one_this_endpoint_reads() {
        // (case, the media type announced, whether it is another endpoint's protocol)
        let rows: &[(&str, Option<&str>, bool)] = &[
            (
                "this protocol's own media type",
                Some("application/x-amz-json-1.1"),
                false,
            ),
            (
                "it with a parameter, which is the same media type",
                Some("application/x-amz-json-1.1; charset=utf-8"),
                false,
            ),
            (
                "it in upper case, since a media type is case-insensitive",
                Some("Application/X-Amz-Json-1.1"),
                false,
            ),
            ("none announced at all", None, false),
            ("plain JSON, which is another protocol", Some("application/json"), true),
            (
                "another version of the awsJson family",
                Some("application/x-amz-json-1.0"),
                true,
            ),
            ("XML, which is no JSON at all", Some("application/xml"), true),
            ("plain text", Some("text/plain"), true),
        ];
        for (case, media_type, expected) in rows {
            let actual = announces_another_media_type(&announcing(*media_type));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a target names an operation of this API only as this endpoint's own prefix,
    /// the separator, and a name — and the name is whatever follows the FIRST separator.
    #[test]
    fn only_this_endpoints_prefix_names_an_operation_of_this_api() {
        // (case, the target the request carries, the operation it names)
        let rows: &[(&str, Option<&str>, Option<&str>)] = &[
            (
                "this endpoint's prefix and an operation it publishes",
                Some("Firehose_20150804.ListDeliveryStreams"),
                Some("ListDeliveryStreams"),
            ),
            (
                "this endpoint's prefix and a name it does not publish",
                Some("Firehose_20150804.DescribeDeliveryStreams"),
                Some("DescribeDeliveryStreams"),
            ),
            (
                "another endpoint's prefix",
                Some("Kinesis_20131202.ListDeliveryStreams"),
                None,
            ),
            (
                "this prefix at another API version",
                Some("Firehose_20150805.ListDeliveryStreams"),
                None,
            ),
            (
                "this prefix in another case, which is another prefix",
                Some("firehose_20150804.ListDeliveryStreams"),
                None,
            ),
            (
                "no prefix and no separator",
                Some("ListDeliveryStreams"),
                None,
            ),
            ("an empty prefix", Some(".ListDeliveryStreams"), None),
            ("no target at all", None, None),
        ];
        for (case, target, expected) in rows {
            let request = request(*target, b"{}");
            let actual = targeted_operation(&request);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a body is unreadable unless it opens the structure every input shape of this
    /// API is; an empty body is that structure, and a sentence is stated only for a token
    /// class this framework words one for.
    #[test]
    fn a_body_is_unreadable_unless_it_opens_the_input_structure() {
        // (case, the body's bytes, what this surface owns for it)
        let rows: &[(&str, &'static [u8], Option<Unserved>)] = &[
            ("an object, which every input shape is", b"{}", None),
            (
                "an object carrying members",
                br#"{"DeliveryStreamName":"s"}"#,
                None,
            ),
            ("no body at all, read as the empty structure", b"", None),
            (
                "a list where the structure must open",
                b"[]",
                Some(Unserved::UnreadableBody(Some(START_OF_LIST))),
            ),
            (
                "a list carrying structures",
                br#"[{"DeliveryStreamName":"s"}]"#,
                Some(Unserved::UnreadableBody(Some(START_OF_LIST))),
            ),
            (
                "a truncated document, which is no document",
                br#"{"DeliveryStreamName""#,
                Some(Unserved::UnreadableBody(None)),
            ),
            (
                "bytes that are no JSON at all",
                b"not json",
                Some(Unserved::UnreadableBody(None)),
            ),
            (
                "a string where the structure must open",
                br#""ListDeliveryStreams""#,
                Some(Unserved::UnreadableBody(None)),
            ),
            (
                "a number where the structure must open",
                b"42",
                Some(Unserved::UnreadableBody(None)),
            ),
            (
                "null where the structure must open",
                b"null",
                Some(Unserved::UnreadableBody(None)),
            ),
        ];
        for (case, body, expected) in rows {
            let request = request(Some("Firehose_20150804.ListDeliveryStreams"), body);
            let actual = unreadable_body(&request);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
