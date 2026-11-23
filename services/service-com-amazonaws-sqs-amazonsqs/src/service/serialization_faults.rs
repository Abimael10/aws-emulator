// SPDX-License-Identifier: CC0-1.0

//! The answer to a request body this endpoint's JSON wire could not READ.
//!
//! # Two different faults, two different classes
//!
//! A request can be wrong in two unrelated ways, and a client recovers from them
//! differently:
//!
//! * the body DESERIALIZES and a value in it is one the operation does not accept — a
//!   parameter the call requires that is absent, a value outside a documented range, a name
//!   outside a published set. That is an input fault, answered under the parameter classes
//!   this service words in [`crate::scaffolded`] (`MissingParameter`,
//!   `InvalidParameterValue`) and reported per member;
//! * the body does not DESERIALIZE at all, because a value's JSON type is not one the
//!   member's modelled shape can hold — a number where the model declares a string, a
//!   structure where it declares a scalar, a scalar where it declares a list. Nothing was
//!   read, so there is no member to report and no value to echo: the fault is in the
//!   caller's serializer, never in the values it was asked to send, and it is never
//!   retryable.
//!
//! RECORDED: this endpoint answers the second class HTTP 400 naming
//! [`SERIALIZATION_EXCEPTION_SHAPE_ID`], with a body carrying exactly two members — the
//! class and the sentence stating the mismatch ([`MESSAGE_MEMBER`]) — and nothing else.
//! Mapping it onto the first class, as the framework's generic rendering does, tells the
//! caller to look at a value it never sent.
//!
//! # Where the translation happens
//!
//! The framework reads the body against the operation's input schema and renders a codec
//! fault OUTSIDE the per-operation sequence, from a code and a message alone (see
//! `smithy_server_runtime`'s pre-dispatch renderers), under its own generic validation
//! class and carrying the codec's own report of the mismatch. That report is what this
//! module reads: [`answering_a_deserialization_fault`] recognises the report's grammar —
//! the pair of JSON types the codec expected and found — and states the mismatch as this
//! endpoint states it.
//!
//! It runs where every error answer of this service passes through
//! ([`super::completing_the_envelope`]), so the class does not depend on which operation
//! was called or which member carried the value: every operation of this API is read off
//! its own schema by the one codec, so one reading of its report covers all of them.

/// The error SHAPE a wire deserialization fault names: the shape id of the request
/// framework in front of Amazon SQS, which is what the JSON envelope's `__type`
/// discriminator carries and what an SDK resolves the typed exception from.
///
/// It is that framework's and not this model's for the same reason
/// [`crate::scaffolded::MISSING_PARAMETER_SHAPE_ID`] is: the body never reached this
/// service's model, so no shape of that model classifies the failure.
pub const SERIALIZATION_EXCEPTION_SHAPE_ID: &str = "com.amazon.coral.service#SerializationException";

/// The member that answer carries its sentence in.
///
/// Spelled with a capital `M`, like the other classes this request framework raises
/// (`crate::scaffolded`'s `AccessDeniedException`) and unlike the `message` member the
/// awsJson codec writes for a modelled error of the service itself.
pub const MESSAGE_MEMBER: &str = "Message";

/// The status it carries: the caller's serializer wrote a body this API cannot read, which
/// is a fault in the request.
const DESERIALIZATION_FAULT_STATUS: u16 = 400;

/// The class the framework renders a codec fault under — a shape this service's model does
/// not declare at all, which is what makes an answer naming it a failure this module
/// recognises rather than one of this API's own refusals.
const FRAMEWORK_VALIDATION_CLASS: &str = "ValidationException";

/// The member the framework's renderer writes the codec's report in.
const FRAMEWORK_MESSAGE_MEMBER: &str = "message";

/// How the codec reports a type mismatch: the type it expected, then the one the body
/// held.
const REPORT_PREFIX: &str = "expected ";
const REPORT_SEPARATOR: &str = ", got ";

/// The JSON grammar terms the codec names a structure's or map's opening and a list's
/// opening by.
const STRUCTURE_OR_MAP_START: &str = "object start";
const LIST_START: &str = "array start";

/// What this endpoint says when a member's own JSON type is not the one its shape holds —
/// a value where the model declares a collection.
const UNEXPECTED_FIELD_TYPE: &str = "Unexpected field type";

/// What it says when a structure or a map begins where the shape holds a single value.
const STRUCTURE_WHERE_NOT_EXPECTED: &str = "Start of structure or map found where not expected";

/// `resp` as this endpoint answers a body its JSON wire could not read: the deserialization
/// class and the sentence stating the mismatch, as the whole document.
///
/// Every other answer passes through untouched — a success, a refusal of a value the body
/// did carry, and an answer whose body is not a JSON document (the AWS Query surface's XML
/// one).
pub(super) fn answering_a_deserialization_fault(
    mut resp: aws_smithy_runtime_api::http::Response,
) -> aws_smithy_runtime_api::http::Response {
    if resp.status().as_u16() != DESERIALIZATION_FAULT_STATUS {
        return resp;
    }
    // An error body is built in memory by its renderer, so its bytes are readable here; a
    // streamed body is not an error body and is left alone.
    let document = resp
        .body()
        .bytes()
        .and_then(|bytes| ::serde_json::from_slice::<::serde_json::Value>(bytes).ok());
    let Some(document) = document else {
        return resp;
    };
    if document
        .get(super::TYPE_MEMBER)
        .and_then(::serde_json::Value::as_str)
        .map(str::trim)
        != Some(FRAMEWORK_VALIDATION_CLASS)
    {
        return resp;
    }
    let Some(message) = document
        .get(FRAMEWORK_MESSAGE_MEMBER)
        .and_then(::serde_json::Value::as_str)
        .and_then(deserialization_fault_message)
    else {
        return resp;
    };
    let body = ::serde_json::json!({
        super::TYPE_MEMBER: SERIALIZATION_EXCEPTION_SHAPE_ID,
        MESSAGE_MEMBER: message,
    })
    .to_string();
    // Only a renderer that advertised a length needs it corrected; the framework's error
    // renderers set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = body.len().to_string();
        let _ = resp.headers_mut().try_insert("content-length", length);
    }
    *resp.body_mut() = ::aws_smithy_types::body::SdkBody::from(body);
    resp
}

/// The sentence this endpoint states the mismatch `report` describes in, or `None` when
/// `report` describes no type mismatch — a refusal of a value the body DID carry, which is
/// an input fault and keeps the class it was answered under.
///
/// Three sentences, each for one shape of the mismatch:
///
/// * the member's own JSON type is not the one its shape holds, so nothing of the value was
///   read at all: a list or a structure was expected and the body held something else;
/// * a structure or a map begins where the shape holds a single value;
/// * a scalar token cannot be converted into the scalar type the shape holds, which the
///   sentence names as the pair it is: the JSON token and the modelled type.
fn deserialization_fault_message(report: &str) -> Option<String> {
    let (expected, found) = type_mismatch(report)?;
    if expected == LIST_START || expected == STRUCTURE_OR_MAP_START {
        return Some(UNEXPECTED_FIELD_TYPE.to_string());
    }
    if found == STRUCTURE_OR_MAP_START {
        return Some(STRUCTURE_WHERE_NOT_EXPECTED.to_string());
    }
    Some(format!(
        "{} cannot be converted to {}",
        json_token(found)?,
        modelled_type(expected)?
    ))
}

/// The pair of JSON types a codec report names — expected, then found — or `None` for a
/// message that is not such a report.
fn type_mismatch(report: &str) -> Option<(&str, &str)> {
    report.strip_prefix(REPORT_PREFIX)?.split_once(REPORT_SEPARATOR)
}

/// The name this class names the JSON token `found` by, or `None` for a term it states no
/// conversion of.
///
/// The terms are the codec's own reading of the body
/// (`aws_smithy_schema_server::json`'s token vocabulary); the names are the ones the request
/// framework's own sentence carries.
fn json_token(found: &str) -> Option<&'static str> {
    Some(match found {
        "number" => "NUMBER_VALUE",
        "string" => "STRING_VALUE",
        "boolean" => "BOOLEAN_VALUE",
        "null" => "NULL_VALUE",
        _ => return None,
    })
}

/// The modelled type the member holds, named as its Smithy shape type, or `None` for a term
/// that names no simple shape.
///
/// `number` is this model's integer type: every numeric member of this service's shapes
/// targets `Integer` (`MaxNumberOfMessages`, `VisibilityTimeout`, `WaitTimeSeconds`,
/// `DelaySeconds`), so the sentence names the type the body would have had to carry.
fn modelled_type(expected: &str) -> Option<&'static str> {
    Some(match expected {
        "string" => "String",
        "number" => "Integer",
        "boolean" => "Boolean",
        "base64 string" => "Blob",
        "timestamp" => "Timestamp",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{deserialization_fault_message, STRUCTURE_WHERE_NOT_EXPECTED, UNEXPECTED_FIELD_TYPE};

    /// One row per shape of mismatch the codec can report, and one per message that is not
    /// a mismatch report at all — those keep the class they were answered under.
    #[test]
    fn each_shape_of_mismatch_is_stated_in_its_own_sentence() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "a number where a string is declared",
                "expected string, got number",
                Some("NUMBER_VALUE cannot be converted to String"),
            ),
            (
                "a structure where a string is declared",
                "expected string, got object start",
                Some(STRUCTURE_WHERE_NOT_EXPECTED),
            ),
            (
                "a scalar where a list is declared",
                "expected array start, got number",
                Some(UNEXPECTED_FIELD_TYPE),
            ),
            (
                "a structure where a list is declared",
                "expected array start, got object start",
                Some(UNEXPECTED_FIELD_TYPE),
            ),
            (
                "a scalar where a map is declared",
                "expected object start, got string",
                Some(UNEXPECTED_FIELD_TYPE),
            ),
            (
                "a string where a number is declared",
                "expected number, got string",
                Some("STRING_VALUE cannot be converted to Integer"),
            ),
            (
                "a number where a boolean is declared",
                "expected boolean, got number",
                Some("NUMBER_VALUE cannot be converted to Boolean"),
            ),
            (
                "a refusal of a value the body did carry",
                "1 validation error detected: Value at 'queueUrl' failed to satisfy \
                 constraint: Member must not be empty",
                None,
            ),
            (
                "a codec failure that is no type mismatch",
                "json parse error: expected value at line 1 column 1",
                None,
            ),
            (
                "a term this sentence states no conversion of",
                "expected string, got array start",
                None,
            ),
        ];
        for (case, report, expected) in rows {
            let actual = deserialization_fault_message(report);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
