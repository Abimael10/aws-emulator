// SPDX-License-Identifier: CC0-1.0

//! The ERROR ENVELOPE a failure of this service is answered in on its `rpcv2Cbor` route: what
//! names the class, what the map carries beside that name, and the status it travels under.
//!
//! This service's model declares `rpcv2Cbor` beside `awsJson1_0` and `awsQuery`
//! (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`),
//! so a client may call every operation over
//! `POST /service/GraniteServiceVersion20100801/operation/<Operation>` with
//! `smithy-protocol: rpc-v2-cbor` — and a failure of such a call is answered with a CBOR map
//! whose form the API model does not express. What a client reads there is recorded behaviour,
//! and it is the same for every operation and every class:
//!
//! ```text
//! HTTP/1.1 400 Bad Request
//! content-type: application/cbor
//! smithy-protocol: rpc-v2-cbor
//!
//! {"__type": "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException"}
//! ```
//!
//! Three things about that answer are this module's, and none of them is anything one request
//! chooses:
//!
//! * the class is named by the FULLY QUALIFIED shape id of its exception, in the package that
//!   declares it — this service's own versioned package ([`MODELLED_FAULT_PACKAGE`]) for a
//!   fault its API declares, and the package of the framework in front of the service
//!   ([`super::query_compatible_errors`]) for the request-handling classes that framework
//!   answers, exactly as the `awsJson` route names those same classes. An SDK resolves a CBOR
//!   error to an exception class by matching this value, so a bare local name resolves to
//!   nothing and a client's `is not found` branch never fires;
//! * the map carries that name and, where the failure has one, its message — and NO other
//!   member of the exception shape. `ResourceNotFoundException` declares `ResourceType`,
//!   `ResourceId` and `Message`
//!   (`smithy_query(query="shape",shape="ResourceNotFoundException",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`),
//!   and a client reading the first two on this wire reads nothing;
//! * the status is the one the FAULT SIDE of the class earns rather than the per-shape
//!   `httpError` its model binds: [`CLIENT_FAULT_STATUS`] for a failure the caller caused,
//!   whatever that shape's own status is, and the status it arrived with for a failure the
//!   service caused. A 404 for a resource a request named is read by proxies and generic HTTP
//!   layers as a routing problem rather than as the service-level client error it is.
//!
//! WHY A WRAPPER, and not each refusal site: a failure on this route is rendered by three
//! different layers — a handler's modelled error and the constraint walk's refusal, both
//! inside the framework's per-request sequence, and a refusal this service words ahead of that
//! sequence — and all three are CBOR by the time anything can read them. Completing the
//! envelope once, on the way out, is what makes the three read alike;
//! [`TheCborErrorEnvelope`] is that one place, and it serves the very operation the generated
//! routing table names for the request, so the framework still owns the whole deserialize ->
//! validate -> handle -> serialize sequence.
//!
//! What is deliberately LEFT ALONE: every success, every answer in another protocol's
//! envelope, and the emulator's own synthetic signals — the scaffold's 501 and the
//! authorization layer's 403 belong to no class of this service's API, so they are answered
//! exactly as the layer that built them wrote them.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::{Schema, ShapeId, ShapeType};
use aws_smithy_schema_server::CborCodec;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::query_compatible_errors::{
    class_of, local_name, restated_body, ErrorClass, FIRST_FAILURE_STATUS,
    FIRST_SERVER_FAULT_STATUS, NOT_A_SERVICE_CLASS, TYPE_MEMBER,
};

/// The package a fault this service's own API declares is named in on this route.
///
/// It is the model's namespace followed by the version of the service shape with its dashes
/// written as underscores — `com.amazonaws.cloudwatch` and `2010-08-01` for
/// `com.amazonaws.cloudwatch#GraniteServiceVersion20100801`
/// (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`)
/// — which is a DIFFERENT package from the one the request-handling classes of the framework in
/// front of the service are named in. The local name is the model's and is taken from the
/// answer rather than restated here, so this file holds no second copy of the model's error
/// list.
///
/// It is also a different package from the one the `awsJson` route names this service's OWN
/// faults in (`super::query_compatible_errors`'s `CORAL_SERVICE_PACKAGE`): the two wires are
/// recorded naming a modelled fault differently, so these two constants are deliberately
/// separate and must not be unified. What the two wires DO share is the package of the
/// request-handling classes, which both read off the one class table.
const MODELLED_FAULT_PACKAGE: &str = "com.amazonaws.cloudwatch.v2010_08_01";

/// The status a failure the CALLER caused is answered with on this route, whatever `httpError`
/// its own shape binds.
const CLIENT_FAULT_STATUS: u16 = 400;

/// The media type this protocol labels every body it writes with, a failure's included: the
/// marker that an answer is in THIS envelope rather than in one of the other two this service
/// declares.
const CBOR_CONTENT_TYPE: &str = "application/cbor";

/// Whether `resp` was written in this protocol's envelope.
///
/// The media type alone decides, read as a media type — the part before any parameter,
/// case-insensitively — because that is what the header states and its case is not
/// significant.
fn is_a_cbor_answer(resp: &RtResponse) -> bool {
    resp.headers().get("content-type").is_some_and(|header| {
        header
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case(CBOR_CONTENT_TYPE)
    })
}

/// The `__type` member of the envelope, as the codec writes its key.
static TYPE_MEMBER_SCHEMA: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#CborError$__type",
        "smithy.framework",
        "__type",
    ),
    ShapeType::String,
    TYPE_MEMBER,
    0,
)
.with_json_name(TYPE_MEMBER);

/// The message member in the spelling this service's parameter faults declare it
/// (`InvalidParameterValueException$message`, `MissingRequiredParameterException$message`,
/// `LimitExceededFault$message`), which is also the one every refusal this service words
/// renders through (`crate::scaffolded::VALIDATION_EXCEPTION_SCHEMA`).
static LOWER_MESSAGE_MEMBER: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#CborError$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    "message",
    1,
)
.with_json_name("message");

/// The message member in the spelling this service's resource and fault shapes declare it
/// (`ResourceNotFoundException$Message`, `InternalServiceFault$Message`).
static UPPER_MESSAGE_MEMBER: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#CborError$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    "Message",
    1,
)
.with_json_name("Message");

/// The envelope shape itself: the discriminator and the message, which is every member a
/// failure on this route states ([module docs](self)).
static ENVELOPE_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static("smithy.framework#CborError", "smithy.framework", "CborError"),
    ShapeType::Structure,
    &[&TYPE_MEMBER_SCHEMA, &LOWER_MESSAGE_MEMBER],
);

/// Every spelling of the message member this service's error shapes declare, in the order the
/// envelope considers them.
const MESSAGE_MEMBERS: &[&Schema] = &[&LOWER_MESSAGE_MEMBER, &UPPER_MESSAGE_MEMBER];

/// The member schema `key` names the message under, or `None` when it names another member.
///
/// Keyed on the member NAME rather than on the shape the answer came from, because the
/// envelope is completed from the rendered answer: the spellings are the ones this service's
/// error shapes declare the sentence under, and the answer keeps the one its own shape wrote.
fn message_member(key: &str) -> Option<&'static Schema> {
    MESSAGE_MEMBERS
        .iter()
        .copied()
        .find(|member| member.member_name() == Some(key))
}

/// The TEXT entries of the CBOR map `body` carries, in the order the map carries them, or
/// `None` when it is no CBOR map at all.
///
/// Read with the maintained CBOR codec the framework's own codec is built on, because the
/// members of an ANSWERED error map are not known from a shape: the schema-driven reader can
/// only read a body against a shape it knows, and what the envelope must see is every member
/// the layer below wrote — including the ones it is about to drop. An item that is not text is
/// read PAST rather than stopping the read: a member of an error shape need not be text —
/// `DashboardInvalidInputError` declares a LIST of validation messages beside its sentence
/// (`smithy_query(query="shape",shape="DashboardInvalidInputError",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`)
/// — and the envelope states no member but the class and its sentence anyway.
///
/// `pub` because this is also how a client reads an answer of this route back, which the suites
/// that assert one do: one definition of "the entries a client decodes", rather than a second
/// reader beside this one.
pub fn error_entries(body: &[u8]) -> Option<Vec<(String, String)>> {
    let mut decoder = minicbor::Decoder::new(body);
    // A map of a declared length, or an indefinite one closed by a break item: an encoder is
    // free to write either, and the framework's own codec writes the second.
    let declared = decoder.map().ok()?;
    let mut entries = Vec::new();
    let mut read = 0_u64;
    loop {
        match declared {
            Some(count) if read == count => break,
            None if matches!(decoder.datatype().ok()?, minicbor::data::Type::Break) => {
                decoder.skip().ok()?;
                break;
            }
            _ => {}
        }
        let key = decoder.str().ok()?.to_string();
        if matches!(decoder.datatype().ok()?, minicbor::data::Type::String) {
            entries.push((key, decoder.str().ok()?.to_string()));
        } else {
            decoder.skip().ok()?;
        }
        read = read.saturating_add(1);
    }
    Some(entries)
}

/// The fully qualified shape id a failure naming `named` states on this route, where `class` is
/// the request-handling class that name belongs to, if any.
///
/// A class the framework in front of the service answers is stated in THAT framework's own
/// package, the one the `awsJson` route states it in too — one source for both wires, so the
/// same refusal cannot name two packages. Every other name is a fault this service's own API
/// declares, and is stated in this service's versioned package.
fn shape_id_of(named: &str, class: Option<&ErrorClass>) -> String {
    match class {
        Some(class) => class.shape_id.to_string(),
        None => format!("{MODELLED_FAULT_PACKAGE}#{named}"),
    }
}

/// The envelope as the codec writes it: the discriminator first, then the message where there
/// is one, and nothing else.
struct TheEnvelope<'a> {
    shape_id: &'a str,
    message: Option<(&'static Schema, &'a str)>,
}

impl SerializableStruct for TheEnvelope<'_> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        serializer.write_string(&TYPE_MEMBER_SCHEMA, self.shape_id)?;
        if let Some((member, message)) = self.message {
            serializer.write_string(member, message)?;
        }
        Ok(())
    }
}

/// The envelope bytes for `shape_id`, carrying `message` when the failure has one.
///
/// Written through the protocol's own codec, so the map is framed exactly as every other answer
/// of this route is; `None` when the codec could not write it, which leaves the answer as the
/// layer below rendered it.
fn envelope_body(shape_id: &str, message: Option<(&'static Schema, &str)>) -> Option<Vec<u8>> {
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(&ENVELOPE_SCHEMA, &TheEnvelope { shape_id, message })
        .ok()?;
    Some(serializer.finish())
}

/// The status a failure that arrived with `status` travels under, or `None` when it keeps the
/// one it has.
///
/// A failure the caller caused is one class of answer here whatever the `httpError` of its own
/// shape, and a failure the service caused keeps the status that says so.
fn answered_status(status: u16) -> Option<u16> {
    (FIRST_FAILURE_STATUS..FIRST_SERVER_FAULT_STATUS)
        .contains(&status)
        .then_some(CLIENT_FAULT_STATUS)
        .filter(|restated| *restated != status)
}

/// `resp` as this service answers a failure on its `rpcv2Cbor` route ([module docs](self)): the
/// class named in full, the map carrying that name and the message alone, and the status of the
/// fault side rather than of the shape.
///
/// A success names no class and is returned untouched, and so is an answer in another
/// protocol's envelope, an answer whose body is no envelope this service rendered, and a
/// failure whose name belongs to no class of this service's API.
pub fn completing_the_cbor_envelope(mut resp: RtResponse) -> RtResponse {
    if resp.status().as_u16() < FIRST_FAILURE_STATUS || !is_a_cbor_answer(&resp) {
        return resp;
    }
    // An error body is built in memory by its renderer, so the bytes are readable here; a
    // streamed body is not an error body and is left alone.
    let Some(entries) = resp.body().bytes().and_then(error_entries) else {
        return resp;
    };
    let Some(named) = entries
        .iter()
        .find(|(key, _)| key == TYPE_MEMBER)
        .map(|(_, value)| local_name(value).to_string())
    else {
        return resp;
    };
    if named.is_empty() || NOT_A_SERVICE_CLASS.contains(&named.as_str()) {
        return resp;
    }
    let shape_id = shape_id_of(&named, class_of(&named));
    let message = entries
        .iter()
        .find_map(|(key, value)| message_member(key).map(|member| (member, value.as_str())));
    let Some(body) = envelope_body(&shape_id, message) else {
        return resp;
    };
    restated_body(&mut resp, body);
    if let Some(restated) = answered_status(resp.status().as_u16()) {
        if let Ok(status) = StatusCode::try_from(restated) {
            *resp.status_mut() = status;
        }
    }
    resp
}

/// One operation, served so that whatever failure it answers on the `rpcv2Cbor` route travels
/// in the envelope that route answers one in ([`completing_the_cbor_envelope`]).
///
/// It re-implements nothing and makes no routing decision: the wrapped operation is the one
/// this service would have served anyway, and only the rendered answer is completed on the way
/// out. Every operation of this service is served through one of these, so a failure raised by
/// ANY layer passes through the one place that names its class the way this route names one.
#[derive(Debug)]
pub struct TheCborErrorEnvelope<O> {
    operation: O,
}

impl<O> TheCborErrorEnvelope<O> {
    /// Wrap the operation, already constructed from the shared engine.
    pub fn new(operation: O) -> Self {
        Self { operation }
    }
}

impl<O> ErasedOperation for TheCborErrorEnvelope<O>
where
    O: ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let answered = self.operation.run(svc, req, ctx).await;
            completing_the_cbor_envelope(answered)
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        completing_the_cbor_envelope, envelope_body, error_entries, CBOR_CONTENT_TYPE,
        ENVELOPE_SCHEMA, LOWER_MESSAGE_MEMBER, MODELLED_FAULT_PACKAGE, TYPE_MEMBER_SCHEMA,
        UPPER_MESSAGE_MEMBER,
    };
    use aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
    use aws_smithy_schema::codec::{Codec, FinishSerializer};
    use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
    use aws_smithy_schema::{Schema, ShapeId, ShapeType};
    use aws_smithy_schema_server::CborCodec;
    use aws_smithy_types::body::SdkBody;

    use crate::ops::types::{
        DashboardInvalidInputError, DashboardValidationMessage,
        SCHEMA_DASHBOARD_INVALID_INPUT_ERROR,
    };

    /// The two members a handler populates on a not-found beside the sentence
    /// (`ResourceNotFoundException$ResourceType`, `$ResourceId`), so a case can render the
    /// answer this service's handlers produce today.
    static RESOURCE_TYPE_MEMBER: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.cloudwatch#ResourceNotFoundException$ResourceType",
            "com.amazonaws.cloudwatch",
            "ResourceType",
        ),
        ShapeType::String,
        "ResourceType",
        0,
    );
    static RESOURCE_ID_MEMBER: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.cloudwatch#ResourceNotFoundException$ResourceId",
            "com.amazonaws.cloudwatch",
            "ResourceId",
        ),
        ShapeType::String,
        "ResourceId",
        1,
    );

    /// Every member a case below renders, by the key it is written under.
    fn member(key: &str) -> &'static Schema {
        [
            &TYPE_MEMBER_SCHEMA,
            &LOWER_MESSAGE_MEMBER,
            &UPPER_MESSAGE_MEMBER,
            &RESOURCE_TYPE_MEMBER,
            &RESOURCE_ID_MEMBER,
        ]
        .into_iter()
        .find(|member| member.member_name() == Some(key))
        .unwrap_or_else(|| panic!("no member schema for the key {key:?}"))
    }

    /// A map of text entries as a layer below the wrapper rendered it, written through the
    /// protocol's own codec.
    struct AsRendered<'a>(&'a [(&'a str, &'a str)]);

    impl SerializableStruct for AsRendered<'_> {
        fn serialize_members(
            &self,
            serializer: &mut dyn ShapeSerializer,
        ) -> Result<(), SerdeError> {
            for (key, value) in self.0 {
                serializer.write_string(member(key), value)?;
            }
            Ok(())
        }
    }

    /// The bytes of the CBOR map `members` spells out.
    fn rendered(members: &[(&str, &str)]) -> Vec<u8> {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(&ENVELOPE_SCHEMA, &AsRendered(members))
            .expect("a map of text entries serializes");
        serializer.finish()
    }

    /// One failure answer in this envelope, as a renderer below the wrapper built it.
    fn answered(status: u16, members: &[(&str, &str)]) -> RtResponse {
        let mut resp = RtResponse::new(
            StatusCode::try_from(status).expect("a status code"),
            SdkBody::from(rendered(members)),
        );
        resp.headers_mut().insert("content-type", CBOR_CONTENT_TYPE);
        resp
    }

    /// The entries of an answer, as a client decoding its body reads them.
    fn entries(resp: &RtResponse) -> Vec<(String, String)> {
        error_entries(resp.body().bytes().expect("an in-memory body")).expect("a CBOR error map")
    }

    /// `expected` as the pairs [`entries`] hands back.
    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// One row of the table below: the case, the map a layer below rendered, and the map a
    /// client then reads. Named so the row type stays simple enough to read.
    type EnvelopeCase = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
    );

    /// A class is named in full, in the package that declares it, whichever layer named it —
    /// and the map carries that name and the message alone.
    #[test]
    fn a_failure_names_its_class_in_full_and_carries_nothing_else() {
        // (case, the map a layer below rendered, the map a client then reads)
        let rows: &[EnvelopeCase] = &[
            (
                "a modelled not-found, with the members a handler populated",
                &[
                    ("__type", "ResourceNotFoundException"),
                    ("ResourceType", "AlarmMuteRule"),
                    ("ResourceId", "a-rule"),
                ],
                &[(
                    "__type",
                    "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException",
                )],
            ),
            (
                "another modelled fault, named in the same package",
                &[("__type", "LimitExceededFault")],
                &[(
                    "__type",
                    "com.amazonaws.cloudwatch.v2010_08_01#LimitExceededFault",
                )],
            ),
            (
                "a modelled fault carrying its sentence",
                &[
                    ("__type", "InvalidFormatFault"),
                    ("Message", "the state value is not one of the three"),
                ],
                &[
                    (
                        "__type",
                        "com.amazonaws.cloudwatch.v2010_08_01#InvalidFormatFault",
                    ),
                    ("Message", "the state value is not one of the three"),
                ],
            ),
            (
                "the framework's parameter-value fault, named in ITS package",
                &[
                    ("__type", "InvalidParameterValueException"),
                    (
                        "message",
                        "INVALID_MAX_RESULTS: MaxResults must be greater than 0.",
                    ),
                ],
                &[
                    (
                        "__type",
                        "com.amazon.coral.service#InvalidParameterValueException",
                    ),
                    (
                        "message",
                        "INVALID_MAX_RESULTS: MaxResults must be greater than 0.",
                    ),
                ],
            ),
            (
                "the request-validation class",
                &[("__type", "ValidationException"), ("message", "no")],
                &[
                    ("__type", "com.amazon.coral.validate#ValidationException"),
                    ("message", "no"),
                ],
            ),
            (
                "this service's own wording of that class",
                &[("__type", "ValidationError"), ("message", "no")],
                &[
                    ("__type", "com.amazon.coral.validate#ValidationException"),
                    ("message", "no"),
                ],
            ),
            (
                "an answer already naming its class in full",
                &[(
                    "__type",
                    "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException",
                )],
                &[(
                    "__type",
                    "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException",
                )],
            ),
        ];
        for (case, as_rendered, expected) in rows.iter().copied() {
            let completed = completing_the_cbor_envelope(answered(404, as_rendered));
            let actual = entries(&completed);
            let expected = pairs(expected);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the map {expected:?}, got {actual:?}"
            );
        }
    }

    /// The status is the fault side's, not the shape's: every failure the caller caused is one
    /// status here, a failure the service caused keeps its own, and an answer this envelope
    /// states no class for keeps the one it arrived with.
    #[test]
    fn the_status_is_the_one_the_fault_side_earns() {
        // (case, the status a layer below answered, the name it named, the status answered)
        let rows: &[(&str, u16, &str, u16)] = &[
            (
                "a modelled not-found, whose shape binds 404",
                404,
                "ResourceNotFoundException",
                400,
            ),
            (
                "a parameter fault, already 400",
                400,
                "LimitExceededFault",
                400,
            ),
            (
                "a conflict, whose shape binds 409",
                409,
                "ConflictException",
                400,
            ),
            ("a fault the service caused", 500, "InternalServiceFault", 500),
            ("the scaffold's own signal", 501, "NotImplemented", 501),
            ("the authorization layer's refusal", 403, "AccessDenied", 403),
        ];
        for (case, as_rendered, named, expected) in rows.iter().copied() {
            let completed =
                completing_the_cbor_envelope(answered(as_rendered, &[("__type", named)]));
            let actual = completed.status().as_u16();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected status {expected}, got {actual}"
            );
        }
    }

    /// An answer this module states no envelope for is returned byte for byte: a success, an
    /// answer in another protocol's envelope, a body that is no CBOR map, and a failure whose
    /// name belongs to no class of this service's API.
    #[test]
    fn an_answer_this_envelope_does_not_own_is_left_alone() {
        // (case, the status, the content type, the body bytes)
        let rows: &[(&str, u16, &str, &[u8])] = &[
            ("a success in this envelope", 200, CBOR_CONTENT_TYPE, &[0xa0]),
            (
                "a failure in the JSON envelope",
                404,
                "application/x-amz-json-1.0",
                br#"{"__type":"ResourceNotFoundException"}"#,
            ),
            (
                "a failure whose body is no CBOR map",
                400,
                CBOR_CONTENT_TYPE,
                b"not cbor at all",
            ),
        ];
        for (case, status, content_type, body) in rows.iter().copied() {
            let mut resp = RtResponse::new(
                StatusCode::try_from(status).expect("a status code"),
                SdkBody::from(body),
            );
            resp.headers_mut()
                .insert("content-type", content_type.to_string());
            let completed = completing_the_cbor_envelope(resp);
            let actual = completed.body().bytes().expect("an in-memory body").to_vec();
            assert_eq!(
                actual, body,
                "case {case:?}: expected the answer byte for byte, got {actual:?}"
            );
            assert_eq!(
                completed.status().as_u16(),
                status,
                "case {case:?}: the status of an answer this envelope does not own"
            );
        }
        // The emulator's own synthetic signals keep their body as well as their status.
        for named in ["NotImplemented", "AccessDenied"] {
            let members: &[(&str, &str)] = &[("__type", named), ("message", "no")];
            let completed = completing_the_cbor_envelope(answered(403, members));
            let actual = completed.body().bytes().expect("an in-memory body").to_vec();
            assert_eq!(
                actual,
                rendered(members),
                "case {named:?}: a name this service's API does not declare is left alone"
            );
        }
    }

    /// The envelope writer and the envelope reader are one round trip, in both spellings of the
    /// message member, and the discriminator is written first.
    #[test]
    fn the_envelope_round_trips_in_either_spelling_of_the_message() {
        // (case, the member schema the message is written under, the key a client reads)
        let rows: &[(&str, &'static Schema, &str)] = &[
            (
                "the parameter faults' spelling",
                &LOWER_MESSAGE_MEMBER,
                "message",
            ),
            (
                "the resource shapes' spelling",
                &UPPER_MESSAGE_MEMBER,
                "Message",
            ),
        ];
        let shape_id = format!("{MODELLED_FAULT_PACKAGE}#ResourceNotFoundException");
        for (case, member, key) in rows.iter().copied() {
            let body = envelope_body(&shape_id, Some((member, "why"))).expect("the envelope");
            let actual = error_entries(&body).expect("a CBOR error map");
            let expected = pairs(&[("__type", shape_id.as_str()), (key, "why")]);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the map {expected:?}, got {actual:?}"
            );
        }
        let alone = envelope_body(&shape_id, None).expect("the envelope");
        let actual = error_entries(&alone).expect("a CBOR error map");
        let expected = pairs(&[("__type", shape_id.as_str())]);
        assert_eq!(
            actual, expected,
            "case a failure with no sentence: expected the map {expected:?}, got {actual:?}"
        );
    }

    /// An error shape whose members are not all TEXT is completed like any other: the class in
    /// full, its sentence, and the member of another type dropped rather than the whole answer
    /// being left as it was.
    ///
    /// Rendered from the error shape ITSELF, through the same codec the framework serializes a
    /// modelled error with, so what the envelope is read out of is the body a handler raising
    /// this fault really produces: `DashboardInvalidInputError` carries a LIST of validation
    /// messages beside its sentence (`crate::ops::put_dashboard`).
    #[test]
    fn a_fault_carrying_a_member_that_is_not_text_is_completed_too() {
        let raised = DashboardInvalidInputError {
            message: Some("the dashboard body is invalid".to_string()),
            dashboard_validation_messages: Some(vec![DashboardValidationMessage {
                data_path: Some("/widgets/0/properties".to_string()),
                message: Some("Should have required property 'markdown'".to_string()),
            }]),
        };
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(&SCHEMA_DASHBOARD_INVALID_INPUT_ERROR, &AsRaised(&raised))
            .expect("the error shape serializes");
        let mut resp = RtResponse::new(
            StatusCode::try_from(400u16).expect("a status code"),
            SdkBody::from(serializer.finish()),
        );
        resp.headers_mut().insert("content-type", CBOR_CONTENT_TYPE);

        let completed = completing_the_cbor_envelope(resp);
        let actual = entries(&completed);
        let expected = pairs(&[
            (
                "__type",
                format!("{MODELLED_FAULT_PACKAGE}#DashboardInvalidInputError").as_str(),
            ),
            ("message", "the dashboard body is invalid"),
        ]);
        assert_eq!(
            actual, expected,
            "case a fault carrying a list member: expected the map {expected:?}, got {actual:?}"
        );
        assert_eq!(
            completed.status().as_u16(),
            400,
            "case a fault carrying a list member: the status of the fault side"
        );
    }

    /// The error shape as the framework renders it: its `__type` first, then the shape's own
    /// members, which is the body [`completing_the_cbor_envelope`] is handed.
    struct AsRaised<'a>(&'a DashboardInvalidInputError);

    impl SerializableStruct for AsRaised<'_> {
        fn serialize_members(
            &self,
            serializer: &mut dyn ShapeSerializer,
        ) -> Result<(), SerdeError> {
            serializer.write_string(&TYPE_MEMBER_SCHEMA, "DashboardInvalidInputError")?;
            SerializableStruct::serialize_members(self.0, serializer)
        }
    }
}
